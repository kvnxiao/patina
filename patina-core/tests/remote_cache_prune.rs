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
use patina_core::config::RemoteName;
use patina_core::remote::cache;
use std::collections::BTreeSet;
use tempfile::TempDir;

/// Two distinct, well-formed checkout directory names.
const REV_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REV_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The fixture's one declared remote.
fn humanizer() -> RemoteName {
    RemoteName::parse("humanizer").expect("a legal remote name")
}

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
    fn checkout(&self, module: &RemoteName, rev: &str) -> Utf8PathBuf {
        let dir = cache::checkout_dir(&self.state, module, rev);
        fs_err::create_dir_all(dir.as_std_path()).expect("mkdir checkout");
        let leaf = dir.join("SKILL.md");
        fs_err::write(leaf.as_std_path(), b"body").expect("write checkout leaf");
        leaf
    }

    /// Sweep the cache with `humanizer` as the only declared remote, so the
    /// tests exercise per-checkout reachability rather than the whole-tree
    /// undeclared removal. `keep` is `None` when the current pins are unknown.
    fn prune(&self, keep: Option<&[(RemoteName, String)]>) -> Vec<Utf8PathBuf> {
        let declared = [humanizer()];
        let declared: BTreeSet<&RemoteName> = declared.iter().collect();
        cache::prune(&self.state, &declared, keep).expect("prune the cache")
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
    let kept_leaf = f.checkout(&humanizer(), REV_A);
    f.checkout(&humanizer(), REV_B);
    f.commit("20260811T140000Z", &kept_leaf);

    let removed = f.prune(Some(&[]));

    assert_eq!(
        removed,
        vec![cache::checkout_dir(&f.state, &humanizer(), REV_B)],
        "only the unreferenced checkout may be removed"
    );
    assert!(
        cache::checkout_present(&f.state, &humanizer(), REV_A),
        "the checkout a journal record names must survive"
    );
    assert!(!cache::checkout_present(&f.state, &humanizer(), REV_B));
}

#[test]
fn a_checkout_referenced_only_by_an_older_commit_survives() {
    // Rollback walks back through older commits. "Referenced by the latest
    // apply" is the wrong retention rule; reachability is per-record.
    let f = Fixture::new();
    let previous_leaf = f.checkout(&humanizer(), REV_A);
    let current_leaf = f.checkout(&humanizer(), REV_B);
    f.commit("20260810T140000Z", &previous_leaf);
    f.commit("20260811T140000Z", &current_leaf);

    let removed = f.prune(Some(&[]));

    assert!(
        removed.is_empty(),
        "both the current and the previous rev are still reachable, got {removed:?}"
    );
}

