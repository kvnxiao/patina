#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! The `git` subprocess layer, driven against throwaway origin repositories.
//!
//! Every fixture is a real git repository built in a `TempDir` with the real
//! `git` binary and fetched over the local filesystem, so the suite exercises
//! the actual subprocess plumbing without touching the network.
//!
//! See `docs/REMOTE_SOURCES.md` "The remote cache".

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::remote::cache;
use patina_core::remote::git;
use std::process::Command;
use tempfile::TempDir;

/// A fixed committer epoch the ancestry / age assertions are written against.
const BASE_EPOCH: i64 = 1_700_000_000;

/// Run `git` in `cwd` with a pinned identity and committer/author date, so the
/// commits the fixtures produce have stable, clock-independent SHAs and do not
/// depend on the developer's global git config.
fn git_in(cwd: &Utf8Path, epoch: i64, args: &[&str]) -> String {
    let date = format!("{epoch} +0000");
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd.as_std_path())
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A throwaway origin repository plus an isolated state directory.
struct Fixture {
    _temp: TempDir,
    origin: Utf8PathBuf,
    state: Utf8PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        let origin = root.join("origin");
        let state = root.join("state");
        fs_err::create_dir_all(origin.as_std_path()).expect("mkdir origin");
        fs_err::create_dir_all(state.as_std_path()).expect("mkdir state");
        git_in(
            &root,
            BASE_EPOCH,
            &["init", "--quiet", "-b", "main", "origin"],
        );
        Self {
            _temp: temp,
            origin,
            state,
        }
    }

    /// Write `path` inside the origin with `body`, commit it, and return the
    /// resulting commit SHA.
    fn commit(&self, path: &str, body: &str, epoch: i64) -> String {
        let full = self.origin.join(path);
        if let Some(parent) = full.parent() {
            fs_err::create_dir_all(parent.as_std_path()).expect("mkdir source parent");
        }
        fs_err::write(full.as_std_path(), body).expect("write source");
        git_in(&self.origin, epoch, &["add", "-A"]);
        git_in(&self.origin, epoch, &["commit", "--quiet", "-m", path]);
        git_in(&self.origin, epoch, &["rev-parse", "HEAD"])
    }

    fn bare(&self) -> Utf8PathBuf {
        cache::bare_repo(&self.state, "m")
    }
}

#[test]
fn fetch_by_exact_sha_then_checkout_materializes_the_tree() {
    let f = Fixture::new();
    let sha = f.commit("skills/humanizer/SKILL.md", "hello\n", BASE_EPOCH);

    let checkout = cache::ensure_checkout(&f.state, "m", f.origin.as_str(), Some("main"), &sha)
        .expect("fetch and check out the pinned rev");

    assert_eq!(checkout, cache::checkout_dir(&f.state, "m", &sha));
    assert_eq!(
        fs_err::read_to_string(checkout.join("skills/humanizer/SKILL.md").as_std_path())
            .expect("the checked-out leaf is readable"),
        "hello\n",
        "the checkout must reproduce the committed bytes, subdirectories included"
    );
    assert!(
        !checkout.with_extension("partial").exists(),
        "the staging directory must not survive a successful checkout"
    );
}

#[test]
fn a_second_ensure_checkout_is_a_no_op_and_needs_no_remote() {
    // This is the property that makes a plain `apply` work offline against a
    // warm cache: the origin is deleted, so any fetch attempt would fail.
    let f = Fixture::new();
    let sha = f.commit("a.txt", "one\n", BASE_EPOCH);
    cache::ensure_checkout(&f.state, "m", f.origin.as_str(), Some("main"), &sha)
        .expect("first checkout");
    fs_err::remove_dir_all(f.origin.as_std_path()).expect("delete the origin");

    let again = cache::ensure_checkout(&f.state, "m", f.origin.as_str(), Some("main"), &sha)
        .expect("a warm checkout must not touch the remote");
    assert_eq!(
        fs_err::read_to_string(again.join("a.txt").as_std_path()).expect("leaf readable"),
        "one\n"
    );
}

#[test]
fn a_cold_cache_with_an_unreachable_remote_is_a_typed_error_naming_the_url() {
    let f = Fixture::new();
    let missing = f.state.join("no-such-origin");
    let err = cache::ensure_checkout(
        &f.state,
        "m",
        missing.as_str(),
        Some("main"),
        "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0",
    )
    .expect_err("an unreachable remote with a cold cache must fail");
    let rendered = err.to_string();
    assert!(
        rendered.contains(missing.as_str()),
        "the error must name the remote it could not reach, got: {rendered}"
    );
}

