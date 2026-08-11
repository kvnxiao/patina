#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Reachability-based pruning of the remote checkout cache.
//!
//! A checkout survives while any journal record on disk still names a path
//! inside it, including records from older commits, because `patina rollback`
//! walks back through them. Everything else that looks like a checkout goes,
//! and so do patina's own scratch artifacts.
//!
//! See `docs/REMOTE_SOURCES.md` "The remote cache".

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::ApplyRecord;
use patina_core::Disposition;
use patina_core::ExpectedTarget;
use patina_core::LastApply;
use patina_core::remote::cache;
use tempfile::TempDir;

/// Two distinct, well-formed checkout directory names.
const REV_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REV_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Fixture {
    _temp: TempDir,
    state: Utf8PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let state = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .join("state");
        fs_err::create_dir_all(state.join("journal").as_std_path()).expect("mkdir journal");
        fs_err::create_dir_all(cache::remotes_root(&state).as_std_path()).expect("mkdir remotes");
        Self { _temp: temp, state }
    }

    /// Create a checkout directory holding one leaf, and return the leaf's
    /// path.
    fn checkout(&self, module: &str, rev: &str) -> Utf8PathBuf {
        let dir = cache::checkout_dir(&self.state, module, rev);
        fs_err::create_dir_all(dir.as_std_path()).expect("mkdir checkout");
        let leaf = dir.join("SKILL.md");
        fs_err::write(leaf.as_std_path(), b"body").expect("write checkout leaf");
        leaf
    }

    /// Write a `<ts>.COMMIT` sentinel whose single recorded target was
    /// materialized from `source`.
    fn commit(&self, ts: &str, source: &Utf8Path) {
        let record = ApplyRecord::new(
            LastApply {
                at: "2026-08-11T14:00:00Z".to_owned(),
                user: "u".to_owned(),
                host: "h".to_owned(),
            },
            vec![ExpectedTarget::Symlink {
                target: "/home/u/.claude/skills/x".to_owned(),
                link_target: source.as_str().to_owned(),
                entry: 0,
                disposition: Disposition::Create,
            }],
        );
        fs_err::write(
            self.state
                .join("journal")
                .join(format!("{ts}.COMMIT"))
                .as_std_path(),
            record.encode().expect("encode the apply record"),
        )
        .expect("write the commit sentinel");
    }
}

#[test]
fn an_unreferenced_checkout_is_pruned_and_a_referenced_one_survives() {
    let f = Fixture::new();
    let kept_leaf = f.checkout("humanizer", REV_A);
    f.checkout("humanizer", REV_B);
    f.commit("20260811T140000Z", &kept_leaf);

    let removed = cache::prune(&f.state).expect("prune the cache");

    assert_eq!(
        removed,
        vec![cache::checkout_dir(&f.state, "humanizer", REV_B)],
        "only the unreferenced checkout may be removed"
    );
    assert!(
        cache::checkout_present(&f.state, "humanizer", REV_A),
        "the checkout a journal record names must survive"
    );
    assert!(!cache::checkout_present(&f.state, "humanizer", REV_B));
}

#[test]
fn a_checkout_referenced_only_by_an_older_commit_survives() {
    // Rollback walks back through older commits, so "referenced by the latest
    // apply" is the wrong retention rule; reachability is per-record.
    let f = Fixture::new();
    let previous_leaf = f.checkout("humanizer", REV_A);
    let current_leaf = f.checkout("humanizer", REV_B);
    f.commit("20260810T140000Z", &previous_leaf);
    f.commit("20260811T140000Z", &current_leaf);

    let removed = cache::prune(&f.state).expect("prune the cache");

    assert!(
        removed.is_empty(),
        "both the current and the previous rev are still reachable, got {removed:?}"
    );
}

#[test]
fn the_bare_repository_is_never_pruned() {
    let f = Fixture::new();
    let bare = cache::bare_repo(&f.state, "humanizer");
    fs_err::create_dir_all(bare.join("objects").as_std_path()).expect("mkdir bare repo");
    f.checkout("humanizer", REV_A);

    let removed = cache::prune(&f.state).expect("prune the cache");

    assert_eq!(
        removed,
        vec![cache::checkout_dir(&f.state, "humanizer", REV_A)],
        "the unreferenced checkout goes but the fetch repository stays"
    );
    assert!(
        bare.is_dir(),
        "pruning must never remove the bare fetch repository"
    );
}

#[test]
fn scratch_artifacts_are_always_removed() {
    let f = Fixture::new();
    let module = cache::module_dir(&f.state, "humanizer");
    let partial = module.join(format!("{REV_A}.partial"));
    let index = module.join(format!("{REV_A}.index"));
    fs_err::create_dir_all(partial.as_std_path()).expect("mkdir staging dir");
    fs_err::write(index.as_std_path(), b"junk").expect("write scratch index");

    let removed = cache::prune(&f.state).expect("prune the cache");

    assert!(
        !partial.exists(),
        "an interrupted staging dir must be swept"
    );
    assert!(!index.exists(), "a scratch index file must be swept");
    assert_eq!(
        removed.len(),
        2,
        "both artifacts must be reported: {removed:?}"
    );
}

#[test]
fn an_unrecognized_directory_name_is_left_alone() {
    // The pruner must not become a general-purpose deleter: anything that is
    // not a checkout, a staging dir, or a scratch index stays put.
    let f = Fixture::new();
    let stray = cache::module_dir(&f.state, "humanizer").join("notes");
    fs_err::create_dir_all(stray.as_std_path()).expect("mkdir stray dir");

    let removed = cache::prune(&f.state).expect("prune the cache");

    assert!(
        removed.is_empty(),
        "nothing prunable was present: {removed:?}"
    );
    assert!(stray.is_dir());
}

#[test]
fn an_undecodable_commit_sentinel_suspends_pruning() {
    // Reachability is unknown, so deleting could strand a rollback. A stale
    // checkout costs only disk.
    let f = Fixture::new();
    f.checkout("humanizer", REV_A);
    fs_err::write(
        f.state
            .join("journal")
            .join("20260811T140000Z.COMMIT")
            .as_std_path(),
        b"",
    )
    .expect("write a torn sentinel");

    let removed = cache::prune(&f.state).expect("prune must not fail on a torn sentinel");

    assert!(
        removed.is_empty(),
        "nothing may be pruned while reachability is unknown, got {removed:?}"
    );
    assert!(cache::checkout_present(&f.state, "humanizer", REV_A));
}

#[test]
fn pruning_an_absent_cache_is_a_clean_no_op() {
    let temp = TempDir::new().expect("tempdir");
    let state = Utf8Path::from_path(temp.path())
        .expect("utf8 temp path")
        .join("state");
    assert!(
        cache::prune(&state)
            .expect("a state dir with no remotes cache prunes cleanly")
            .is_empty()
    );
}
