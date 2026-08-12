//! The `git` subprocess layer.
//!
//! Patina shells out to the `git` on `PATH` rather than linking a git library,
//! so a user's existing authentication (SSH agent, credential helper,
//! insteadOf rewrites, proxy config) applies untouched. `patina doctor`
//! reports when the binary is missing. See `docs/REMOTE_SOURCES.md` "The remote
//! cache".
//!
//! Every function here is a thin, typed wrapper over one `git` invocation. The
//! layer captures `stderr` into its errors and prints nothing itself:
//! user-facing output belongs to the CLI's reporter.

use camino::Utf8Path;
use std::process::Command;
use std::process::Output;

/// The git executable, resolved through `PATH` by the OS.
const GIT: &str = "git";

/// A full 40-hex-character commit SHA.
const SHA_LEN: usize = 40;

/// Failures from a `git` invocation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitError {
    /// `git` could not be spawned. Most often it is not installed or not on
    /// `PATH`.
    #[error("failed to run `git {args}`: {source}. Is git installed and on PATH?")]
    Spawn {
        /// The argument list, space-joined, for the failed invocation.
        args: String,
        /// The underlying spawn error.
        #[source]
        source: std::io::Error,
    },

    /// `git` ran and exited non-zero.
    #[error("`git {args}` failed{status}: {stderr}")]
    Failed {
        /// The argument list, space-joined.
        args: String,
        /// A rendered `" (exit N)"`, or the empty string when the process was
        /// signalled and reported no code.
        status: String,
        /// The captured `stderr`, trimmed.
        stderr: String,
    },

    /// `git` succeeded but its output did not match what the caller parses.
    #[error("`git {args}` produced output patina could not parse: {output}")]
    Unparseable {
        /// The argument list, space-joined.
        args: String,
        /// The captured `stdout`, trimmed.
        output: String,
    },

    /// A directory the cache layout requires could not be created.
    #[error("failed to create the remote cache directory {path}: {source}")]
    CacheDir {
        /// The directory that could not be created.
        path: camino::Utf8PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Whether a `git` binary resolves on `PATH`.
///
/// Read by `patina doctor` to raise its git-missing finding before a remote
/// operation fails deeper in.
#[must_use = "the availability of git decides whether remote operations can run"]
pub fn git_available() -> bool {
    crate::apply::resolve_on_path(GIT).is_some()
}

/// Run `git` with `args` and return its captured output, or a typed error when
/// the process could not start or exited non-zero.
///
/// `cwd` sets the working directory when the invocation is path-sensitive
/// (`checkout-index` writes relative to it).
fn run(args: &[&str], cwd: Option<&Utf8Path>) -> Result<Output, GitError> {
    let mut command = Command::new(GIT);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd.as_std_path());
    }
    // A prompt from a credential helper or from `ssh` would hang a command the
    // user may have launched from a shell hook. Fail fast instead.
    command.env("GIT_TERMINAL_PROMPT", "0");
    let output = command
        .output()
        .map_err(|source| spawn_error(args, source))?;
    if output.status.success() {
        return Ok(output);
    }
    Err(failed_error(args, &output))
}

/// The [`GitError::Spawn`] for an invocation the OS could not start.
fn spawn_error(args: &[&str], source: std::io::Error) -> GitError {
    GitError::Spawn {
        args: args.join(" "),
        source,
    }
}

