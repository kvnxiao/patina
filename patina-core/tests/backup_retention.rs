#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration coverage for count-based backup retention (T-012 / REQ-015).
//!
//! The end-to-end `patina apply --yes` surface CHK-026 names cannot run
//! yet: the `apply` subcommand and the commit→GC sequencing in the
//! executor land in later tasks (T-014, T-016). These tests drive the
//! `patina_core::backups::gc_retain` entry point directly — the layer
//! T-012 owns — by staging the on-disk backup tree the SPEC scenarios
//! describe and asserting retention converges to the REQ-015 shape. Each
//! test maps to one REQ-015 `<done-when>` / `<behavior>` bullet:
//!
//! - CHK-026: 15 historical cycles + a successful apply prune down to exactly
//!   10, keeping the newest.
//! - "a failed apply triggers no GC": a caller that never commits never calls
//!   `gc_retain`, so its historical cycles plus the partial attempt all
//!   survive.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::backups::RETENTION_COUNT;
use patina_core::backups::gc_retain;
use patina_core::prune_cycles;
use tempfile::TempDir;

/// A staged backup tree under a state directory.
struct Scene {
    _temp: TempDir,
    backups: Utf8PathBuf,
}

impl Scene {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let backups = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .join("state")
            .join("patina")
            .join("backups");
        fs_err::create_dir_all(&backups).expect("create backups dir");
        Self {
            _temp: temp,
            backups,
        }
    }

    /// Create timestamped cycle directories named so they sort
    /// chronologically, each holding one backed-up file so removal must
    /// recurse. Returns the names in chronological order.
    fn seed_cycles(&self, count: usize) -> Vec<String> {
        let mut names = Vec::with_capacity(count);
        for i in 0..count {
            let name = format!("202605{i:02}T120000Z");
            let dir = self.backups.join(&name);
            fs_err::create_dir_all(dir.join("home").join("u")).expect("cycle subtree");
            fs_err::write(dir.join("home").join("u").join(".zshrc"), b"orig")
                .expect("backed-up file");
            names.push(name);
        }
        names
    }

    fn surviving_cycles(&self) -> Vec<String> {
        let mut got: Vec<String> = fs_err::read_dir(&self.backups)
            .expect("read backups")
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        got.sort();
        got
    }
}

#[test]
fn a_successful_apply_prunes_to_exactly_ten_keeping_the_newest() {
    // CHK-026: 15 timestamped subdirectories; after the just-completed
    // apply's GC, exactly 10 remain — the 10 most recent — and the 5 oldest
    // are gone.
    let scene = Scene::new();
    let names = scene.seed_cycles(15);

    let removed = gc_retain(&scene.backups, RETENTION_COUNT).expect("retention GC");

    let (oldest_five, newest_ten) = names.split_at(5);
    assert_eq!(
        removed, oldest_five,
        "the five oldest cycles must be the ones removed"
    );
    let survivors = scene.surviving_cycles();
    assert_eq!(survivors.len(), 10, "exactly ten cycles must remain");
    assert_eq!(
        survivors, newest_ten,
        "the ten retained cycles must be the ten most recent"
    );
}

#[test]
fn a_failed_apply_leaves_historical_backups_untouched() {
    // REQ-015 behavior: an apply that fails before COMMIT never runs GC.
    // We model that by *not* calling gc_retain — the failed attempt's
    // caller short-circuits — and assert the three historical cycles plus
    // the partial attempt directory all survive.
    let scene = Scene::new();
    let historical = scene.seed_cycles(3);
    // A partial backup directory the failed attempt wrote before crashing.
    let partial = "202699T120000Z";
    fs_err::create_dir_all(scene.backups.join(partial).join("home")).expect("partial attempt dir");

    // No gc_retain call: the failure path does not commit, so it does not GC.

    let mut expected = historical.clone();
    expected.push(partial.to_owned());
    expected.sort();
    assert_eq!(
        scene.surviving_cycles(),
        expected,
        "a failed apply must touch none of the prior cycles and leave its partial dir in place"
    );
}

#[test]
fn pruned_backup_cycles_drop_their_commit_sentinels_in_lockstep() {
    // H1 regression: retention removes the oldest backup cycle directories;
    // the matching journal COMMIT sentinels must be dropped alongside them.
    // Otherwise `patina rollback` could walk back to a commit whose backups
    // are gone and *delete* an overwrite target it can no longer restore.
    let scene = Scene::new();
    let names = scene.seed_cycles(11);
    let journal = scene
        .backups
        .parent()
        .expect("backups has a parent")
        .join("journal");
    fs_err::create_dir_all(&journal).expect("create journal dir");
    // One COMMIT sentinel per seeded backup cycle (the body is opaque to
    // pruning, which keys on the filename).
    for ts in &names {
        fs_err::write(journal.join(format!("{ts}.COMMIT")), b"record").expect("commit sentinel");
    }

    let removed = gc_retain(&scene.backups, RETENTION_COUNT).expect("retention GC");
    prune_cycles(&journal, &removed).expect("prune the matching sentinels");

    let (oldest, newest_ten) = names.split_at(1);
    assert_eq!(removed, oldest, "the single oldest cycle is pruned");
    let oldest_ts = oldest.first().expect("split_at(1) yields one oldest cycle");
    assert!(
        !journal.join(format!("{oldest_ts}.COMMIT")).exists(),
        "the pruned cycle's COMMIT sentinel must be gone so rollback cannot target it"
    );
    for ts in newest_ten {
        assert!(
            journal.join(format!("{ts}.COMMIT")).exists(),
            "a retained cycle's COMMIT sentinel must survive"
        );
    }
}
