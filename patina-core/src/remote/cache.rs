//! The per-machine remote cache under `<state>/remotes/`.
//!
//! ```text
//! <state>/remotes/
//! ├── notice                       plain-text pending-update notice
//! ├── pending                      the same fact, one module name per line
//! ├── last_check                   background-check throttle stamp
//! └── <module>/
//!     ├── repo.git/                bare fetch repository
//!     └── <sha>/                   immutable checkout, one per pinned rev
//! ```
//!
//! Checkouts never live in the dotfiles repository. A checkout directory is
//! immutable once it exists: an update writes a *new* directory, and apply
//! re-points links at it through the ordinary journaled flow, so no content
//! ever changes under a live symlink. This immutability lets `patina rollback`
//! re-point links back, and explains why pruning is reachability-based rather
//! than "keep the newest".
//!
//! See `docs/REMOTE_SOURCES.md` "The remote cache".

use super::RemoteError;
use super::RemoteRepr;
use super::git;
use crate::config::remote::RemoteName;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::collections::BTreeSet;

/// Directory name of the bare fetch repository inside a module's cache
/// directory.
const BARE_REPO_DIR: &str = "repo.git";

/// Suffix of the directory a checkout is written into before it is renamed into
/// place. Its presence means an interrupted checkout, never a usable one.
const PARTIAL_SUFFIX: &str = ".partial";

/// `<state>/remotes/`, the root of the remote cache.
#[must_use = "the cache root locates every checkout and the notice files"]
pub fn remotes_root(state_dir: &Utf8Path) -> Utf8PathBuf {
    state_dir.join("remotes")
}

/// `<state>/remotes/<module>/`, one directory per remote-backed module.
///
/// Named by the remote's folded key rather than the spelling its author wrote.
/// A case-only respelling of a declaration therefore keeps addressing the
/// checkouts already on disk, without cold-starting a second tree beside
/// them.
#[must_use = "the module cache directory holds the bare repo and its checkouts"]
pub fn module_dir(state_dir: &Utf8Path, module: &RemoteName) -> Utf8PathBuf {
    remotes_root(state_dir).join(module.key())
}

/// `<state>/remotes/<module>/repo.git`, the bare repository fetches land in.
#[must_use = "the bare repository is the git-dir every remote git call uses"]
pub fn bare_repo(state_dir: &Utf8Path, module: &RemoteName) -> Utf8PathBuf {
    module_dir(state_dir, module).join(BARE_REPO_DIR)
}

/// `<state>/remotes/<module>/<rev>/`, the immutable checkout of one pinned
/// rev.
#[must_use = "entry sources use the checkout directory"]
pub fn checkout_dir(state_dir: &Utf8Path, module: &RemoteName, rev: &str) -> Utf8PathBuf {
    module_dir(state_dir, module).join(rev)
}

/// `<state>/remotes/notice`, the plain-text pending-update notice a shell
/// startup prints.
#[must_use = "the notice path is read by the shell integration and by `patina status`"]
pub fn notice_path(state_dir: &Utf8Path) -> Utf8PathBuf {
    remotes_root(state_dir).join("notice")
}

/// `<state>/remotes/pending`, the module names the last check found behind:
/// the machine-readable twin of the prose notice.
#[must_use = "the pending path carries the per-remote state `remote list` reports"]
pub fn pending_path(state_dir: &Utf8Path) -> Utf8PathBuf {
    remotes_root(state_dir).join("pending")
}

/// `<state>/remotes/last_check`, the background-check throttle stamp.
#[must_use = "the stamp path drives the `remote check --hook` self-throttle"]
pub fn last_check_path(state_dir: &Utf8Path) -> Utf8PathBuf {
    remotes_root(state_dir).join("last_check")
}

/// Whether the checkout of `rev` for `module` is already materialized.
#[must_use = "a warm checkout lets a plain apply run offline"]
pub fn checkout_present(state_dir: &Utf8Path, module: &RemoteName, rev: &str) -> bool {
    checkout_dir(state_dir, module, rev).is_dir()
}