/// The [`GitError::Failed`] for an invocation that ran and exited non-zero.
fn failed_error(args: &[&str], output: &Output) -> GitError {
    GitError::Failed {
        args: args.join(" "),
        status: output
            .status
            .code()
            .map_or_else(String::new, |code| format!(" (exit {code})")),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}

/// Trimmed `stdout` of a completed invocation.
fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Whether `candidate` is a full 40-character hexadecimal commit SHA. Shared by
/// the lockfile parser and the cache pruner, which key checkouts by this shape.
pub(crate) fn is_full_sha(candidate: &str) -> bool {
    candidate.len() == SHA_LEN && candidate.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Read the commit SHA that `git_ref` (or the remote's default branch when
/// `git_ref` is `None`) currently points at, without downloading any object.
///
/// This is the only network call `patina remote check` makes.
///
/// An annotated tag resolves to the commit it peels to, so a tag and a branch
/// naming the same commit produce the same SHA.
///
/// # Errors
///
/// Returns [`GitError::Spawn`] when `git` cannot start, [`GitError::Failed`]
/// when the remote is unreachable or rejects the request, and
/// [`GitError::Unparseable`] when the ref does not exist on the remote or the
/// output carries no SHA.
pub fn ls_remote(url: &str, git_ref: Option<&str>) -> Result<String, GitError> {
    let wanted = git_ref.unwrap_or("HEAD");
    let args = ["ls-remote", url, wanted];
    let output = run(&args, None)?;
    let text = stdout_of(&output);
    select_ls_remote_sha(&text, wanted).ok_or_else(|| GitError::Unparseable {
        args: args.join(" "),
        output: text,
    })
}

/// Pick the SHA for `wanted` out of `ls-remote` output.
///
/// `ls-remote <url> <name>` can answer with several lines: a branch and a tag
/// may share a name, and an annotated tag also reports its peeled commit as
/// `refs/tags/<name>^{}`. The preference order is peeled tag, then branch, then
/// plain tag, then the exact name as written (which covers `HEAD` and a
/// fully-qualified ref), so the returned SHA is always a commit and never
/// depends on git's output ordering.
///
/// Nothing else is accepted: `ls-remote` pattern-matches trailing path
/// components, so when the named ref is gone its output can still carry a
/// suffix-matching stranger like `refs/pull/42/main` for `main`. Selecting that
/// would silently pin a ref the user never named; refusing it surfaces as
/// [`GitError::Unparseable`], which names what the remote actually answered.
fn select_ls_remote_sha(text: &str, wanted: &str) -> Option<String> {
    let rows: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(sha, name)| (sha.trim(), name.trim()))
        .filter(|(sha, _)| is_full_sha(sha))
        .collect();
    let pick = |name: &str| {
        rows.iter()
            .find(|(_, candidate)| *candidate == name)
            .map(|(sha, _)| (*sha).to_owned())
    };
    pick(&format!("refs/tags/{wanted}^{{}}"))
        .or_else(|| pick(&format!("refs/heads/{wanted}")))
        .or_else(|| pick(&format!("refs/tags/{wanted}")))
        .or_else(|| pick(wanted))
}

/// Create the bare fetch repository at `git_dir` if it does not exist yet.
///
/// Idempotent: `git init --bare` over an existing repository reinitializes it
/// without touching objects.
///
/// # Errors
///
/// Returns [`GitError::CacheDir`] when the parent directory cannot be created,
/// or a `git` failure from the init itself.
pub fn ensure_bare_repo(git_dir: &Utf8Path) -> Result<(), GitError> {
    if let Some(parent) = git_dir.parent() {
        fs_err::create_dir_all(parent.as_std_path()).map_err(|source| GitError::CacheDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    run(&["init", "--quiet", "--bare", git_dir.as_str()], None)?;
    Ok(())
}

/// Whether `rev` is already present as a commit object in `git_dir`.
///
/// This is the cache-warm test: a warm cache lets a plain `apply` run fully
/// offline.
///
/// # Errors
///
/// Returns [`GitError::Spawn`] when `git` cannot start. A `git` exit signalling
/// "no such object" is not an error; it is reported as `false`.
pub fn has_commit(git_dir: &Utf8Path, rev: &str) -> Result<bool, GitError> {
    // `cat-file -e <rev>^{commit}` exits 0 when the object exists and is (or
    // peels to) a commit, and non-zero otherwise. Only a spawn failure is a
    // real error here.
    match run(
        &[
            "--git-dir",
            git_dir.as_str(),
            "cat-file",
            "-e",
            &format!("{rev}^{{commit}}"),
        ],
        None,
    ) {
        Ok(_) => Ok(true),
        Err(GitError::Failed { .. } | GitError::Unparseable { .. }) => Ok(false),
        Err(other) => Err(other),
    }
}

/// Fetch the single commit `rev` from `url` into the bare repository at
/// `git_dir`, shallow.
///
/// A depth-1 fetch of one exact SHA is the cheapest thing that can materialize
/// a pinned rev. Some servers refuse a SHA they were not asked to advertise
/// (`uploadpack.allowReachableSHA1InWant` off), so when `git_ref` is known and
/// the SHA fetch fails, the ref is fetched instead and `rev` is re-checked; the
/// pinned SHA still decides what gets checked out, so the fallback changes only
/// how the object arrives.
///
/// # Errors
///
/// Returns a `git` failure when the remote is unreachable or neither attempt
/// produces `rev`.
pub fn fetch_commit(
    git_dir: &Utf8Path,
    url: &str,
    rev: &str,
    git_ref: Option<&str>,
) -> Result<(), GitError> {
    ensure_bare_repo(git_dir)?;
    let by_sha = run(
        &[
            "--git-dir",
            git_dir.as_str(),
            "fetch",
            "--quiet",
            "--depth",
            "1",
            url,
            rev,
        ],
        None,
    );
    match by_sha {
        Ok(_) => return Ok(()),
        Err(err @ GitError::Spawn { .. }) => return Err(err),
        Err(err) => {
            let Some(git_ref) = git_ref else {
                return Err(err);
            };
            run(
                &[
                    "--git-dir",
                    git_dir.as_str(),
                    "fetch",
                    "--quiet",
                    "--depth",
                    "1",
                    url,
                    git_ref,
                ],
                None,
            )?;
            if !has_commit(git_dir, rev)? {
                return Err(err);
            }
        }
    }
    Ok(())
}

/// Fetch `git_ref` (or the remote's default branch) from `url` into `git_dir`
/// with full history.
///
/// The update gate's ancestry check asks whether the pinned rev is an ancestor
/// of the candidate tip, and `merge-base` can only answer that if the commits
/// between the two are present. A depth-1 fetch leaves the two as disconnected
/// shallow roots, where the question is unanswerable rather than merely false,
/// so the producer path (`patina remote update`) pays for real history while
/// the consumer path (`apply` filling a cold cache for an already-decided pin)
/// stays on [`fetch_commit`]'s shallow fetch.
///
/// # Errors
///
/// Returns a `git` failure when the remote is unreachable or does not have
/// `git_ref`.
pub fn fetch_history(git_dir: &Utf8Path, url: &str, git_ref: Option<&str>) -> Result<(), GitError> {
    ensure_bare_repo(git_dir)?;
    run(
        &[
            "--git-dir",
            git_dir.as_str(),
            "fetch",
            "--quiet",
            url,
            git_ref.unwrap_or("HEAD"),
        ],
        None,
    )?;
    Ok(())
}

/// Materialize the tree of `rev` into `dest`.
///
/// Uses `read-tree` + `checkout-index` rather than `checkout`: the plumbing
/// pair writes a work tree out of a bare repository without moving `HEAD` or
/// disturbing the repository's own index, which matters because one bare
/// repository backs every checkout of that remote. The scratch index lives
/// beside the checkout and is removed afterwards.
///
/// A checkout is a cache of the commit, so it must hold the commit's bytes on
/// every machine. Anything machine-local that could rewrite them on the way out
/// is switched off for the write: line-ending translation (`core.autocrlf`),
/// the per-user and system attribute files, and real symlinks. An in-tree
/// `.gitattributes` is committed content and identical everywhere, but can
/// still make a checkout diverge from the raw bytes; attribute-blind
/// materialization is a post-1.0 item (see `REMOTE_SOURCES.md`).
///
/// `core.symlinks=false` also denies a malicious remote a symlink the resolver
/// could follow out of the checkout, writing the target text as a regular file
/// instead.
///
/// # Errors
///
/// Returns [`GitError::CacheDir`] when `dest` cannot be created, or a `git`
/// failure when `rev` is absent from `git_dir` or the write fails.
pub fn checkout_commit(git_dir: &Utf8Path, rev: &str, dest: &Utf8Path) -> Result<(), GitError> {
    fs_err::create_dir_all(dest.as_std_path()).map_err(|source| GitError::CacheDir {
        path: dest.to_path_buf(),
        source,
    })?;
    // Appended rather than substituted for the extension: `dest` is a staging
    // directory whose name is unique per process, and the index must be too, or
    // two concurrent materializations would corrupt each other's checkout.
    let index = camino::Utf8PathBuf::from(format!("{dest}.index"));
    let git_dir_arg = git_dir.as_str();
    let work_tree_arg = format!("--work-tree={dest}");
    // Beats setting these in the bare repo's config: the override travels with
    // the invocation, so it cannot be lost if the cache repo is ever re-created.
    let verbatim = [
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.eol=lf",
        "-c",
        "core.safecrlf=false",
        "-c",
        "core.attributesFile=",
        "-c",
        "core.symlinks=false",
    ];
    let result = (|| -> Result<(), GitError> {
        let mut read_tree = vec!["--git-dir", git_dir_arg, &work_tree_arg];
        read_tree.extend_from_slice(&verbatim);
        read_tree.extend_from_slice(&["read-tree", rev]);
        run_with_index(&read_tree, dest, &index)?;

        let mut checkout = vec!["--git-dir", git_dir_arg, &work_tree_arg];
        checkout.extend_from_slice(&verbatim);
        checkout.extend_from_slice(&["checkout-index", "--all", "--force"]);
        run_with_index(&checkout, dest, &index)?;
        Ok(())
    })();
    // The scratch index is derivable state; drop it whether or not the checkout
    // succeeded so a retry starts clean.
    drop(fs_err::remove_file(index.as_std_path()));
    result
}

/// Run `git` with `args` under a scratch `GIT_INDEX_FILE`, from `cwd`.
///
/// `GIT_ATTR_NOSYSTEM` drops the system gitattributes file so a machine-local
/// attribute rule cannot rewrite the bytes a checkout materializes.
fn run_with_index(args: &[&str], cwd: &Utf8Path, index: &Utf8Path) -> Result<(), GitError> {
    let mut command = Command::new(GIT);
    command
        .args(args)
        .current_dir(cwd.as_std_path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_INDEX_FILE", index.as_str());
    let output = command
        .output()
        .map_err(|source| spawn_error(args, source))?;
    if output.status.success() {
        return Ok(());
    }
    Err(failed_error(args, &output))
}

/// Whether `ancestor` is an ancestor of `descendant` in `git_dir`.
///
/// The update gate's ancestry check: a candidate tip that is not a descendant
/// of the pinned rev means upstream history was rewritten.
///
/// # Errors
///
/// Returns [`GitError::Spawn`] when `git` cannot start, or [`GitError::Failed`]
/// when either revision is missing from `git_dir`. `git merge-base
/// --is-ancestor` reports a plain "no" as exit 1, which is a `false` answer
/// rather than a failure; any other non-zero code is surfaced.
pub fn is_ancestor(git_dir: &Utf8Path, ancestor: &str, descendant: &str) -> Result<bool, GitError> {
    let args = [
        "--git-dir",
        git_dir.as_str(),
        "merge-base",
        "--is-ancestor",
        ancestor,
        descendant,
    ];
    let mut command = Command::new(GIT);
    command.args(args).env("GIT_TERMINAL_PROMPT", "0");
    let output = command
        .output()
        .map_err(|source| spawn_error(&args, source))?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(failed_error(&args, &output))
}

/// The committer time of `rev`, as Unix seconds.
///
/// The gate's future, backdating, and age checks all read this one value. It is
/// the *committer* time, not the author time: a rebased or cherry-picked commit
/// keeps its original author date, so author time would let ordinary
/// maintenance trip the gate.
///
/// # Errors
///
/// Returns a `git` failure when `rev` is missing from `git_dir`, and
/// [`GitError::Unparseable`] when the output is not an integer.
pub fn committer_time(git_dir: &Utf8Path, rev: &str) -> Result<i64, GitError> {
    let args = [
        "--git-dir",
        git_dir.as_str(),
        "show",
        "--no-patch",
        // A user's `log.showSignature = true` would otherwise prepend signature
        // lines to stdout and break the integer parse for a signed commit.
        "--no-show-signature",
        "--format=%ct",
        rev,
    ];
    let output = run(&args, None)?;
    let text = stdout_of(&output);
    text.parse()
        .map_err(|_not_an_integer| GitError::Unparseable {
            args: args.join(" "),
            output: text,
        })
}

/// Resolve `rev` to the full commit SHA it names inside `git_dir`.
///
/// Used to normalize whatever the user or a lockfile wrote (an abbreviated SHA,
/// a tag) into the full form the cache directory is keyed by.
///
/// # Errors
///
/// Returns a `git` failure when `rev` does not resolve, and
/// [`GitError::Unparseable`] when the result is not a full SHA.
pub fn resolve_commit(git_dir: &Utf8Path, rev: &str) -> Result<String, GitError> {
    let args = [
        "--git-dir",
        git_dir.as_str(),
        "rev-parse",
        &format!("{rev}^{{commit}}"),
    ];
    let output = run(&args, None)?;
    let text = stdout_of(&output);
    if is_full_sha(&text) {
        Ok(text)
    } else {
        Err(GitError::Unparseable {
            args: args.join(" "),
            output: text,
        })
    }
}

/// Whether the dotfiles repository at `repo_root` is out of sync with the
/// branch it tracks on its origin.
///
/// Answered with `ls-remote` only, so it downloads no objects: the remote tip
/// is compared to the local `HEAD`. That makes it a "differs from origin" test
/// rather than a strict "is behind" one, which is the right signal for the
/// notice, since either way the user's next move is `git pull`.
///
/// Every failure reads as "not behind": the repository may not be a git
/// repository at all, may have no configured remote, or the network may be
/// down, and none of those should make a notify-only check noisy or fatal.
#[must_use = "the answer selects which notice message is written"]
pub fn repo_differs_from_origin(repo_root: &Utf8Path) -> bool {
    try_repo_differs_from_origin(repo_root).unwrap_or(false)
}

/// The fallible core of [`repo_differs_from_origin`]: `None` whenever any step
/// could not be answered.
fn try_repo_differs_from_origin(repo_root: &Utf8Path) -> Option<bool> {
    let in_repo = |args: &[&str]| -> Option<String> {
        let mut full = vec!["-C", repo_root.as_str()];
        full.extend_from_slice(args);
        run(&full, None).ok().map(|output| stdout_of(&output))
    };

    // One spawn answers both questions: the SHA on the first line, the
    // abbreviated ref name on the second.
    let answer = in_repo(&["rev-parse", "HEAD", "--abbrev-ref", "HEAD"])?;
    let mut lines = answer.lines();
    let head = lines.next()?.trim().to_owned();
    let branch = lines.next()?.trim().to_owned();
    // A detached HEAD tracks nothing, so there is nothing to be behind.
    if branch == "HEAD" {
        return Some(false);
    }
    let remote = in_repo(&["config", &format!("branch.{branch}.remote")])
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "origin".to_owned());
    let listing = in_repo(&["ls-remote", &remote, &branch])?;
    Some(select_ls_remote_sha(&listing, &branch)? != head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_sha_is_recognized_and_short_or_dirty_ones_are_not() {
        assert!(is_full_sha("a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0"));
        assert!(!is_full_sha("a1b2c3d"), "an abbreviated SHA is not full");
        assert!(
            !is_full_sha("g1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0"),
            "`g` is not a hex digit"
        );
        assert!(!is_full_sha(""));
    }

    #[test]
    fn ls_remote_prefers_the_branch_over_a_same_named_tag() {
        let text = "1111111111111111111111111111111111111111\trefs/tags/main\n\
                    2222222222222222222222222222222222222222\trefs/heads/main";
        assert_eq!(
            select_ls_remote_sha(text, "main").as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
    }

    #[test]
    fn ls_remote_peels_an_annotated_tag_to_its_commit() {
        // The unpeeled `refs/tags/v1` names the tag object; `^{}` names the
        // commit. Pinning the tag object would make every later `cat-file
        // -e ^{commit}` and checkout indirect for no reason.
        let text = "1111111111111111111111111111111111111111\trefs/tags/v1\n\
                    2222222222222222222222222222222222222222\trefs/tags/v1^{}";
        assert_eq!(
            select_ls_remote_sha(text, "v1").as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
    }

    #[test]
    fn ls_remote_reads_the_head_line() {
        let text = "3333333333333333333333333333333333333333\tHEAD";
        assert_eq!(
            select_ls_remote_sha(text, "HEAD").as_deref(),
            Some("3333333333333333333333333333333333333333")
        );
    }

    #[test]
    fn ls_remote_output_with_no_sha_selects_nothing() {
        assert_eq!(select_ls_remote_sha("", "main"), None);
        assert_eq!(
            select_ls_remote_sha("not-a-sha\trefs/heads/main", "main"),
            None,
            "a malformed SHA column must not be accepted as a rev"
        );
    }

    #[test]
    fn ls_remote_refuses_a_suffix_matching_stranger() {
        // `ls-remote` pattern-matches trailing path components, so with
        // `refs/heads/main` gone this is a real answer for `main`. Pinning it
        // would track a ref the user never named.
        let text = "1111111111111111111111111111111111111111\trefs/pull/42/main";
        assert_eq!(select_ls_remote_sha(text, "main"), None);
    }
}