#[test]
fn ancestry_distinguishes_a_fast_forward_from_a_rewrite() {
    let f = Fixture::new();
    let first = f.commit("a.txt", "one\n", BASE_EPOCH);
    let second = f.commit("a.txt", "two\n", BASE_EPOCH + 60);
    // A second root commit on an orphan branch shares no history with `first`,
    // which is the shape a force-push leaves behind.
    git_in(
        &f.origin,
        BASE_EPOCH + 120,
        &["checkout", "--quiet", "--orphan", "rewritten"],
    );
    let rewritten = f.commit("a.txt", "rewritten\n", BASE_EPOCH + 120);

    let bare = f.bare();
    // The pin arrives the way a consumer's cold cache fills it (shallow, by
    // exact SHA) and the candidates arrive the way the update path fetches
    // them, with the history that makes the ancestry question answerable.
    git::fetch_commit(&bare, f.origin.as_str(), &first, Some("main")).expect("fetch the pin");
    git::fetch_history(&bare, f.origin.as_str(), Some("main")).expect("fetch the descendant");
    git::fetch_history(&bare, f.origin.as_str(), Some("rewritten")).expect("fetch the orphan");

    assert!(
        git::is_ancestor(&bare, &first, &second).expect("ancestry query"),
        "an additive commit must read as a descendant of the pin"
    );
    assert!(
        !git::is_ancestor(&bare, &first, &rewritten).expect("ancestry query"),
        "a rewritten history must not read as a descendant of the pin"
    );
}

#[test]
fn committer_time_reads_the_committed_epoch() {
    let f = Fixture::new();
    let sha = f.commit("a.txt", "one\n", BASE_EPOCH);
    let bare = f.bare();
    git::fetch_commit(&bare, f.origin.as_str(), &sha, Some("main")).expect("fetch");

    assert_eq!(
        git::committer_time(&bare, &sha).expect("read the committer time"),
        BASE_EPOCH,
        "the gate reads committer time, so it must be the committed epoch verbatim"
    );
}

#[test]
fn ls_remote_reports_the_branch_tip() {
    let f = Fixture::new();
    f.commit("a.txt", "one\n", BASE_EPOCH);
    let tip = f.commit("a.txt", "two\n", BASE_EPOCH + 60);

    assert_eq!(
        git::ls_remote(f.origin.as_str(), Some("main")).expect("ls-remote the branch"),
        tip
    );
    assert_eq!(
        git::ls_remote(f.origin.as_str(), None).expect("ls-remote the default branch"),
        tip,
        "no ref means the remote's default branch"
    );
}

#[test]
fn ls_remote_on_a_missing_ref_is_an_error_not_a_silent_empty() {
    let f = Fixture::new();
    f.commit("a.txt", "one\n", BASE_EPOCH);
    git::ls_remote(f.origin.as_str(), Some("no-such-branch"))
        .expect_err("a ref the remote does not have must be an error");
}

#[test]
fn has_commit_reports_cache_warmth() {
    let f = Fixture::new();
    let sha = f.commit("a.txt", "one\n", BASE_EPOCH);
    let bare = f.bare();
    git::ensure_bare_repo(&bare).expect("init the bare cache repo");

    assert!(
        !git::has_commit(&bare, &sha).expect("cold-cache probe"),
        "a freshly initialized bare repo holds no commits"
    );
    git::fetch_commit(&bare, f.origin.as_str(), &sha, Some("main")).expect("fetch");
    assert!(
        git::has_commit(&bare, &sha).expect("warm-cache probe"),
        "the fetched commit must read as present"
    );
}

#[test]
fn a_repository_reports_whether_it_matches_its_origin() {
    // This selects which notice `patina remote check` writes, so it must
    // distinguish a clone that is level with its origin from one that is not.
    let f = Fixture::new();
    f.commit("a.txt", "one\n", BASE_EPOCH);

    let clone = f.state.join("clone");
    git_in(
        &f.state,
        BASE_EPOCH,
        &["clone", "--quiet", f.origin.as_str(), clone.as_str()],
    );
    assert!(
        !git::repo_differs_from_origin(&clone),
        "a fresh clone is level with its origin"
    );

    f.commit("a.txt", "two\n", BASE_EPOCH + 60);
    assert!(
        git::repo_differs_from_origin(&clone),
        "a clone whose origin has moved on must report as differing"
    );
}

#[test]
fn a_directory_that_is_not_a_git_repository_reports_no_difference() {
    // The check is notify-only, so an unanswerable question must read as "not
    // behind" rather than failing a command or spamming a shell prompt.
    let f = Fixture::new();
    let plain = f.state.join("not-a-repo");
    fs_err::create_dir_all(plain.as_std_path()).expect("mkdir plain dir");
    assert!(!git::repo_differs_from_origin(&plain));
}

#[test]
fn resolve_commit_normalizes_an_abbreviated_rev_to_the_full_sha() {
    let f = Fixture::new();
    let sha = f.commit("a.txt", "one\n", BASE_EPOCH);
    let bare = f.bare();
    git::fetch_commit(&bare, f.origin.as_str(), &sha, Some("main")).expect("fetch");

    let short = sha.get(..8).expect("a full SHA has at least 8 characters");
    assert_eq!(
        git::resolve_commit(&bare, short).expect("resolve the abbreviated rev"),
        sha
    );
}