/// Materialize the checkout of `rev` for `module`, fetching the commit first
/// when the bare repository does not already hold it.
///
/// A present checkout directory short-circuits with no `git` call at all, so a
/// plain `apply` against a warm cache runs fully offline. A fresh checkout is
/// written into a `<rev>.partial.<pid>` sibling, then renamed into place. The
/// directory's existence is therefore proof it is complete, not a half-written
/// tree from an interrupted run. The staging name carries the writer's pid.
/// Materialization runs without the process lock, because plan time precedes
/// the consent prompt, so two processes may stage the same rev concurrently.
/// Each writes its own directory. Whichever renames second finds the
/// destination already present and discards its copy, because both staged the
/// same immutable commit.
///
/// Returns the checkout directory.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the fetch, the checkout, or the rename fails.
pub fn ensure_checkout(
    state_dir: &Utf8Path,
    module: &RemoteName,
    url: &str,
    git_ref: Option<&str>,
    rev: &str,
) -> Result<Utf8PathBuf, RemoteError> {
    let final_dir = checkout_dir(state_dir, module, rev);
    if final_dir.is_dir() {
        return Ok(final_dir);
    }

    let git_dir = bare_repo(state_dir, module);
    if !git_dir.is_dir() || !git::has_commit(&git_dir, rev)? {
        git::fetch_commit(&git_dir, url, rev, git_ref)?;
    }

    let staging = staging_dir(&final_dir);
    // A leftover staging directory from an interrupted run (same recycled pid)
    // would otherwise mix its files into this checkout.
    remove_any(&staging)?;
    git::checkout_commit(&git_dir, rev, &staging)?;
    if let Err(source) = fs_err::rename(staging.as_std_path(), final_dir.as_std_path()) {
        if final_dir.is_dir() {
            remove_any(&staging)?;
            return Ok(final_dir);
        }
        return Err(RemoteRepr::Cache {
            action: "renaming the staged checkout into",
            path: final_dir.clone(),
            source,
        }
        .into());
    }
    Ok(final_dir)
}

/// The `<rev>.partial.<pid>` sibling a checkout is staged in.
fn staging_dir(final_dir: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!(
        "{final_dir}{PARTIAL_SUFFIX}.{}",
        std::process::id()
    ))
}

/// Sweep the cache. Remove the whole tree of every remote `declared` does not
/// name. Then remove every checkout no journal record references and no
/// `keep` pin names. Returns what was removed, sorted.
///
/// Reachability is read from every `<ts>.COMMIT` sentinel in the journal
/// directory, not just the newest. `patina rollback` walks back through them,
/// so a checkout an older commit still names must survive. A recorded source
/// path that falls inside a checkout, or inside an undeclared remote's tree,
/// keeps it. Everything else under a module's cache directory that looks like
/// a checkout goes. An undeclared remote loses its directory whole, bare fetch
/// repository included.
///
/// `keep` carries the `(name, rev)` of every current pin, which must survive
/// even when no journal record names it. A pin bumped but not yet applied is
/// the warm cache an offline apply depends on. A plan another process has
/// materialized, but not yet confirmed, points at a pinned rev by
/// construction.
/// `None` means the current pins could not be read at all, and no declared
/// remote's checkouts are touched. An undeclared remote has no pin to protect
/// by definition, so its tree still goes.
///
/// A directory name read off disk is compared folded, because a cache written
/// before checkouts were keyed by the folded name carries the spelling the
/// remote was first declared under.
///
/// When any sentinel fails to decode, nothing is pruned. Deleting on partial
/// knowledge could strand a rollback, and a stale checkout only costs disk.
///
/// Staging leftovers (`<rev>.partial.<pid>`) and scratch index files are
/// always removed: both are derivable, and neither is ever referenced.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the cache directory cannot be read or a
/// removal fails.
pub fn prune(
    state_dir: &Utf8Path,
    declared: &BTreeSet<&RemoteName>,
    keep: Option<&[(RemoteName, String)]>,
) -> Result<Vec<Utf8PathBuf>, RemoteError> {
    use crate::config::remote::name_key;

    let root = remotes_root(state_dir);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let Some(referenced) = referenced_paths(&state_dir.join("journal"))? else {
        return Ok(Vec::new());
    };
    let declared: BTreeSet<&str> = declared.iter().map(|name| name.key()).collect();

    let mut removed = Vec::new();
    for module in read_subdirectories(&root)? {
        let Some(module_name) = module.file_name() else {
            continue;
        };
        let module_key = name_key(module_name);
        let is_declared = declared.contains(module_key.as_str());
        // An undeclared tree goes whole once nothing points into it. While a
        // journal record still points into it, the sweep degrades to the
        // per-checkout logic below, so rollback keeps only what it needs.
        if !is_declared && !is_referenced(&module, &referenced) {
            remove_any(&module)?;
            removed.push(module);
            continue;
        }
        let keep = match keep {
            Some(pins) => pins,
            // The current pins are unknown, so any checkout here may be the one
            // this remote is pinned to. Leave the whole directory alone.
            None if is_declared => continue,
            None => &[],
        };
        for candidate in read_dir_entries(&module)? {
            let Some(name) = candidate.file_name() else {
                continue;
            };
            if name == BARE_REPO_DIR {
                continue;
            }
            if is_scratch_name(name) {
                remove_any(&candidate)?;
                removed.push(candidate);
                continue;
            }
            if !is_checkout_name(name)
                || keep
                    .iter()
                    .any(|(kept, rev)| rev == name && kept.key() == module_key)
                || is_referenced(&candidate, &referenced)
            {
                continue;
            }
            remove_any(&candidate)?;
            removed.push(candidate);
        }
    }
    removed.sort();
    Ok(removed)
}

