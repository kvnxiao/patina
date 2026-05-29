#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration coverage for crash recovery (T-011 / REQ-013).
//!
//! The end-to-end `patina apply --yes` surface CHK-024 names cannot run
//! yet: the `apply` subcommand, the executor loop, and the backup writer
//! land in later tasks (T-012, T-014, T-016). These tests drive the
//! `patina_core::journal::recover_orphans` entry point directly — the
//! layer T-011 owns — by staging the on-disk crash state the SPEC
//! scenarios describe (a flushed `<ts>.plan`, a per-apply backup tree, no
//! `<ts>.COMMIT`) and asserting recovery converges backward to the
//! pre-apply state. Each test maps to one REQ-013 `<done-when>` bullet:
//!
//! - CHK-024: N-of-M completed ops are reversed from backups; orphan plan and
//!   progress files are removed (the headline scenario).
//! - "interrupted before any op executed": no targets touched, orphan files
//!   removed, filesystem unchanged.
//! - "recovery is idempotent": a second pass is a clean no-op.
//! - "recovery never proceeds forward": an un-started op's target is left
//!   absent — recovery rolls back, it does not finish the apply.
//! - probe-over-cursor: a lying progress cursor does not change the reversal,
//!   because recovery probes the filesystem.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::journal::PLAN_SUFFIX;
use patina_core::journal::PROGRESS_SUFFIX;
use patina_core::journal::Plan;
use patina_core::journal::PlannedOperation;
use patina_core::journal::mirror_backup_path;
use patina_core::journal::recover_orphans;
use tempfile::TempDir;

/// A staged crash scene: a state directory with a `journal/` and
/// `backups/` tree, plus a `home/` standing in for the user's targets.
struct Scene {
    _temp: TempDir,
    journal: Utf8PathBuf,
    backups: Utf8PathBuf,
    home: Utf8PathBuf,
}

const TS: &str = "20260528T120000Z";

impl Scene {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let journal = root.join("journal");
        let backups = root.join("backups");
        let home = root.join("home");
        for d in [&journal, &backups, &home] {
            fs_err::create_dir_all(d).expect("create scene dir");
        }
        Self {
            _temp: temp,
            journal,
            backups,
            home,
        }
    }

    fn target(&self, name: &str) -> Utf8PathBuf {
        self.home.join(name)
    }

    /// Stage an *overwrite*: the target pre-existed with `original`
    /// content, the backup of that original was taken, and the apply then
    /// replaced the target with `new_content` before crashing.
    fn stage_overwrite(&self, name: &str, original: &str, new_content: &str) -> PlannedOperation {
        let target = self.target(name);
        // The engine stashed the original under the per-apply backup root
        // before mutating (REQ-014 layout).
        let backup = mirror_backup_path(&self.backups, TS, &target);
        if let Some(parent) = backup.parent() {
            fs_err::create_dir_all(parent).expect("backup parent");
        }
        fs_err::write(&backup, original).expect("write backup");
        // The crashed apply left the new (overwriting) content in place.
        fs_err::write(&target, new_content).expect("write overwriting target");
        PlannedOperation::copy(format!("repo/{name}"), target.as_str())
    }

    /// Stage a *fresh creation* that completed: no backup (nothing
    /// pre-existed), and the target now holds the freshly created content.
    fn stage_fresh_created(&self, name: &str, content: &str) -> PlannedOperation {
        let target = self.target(name);
        if let Some(parent) = target.parent() {
            fs_err::create_dir_all(parent).expect("target parent");
        }
        fs_err::write(&target, content).expect("write fresh target");
        PlannedOperation::copy(format!("repo/{name}"), target.as_str())
    }

    /// Stage a *fresh creation* that never started: no backup, no target.
    fn stage_fresh_unstarted(&self, name: &str) -> PlannedOperation {
        PlannedOperation::copy(format!("repo/{name}"), self.target(name).as_str())
    }

    /// Write the orphan plan (no COMMIT sentinel) for the crash scene.
    fn write_orphan_plan(&self, ops: Vec<PlannedOperation>) {
        let plan = Plan::new(ops);
        let bytes = plan.encode().expect("encode plan");
        fs_err::write(self.journal.join(format!("{TS}{PLAN_SUFFIX}")), bytes).expect("write plan");
    }

    /// Write a progress cursor claiming `completed` op indices.
    fn write_progress(&self, completed: &[u32]) {
        let mut bytes = Vec::new();
        for &i in completed {
            bytes.extend_from_slice(&i.to_le_bytes());
            bytes.push(1); // COMPLETED_MARKER
        }
        fs_err::write(self.journal.join(format!("{TS}{PROGRESS_SUFFIX}")), bytes)
            .expect("write progress");
    }

    fn plan_exists(&self) -> bool {
        self.journal.join(format!("{TS}{PLAN_SUFFIX}")).exists()
    }

    fn progress_exists(&self) -> bool {
        self.journal.join(format!("{TS}{PROGRESS_SUFFIX}")).exists()
    }
}