#[test]
fn the_bare_repository_is_never_pruned() {
    let f = Fixture::new();
    let bare = cache::bare_repo(&f.state, &humanizer());
    fs_err::create_dir_all(bare.join("objects").as_std_path()).expect("mkdir bare repo");
    f.checkout(&humanizer(), REV_A);

    let removed = f.prune(Some(&[]));

    assert_eq!(
        removed,
        vec![cache::checkout_dir(&f.state, &humanizer(), REV_A)],
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
    let module = cache::module_dir(&f.state, &humanizer());
    let partial = module.join(format!("{REV_A}.partial"));
    let pid_partial = module.join(format!("{REV_A}.partial.4242"));
    let index = module.join(format!("{REV_A}.index"));
    let pid_index = module.join(format!("{REV_A}.partial.4242.index"));
    fs_err::create_dir_all(partial.as_std_path()).expect("mkdir staging dir");
    fs_err::create_dir_all(pid_partial.as_std_path()).expect("mkdir pid staging dir");
    fs_err::write(index.as_std_path(), b"junk").expect("write scratch index");
    fs_err::write(pid_index.as_std_path(), b"junk").expect("write pid scratch index");

    let removed = f.prune(Some(&[]));

    for artifact in [&partial, &pid_partial, &index, &pid_index] {
        assert!(!artifact.exists(), "{artifact} must be swept");
    }
    assert_eq!(
        removed.len(),
        4,
        "every artifact must be reported: {removed:?}"
    );
}

#[test]
fn a_pinned_checkout_survives_without_a_journal_reference() {
    // A pin bumped but not yet applied has no journal record. It is still the
    // warm cache an offline apply needs, and a concurrent plan may already
    // point into this checkout.
    let f = Fixture::new();
    f.checkout(&humanizer(), REV_A);
    let keep = vec![(humanizer(), REV_A.to_owned())];

    let removed = f.prune(Some(&keep));

    assert!(
        removed.is_empty(),
        "a currently pinned checkout must survive, got {removed:?}"
    );
    assert!(cache::checkout_present(&f.state, &humanizer(), REV_A));
}

#[test]
fn an_undeclared_remotes_tree_goes_whole() {
    let f = Fixture::new();
    let gone = RemoteName::parse("gone").expect("a legal remote name");
    f.checkout(&humanizer(), REV_A);
    f.checkout(&gone, REV_B);

    let removed = f.prune(Some(&[]));

    assert!(
        !cache::module_dir(&f.state, &gone).exists(),
        "an undeclared remote's whole tree must be removed"
    );
    assert!(
        removed
            .iter()
            .any(|path| path == &cache::module_dir(&f.state, &gone)),
        "the removed tree must be reported: {removed:?}"
    );
}

#[test]
fn a_cache_directory_spelled_in_another_case_is_still_the_declared_remote() {
    // Checkouts are keyed by the folded name, but a directory written before
    // that (or created on a case-insensitive filesystem under the spelling its
    // author first used) carries the display spelling. The undeclared sweep
    // must not read it as a deleted remote and delete the tree whole.
    let f = Fixture::new();
    let legacy = cache::remotes_root(&f.state).join("Humanizer");
    fs_err::create_dir_all(legacy.join(REV_A).as_std_path()).expect("mkdir the legacy tree");

    let removed = f.prune(Some(&[(humanizer(), REV_A.to_owned())]));

    assert!(
        legacy.is_dir(),
        "a declared remote's tree must survive whatever case it is spelled in: {removed:?}"
    );
}

#[test]
fn unknown_pins_leave_a_declared_remotes_checkouts_alone() {
    // A run where no active entry selected a remote never read the lockfile, so
    // every checkout here may be the one that remote is currently pinned to.
    // An undeclared remote has no pin to protect, so its tree still goes.
    let f = Fixture::new();
    f.checkout(&humanizer(), REV_A);
    let gone = RemoteName::parse("gone").expect("a legal remote name");
    f.checkout(&gone, REV_B);

    let removed = f.prune(None);

    assert!(
        cache::checkout_present(&f.state, &humanizer(), REV_A),
        "an unreferenced checkout must survive while the pins are unknown"
    );
    assert_eq!(
        removed,
        vec![cache::module_dir(&f.state, &gone)],
        "only the undeclared tree may go"
    );
}

#[test]
fn a_partial_or_index_with_a_non_sha_stem_is_left_alone() {
    // The scratch sweep keys on a full-SHA stem. A `notes.partial` or
    // `manifest.index` a user or a future version left here is not patina's
    // staging artifact, so the pruner must not delete it.
    let f = Fixture::new();
    let module = cache::module_dir(&f.state, &humanizer());
    let stray_partial = module.join("notes.partial");
    let stray_index = module.join("manifest.index");
    fs_err::create_dir_all(stray_partial.as_std_path()).expect("mkdir stray partial");
    fs_err::write(stray_index.as_std_path(), b"keep").expect("write stray index");

    let removed = f.prune(Some(&[]));

    assert!(
        removed.is_empty(),
        "a non-SHA-stemmed .partial/.index must not be swept: {removed:?}"
    );
    assert!(
        stray_partial.is_dir(),
        "the stray staging-looking dir stays"
    );
    assert!(stray_index.exists(), "the stray index-looking file stays");
}

#[test]
fn an_unrecognized_directory_name_is_left_alone() {
    // The pruner never deletes anything except a checkout, a staging dir, or
    // a scratch index.
    let f = Fixture::new();
    let stray = cache::module_dir(&f.state, &humanizer()).join("notes");
    fs_err::create_dir_all(stray.as_std_path()).expect("mkdir stray dir");

    let removed = f.prune(Some(&[]));

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
    f.checkout(&humanizer(), REV_A);
    fs_err::write(
        f.state
            .join("journal")
            .join("20260811T140000Z.COMMIT")
            .as_std_path(),
        b"",
    )
    .expect("write a torn sentinel");

    let removed = f.prune(Some(&[]));

    assert!(
        removed.is_empty(),
        "nothing may be pruned while reachability is unknown, got {removed:?}"
    );
    assert!(cache::checkout_present(&f.state, &humanizer(), REV_A));
}

#[test]
fn pruning_an_absent_cache_is_a_clean_no_op() {
    let temp = TempDir::new().expect("tempdir");
    let state = Utf8Path::from_path(temp.path())
        .expect("utf8 temp path")
        .join("state");
    assert!(
        cache::prune(&state, &BTreeSet::new(), Some(&[]))
            .expect("a state dir with no remotes cache prunes cleanly")
            .is_empty()
    );
}