/// Whether `name` is one of Patina's own scratch artifacts: a full-SHA stem
/// plus a staging or `.index` suffix. Requiring the SHA stem keeps the sweep
/// from deleting an unrelated `notes.partial` a user or a future version might
/// place here.
fn is_scratch_name(name: &str) -> bool {
    let partial = PARTIAL_SUFFIX.trim_start_matches('.');
    name.split_once('.').is_some_and(|(stem, rest)| {
        is_checkout_name(stem)
            && (rest == "index"
                || rest == partial
                || rest
                    .strip_prefix(partial)
                    .is_some_and(|tail| tail.starts_with('.')))
    })
}

/// Whether `name` is shaped like a checkout directory (a full commit SHA).
///
/// Anything else under a module's cache directory is left alone: the pruner
/// must not become a general-purpose deleter of a directory a user or a future
/// version put there.
fn is_checkout_name(name: &str) -> bool {
    super::git::is_full_sha(name)
}

/// Whether any recorded source path lies inside `checkout`.
///
/// Recorded paths went through [`crate::paths::canonicalize`] at apply time,
/// while `checkout` is built from the state directory as the environment spells
/// it. The two spellings routinely differ. macOS resolves `/var` to
/// `/private/var`. Windows hands back 8.3 short names like `RUNNER~1`. A
/// symlinked `HOME` or a `.` segment produces the same mismatch on any host.
/// Comparing only the raw form would find no reference and delete a checkout
/// that is live under a symbolic link. Both spellings are tested for that
/// reason.
fn is_referenced(checkout: &Utf8Path, referenced: &BTreeSet<Utf8PathBuf>) -> bool {
    let canonical = crate::paths::canonicalize(checkout);
    referenced.iter().any(|path| {
        path.starts_with(checkout)
            || canonical
                .as_ref()
                .is_ok_and(|canonical| path.starts_with(canonical))
    })
}

/// Every source path named by any committed apply record in `journal_dir`, or
/// `None` when a sentinel could not be decoded and reachability is therefore
/// unknown.
fn referenced_paths(journal_dir: &Utf8Path) -> Result<Option<BTreeSet<Utf8PathBuf>>, RemoteError> {
    let mut paths = BTreeSet::new();
    if !journal_dir.is_dir() {
        return Ok(Some(paths));
    }
    for entry in read_dir_entries(journal_dir)? {
        let Some(name) = entry.file_name() else {
            continue;
        };
        if !name.ends_with(crate::journal::COMMIT_SUFFIX) {
            continue;
        }
        let bytes = fs_err::read(entry.as_std_path()).map_err(|source| RemoteRepr::Cache {
            action: "reading",
            path: entry.clone(),
            source,
        })?;
        let Ok(record) = crate::journal::ApplyRecord::decode(&bytes) else {
            tracing::warn!(
                sentinel = %entry,
                "skipping the remote-cache prune: a journal commit sentinel could not be \
                 decoded, so which checkouts are still reachable is unknown"
            );
            return Ok(None);
        };
        for target in &record.targets {
            paths.insert(Utf8PathBuf::from(target.source()));
        }
    }
    Ok(Some(paths))
}