// CHK-024: an apply that completed 3 of 5 file operations before SIGKILL,
// with the backup directory intact, when recovery runs, restores the 3
// previously-overwritten targets from backups to their pre-apply content
// and removes the orphaned plan + progress files.
#[test]
fn restores_overwritten_targets_and_clears_orphan_files() {
    let scene = Scene::new();

    // Ops 0..3 overwrote pre-existing files (3 of 5 completed). Ops 3, 4
    // were fresh creations; one started, one did not.
    let ops = vec![
        scene.stage_overwrite("a", "orig-a", "new-a"),
        scene.stage_overwrite("b", "orig-b", "new-b"),
        scene.stage_overwrite("c", "orig-c", "new-c"),
        scene.stage_fresh_created("d", "new-d"),
        scene.stage_fresh_unstarted("e"),
    ];
    scene.write_orphan_plan(ops);
    scene.write_progress(&[0, 1, 2]); // cursor: 3 of 5 done

    let report = recover_orphans(&scene.journal, &scene.backups).expect("recovery");
    assert_eq!(
        report.recovered_timestamps(),
        &[TS.to_owned()],
        "the single orphan plan is recovered"
    );

    // The 3 overwritten targets are restored to pre-apply content.
    for (name, original) in [("a", "orig-a"), ("b", "orig-b"), ("c", "orig-c")] {
        let got = fs_err::read_to_string(scene.target(name)).expect("read restored target");
        assert_eq!(got, original, "target {name} restored to pre-apply bytes");
    }
    // The fresh creation that completed is deleted (no backup to restore).
    assert!(
        !scene.target("d").exists(),
        "freshly-created target is removed, converging to pre-apply (absent)"
    );

    // The orphan plan and progress files are gone.
    assert!(!scene.plan_exists(), "orphan plan removed");
    assert!(!scene.progress_exists(), "orphan progress removed");
}

// REQ-013 <done-when>: an apply interrupted before any operation executed
// leaves the filesystem in the pre-apply state; the plan and progress
// files are removed.
#[test]
fn interrupted_before_any_op_touches_nothing_and_clears_orphan() {
    let scene = Scene::new();
    let ops = vec![
        scene.stage_fresh_unstarted("a"),
        scene.stage_fresh_unstarted("b"),
    ];
    scene.write_orphan_plan(ops);
    scene.write_progress(&[]); // nothing completed

    recover_orphans(&scene.journal, &scene.backups).expect("recovery");

    assert!(!scene.target("a").exists(), "no target was created");
    assert!(!scene.target("b").exists(), "no target was created");
    assert!(!scene.plan_exists(), "orphan plan removed");
    assert!(!scene.progress_exists(), "orphan progress removed");
}

// REQ-013 <done-when>: recovery is idempotent — running it twice yields
// the same final state as running it once, with no error on the second
// pass.
#[test]
fn recovery_is_idempotent() {
    let scene = Scene::new();
    let ops = vec![
        scene.stage_overwrite("a", "orig-a", "new-a"),
        scene.stage_fresh_created("b", "new-b"),
    ];
    scene.write_orphan_plan(ops);
    scene.write_progress(&[0, 1]);

    let first = recover_orphans(&scene.journal, &scene.backups).expect("first recovery");
    assert!(first.recovered_any(), "first pass reverses the orphan");

    let a_after_first = fs_err::read_to_string(scene.target("a")).expect("read a");
    let b_exists_after_first = scene.target("b").exists();

    // Second pass: the orphan plan is gone, so there is nothing to do.
    let second = recover_orphans(&scene.journal, &scene.backups).expect("second recovery");
    assert!(
        !second.recovered_any(),
        "second pass finds no orphan and is a no-op"
    );

    assert_eq!(
        fs_err::read_to_string(scene.target("a")).expect("read a again"),
        a_after_first,
        "restored content is stable across a second recovery"
    );
    assert_eq!(
        scene.target("b").exists(),
        b_exists_after_first,
        "deleted fresh target stays deleted across a second recovery"
    );
    assert_eq!(a_after_first, "orig-a");
    assert!(!b_exists_after_first);
}

// REQ-013 <done-when>: recovery never proceeds forward. An operation that
// never started (fresh target absent, no backup) is left absent — recovery
// rolls back, it does not finish the half-done apply by creating it.
#[test]
fn recovery_rolls_back_and_never_completes_an_unstarted_op() {
    let scene = Scene::new();
    let ops = vec![
        scene.stage_overwrite("done", "orig", "new"),
        scene.stage_fresh_unstarted("never"),
    ];
    scene.write_orphan_plan(ops);
    scene.write_progress(&[0]);

    recover_orphans(&scene.journal, &scene.backups).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(scene.target("done")).expect("read done"),
        "orig",
        "the completed op is reversed to pre-apply bytes"
    );
    assert!(
        !scene.target("never").exists(),
        "the un-started op is NOT completed forward — it stays absent"
    );
}

// REQ-013 <behavior> / probe-over-cursor: a progress cursor that lies
// (reports more completed than the filesystem reflects) does not change
// the reversal, because recovery probes the filesystem and consults the
// backup directory rather than trusting the cursor.
#[test]
fn lying_progress_cursor_is_ignored_in_favour_of_the_filesystem() {
    let scene = Scene::new();
    // Only op 0 actually overwrote a target; op 1 never started.
    let ops = vec![
        scene.stage_overwrite("a", "orig-a", "new-a"),
        scene.stage_fresh_unstarted("b"),
    ];
    scene.write_orphan_plan(ops);
    // The cursor lies: it claims both ops completed.
    scene.write_progress(&[0, 1]);

    recover_orphans(&scene.journal, &scene.backups).expect("recovery");

    // Op 0 is reversed from its backup regardless of the cursor.
    assert_eq!(
        fs_err::read_to_string(scene.target("a")).expect("read a"),
        "orig-a",
        "the genuinely-overwritten target is restored"
    );
    // Op 1's target was never created; recovery does not invent one to
    // satisfy the cursor's false claim.
    assert!(
        !scene.target("b").exists(),
        "the cursor's lie about op 1 does not cause a phantom restore/delete"
    );
    assert!(!scene.plan_exists(), "orphan plan removed");
}