/// Immediate subdirectories of `dir`, sorted.
fn read_subdirectories(dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, RemoteError> {
    Ok(read_dir_entries(dir)?
        .into_iter()
        .filter(|path| path.is_dir())
        .collect())
}

/// Immediate entries of `dir` as UTF-8 paths, sorted so the pruner's report is
/// a stable function of the directory's contents.
fn read_dir_entries(dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, RemoteError> {
    let mut paths = Vec::new();
    let entries = fs_err::read_dir(dir.as_std_path()).map_err(|source| RemoteRepr::Cache {
        action: "reading",
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| RemoteRepr::Cache {
            action: "reading",
            path: dir.to_path_buf(),
            source,
        })?;
        if let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Remove a file, symlink, or directory tree, tolerating absence.
pub(super) fn remove_any(path: &Utf8Path) -> Result<(), RemoteError> {
    crate::fsx::remove_entry(path).map_err(|source| {
        RemoteRepr::Cache {
            action: "removing",
            path: path.to_path_buf(),
            source,
        }
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn the_layout_nests_every_path_under_the_cache_root() {
        // The pruner walks module directories under the root and compares
        // recorded source paths against checkout directories. A path that
        // escaped the root, or a checkout that was not under its module,
        // would break the sweep. Asserting the relations rather than the
        // literal strings keeps this from being a second copy of the same
        // path constants.
        let state = Utf8Path::new("/state/patina");
        let root = remotes_root(state);
        let humanizer = RemoteName::parse("humanizer").expect("a legal remote name");
        let module = module_dir(state, &humanizer);
        let bare = bare_repo(state, &humanizer);
        let checkout = checkout_dir(state, &humanizer, "abc123");

        assert!(root.starts_with(state) && root != state);
        assert!(module.starts_with(&root) && module != root);
        assert!(bare.starts_with(&module) && checkout.starts_with(&module));
        assert_ne!(
            bare, checkout,
            "the fetch repository and a checkout must be distinct directories"
        );
        for path in [
            notice_path(state),
            pending_path(state),
            last_check_path(state),
        ] {
            assert_eq!(
                path.parent(),
                Some(root.as_path()),
                "{path} must sit directly in the cache root, beside the module dirs"
            );
        }
    }

    #[test]
    fn a_respelled_declaration_addresses_the_same_cache_directory() {
        // The directory is named by the folded key, so respelling a
        // declaration keeps the warm cache instead of cold-starting a second
        // tree beside it (and leaving the first one to linger forever).
        let state = Utf8Path::new("/state/patina");
        let written = RemoteName::parse("Humanizer").expect("a legal remote name");
        let respelled = RemoteName::parse("humanizer").expect("a legal remote name");
        assert_eq!(module_dir(state, &written), module_dir(state, &respelled));
        assert_eq!(
            checkout_dir(state, &written, "abc123"),
            checkout_dir(state, &respelled, "abc123")
        );
    }

    #[test]
    fn only_full_sha_names_are_treated_as_checkouts() {
        assert!(is_checkout_name("a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0"));
        assert!(!is_checkout_name("repo.git"));
        assert!(!is_checkout_name("a1b2c3d"));
        assert!(
            !is_checkout_name("a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9bz"),
            "a non-hex character disqualifies the name"
        );
    }

    #[test]
    fn a_source_inside_a_checkout_keeps_it_and_a_sibling_does_not() {
        let checkout = Utf8PathBuf::from("/state/remotes/m/aaaa");
        let mut referenced = BTreeSet::new();
        referenced.insert(Utf8PathBuf::from("/state/remotes/m/aaaa/skills/x.md"));
        assert!(is_referenced(&checkout, &referenced));
        assert!(
            !is_referenced(&Utf8PathBuf::from("/state/remotes/m/bbbb"), &referenced),
            "an unrelated checkout must not be kept alive by another's reference"
        );
    }

    #[test]
    fn a_checkout_spelled_differently_from_the_recorded_path_is_still_referenced() {
        // A `..` hop reproduces the spelling mismatch on every host:
        // `Path::components` preserves it, so `starts_with` fails, while
        // canonicalization resolves it away.
        let temp = TempDir::new().expect("tempdir");
        let real = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let checkout = real.join("remotes").join("m").join("aaaa");
        fs_err::create_dir_all(checkout.as_std_path()).expect("mkdir checkout");

        let canonical = crate::paths::canonicalize(&checkout).expect("canonicalize the checkout");
        let mut referenced = BTreeSet::new();
        referenced.insert(canonical.join("SKILL.md"));

        let hopped = real
            .join("remotes")
            .join("m")
            .join("..")
            .join("m")
            .join("aaaa");
        assert!(
            !referenced.iter().any(|path| path.starts_with(&hopped)),
            "the fixture must actually produce two different spellings"
        );
        assert!(
            is_referenced(&hopped, &referenced),
            "a reference recorded under the canonical spelling must keep the checkout"
        );
    }
}
