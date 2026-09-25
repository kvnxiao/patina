//! Integration tests for recovery crash.

#![cfg(test)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::Disposition;
use patina_core::journal::PLAN_SUFFIX;
use patina_core::journal::PROGRESS_SUFFIX;
use patina_core::journal::Plan;
use patina_core::journal::PlannedOperation;
use patina_core::journal::mirror_backup_path;
use patina_core::journal::recover_orphans;
use tempfile::TempDir;

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

    fn stage_overwrite(&self, name: &str, original: &str, new_content: &str) -> PlannedOperation {
        let target = self.target(name);
        let backup = mirror_backup_path(&self.backups, TS, &target);
        if let Some(parent) = backup.parent() {
            fs_err::create_dir_all(parent).expect("backup parent");
        }
        fs_err::write(&backup, original).expect("write backup");
        fs_err::write(&target, new_content).expect("write overwriting target");
        PlannedOperation::copy(format!("repo/{name}"), target.as_str(), Disposition::Update)
    }

    fn stage_fresh_created(&self, name: &str, content: &str) -> PlannedOperation {
        let target = self.target(name);
        if let Some(parent) = target.parent() {
            fs_err::create_dir_all(parent).expect("target parent");
        }
        fs_err::write(&target, content).expect("write fresh target");
        PlannedOperation::copy(format!("repo/{name}"), target.as_str(), Disposition::Create)
    }

    fn stage_fresh_unstarted(&self, name: &str) -> PlannedOperation {
        PlannedOperation::copy(
            format!("repo/{name}"),
            self.target(name).as_str(),
            Disposition::Create,
        )
    }

    fn stage_update_unstarted(&self, name: &str, original: &str) -> PlannedOperation {
        let target = self.target(name);
        fs_err::write(&target, original).expect("write pre-existing target");
        PlannedOperation::copy(format!("repo/{name}"), target.as_str(), Disposition::Update)
    }

    fn write_orphan_plan(&self, ops: Vec<PlannedOperation>) {
        let plan = Plan::new(ops);
        let bytes = plan.encode().expect("encode plan");
        fs_err::write(self.journal.join(format!("{TS}{PLAN_SUFFIX}")), bytes).expect("write plan");
    }

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

#[test]
fn restores_overwritten_targets_and_clears_orphan_files() {
    let scene = Scene::new();

    let ops = vec![
        scene.stage_overwrite("a", "orig-a", "new-a"),
        scene.stage_overwrite("b", "orig-b", "new-b"),
        scene.stage_overwrite("c", "orig-c", "new-c"),
        scene.stage_fresh_created("d", "new-d"),
        scene.stage_fresh_unstarted("e"),
    ];
    scene.write_orphan_plan(ops);
    scene.write_progress(&[0, 1, 2]);

    let report = recover_orphans(&scene.journal, &scene.backups).expect("recovery");
    assert_eq!(
        report.recovered_timestamps(),
        &[TS.to_owned()],
        "the single orphan plan is recovered"
    );

    for (name, original) in [("a", "orig-a"), ("b", "orig-b"), ("c", "orig-c")] {
        let got = fs_err::read_to_string(scene.target(name)).expect("read restored target");
        assert_eq!(got, original, "target {name} restored to pre-apply bytes");
    }
    assert!(
        !scene.target("d").exists(),
        "freshly-created target is removed, converging to pre-apply (absent)"
    );

    assert!(!scene.plan_exists(), "orphan plan removed");
    assert!(!scene.progress_exists(), "orphan progress removed");
}

#[test]
fn interrupted_before_any_op_touches_nothing_and_clears_orphan() {
    let scene = Scene::new();
    let ops = vec![
        scene.stage_fresh_unstarted("a"),
        scene.stage_fresh_unstarted("b"),
    ];
    scene.write_orphan_plan(ops);
    scene.write_progress(&[]);

    recover_orphans(&scene.journal, &scene.backups).expect("recovery");

    assert!(!scene.target("a").exists(), "no target was created");
    assert!(!scene.target("b").exists(), "no target was created");
    assert!(!scene.plan_exists(), "orphan plan removed");
    assert!(!scene.progress_exists(), "orphan progress removed");
}

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
        "the un-started op is NOT completed forward; it stays absent"
    );
}

#[test]
fn recovery_keeps_the_original_bytes_of_an_unstarted_update_target() {
    let scene = Scene::new();
    let ops = vec![
        scene.stage_fresh_created("created", "new"),
        scene.stage_update_unstarted("edited", "user-bytes"),
    ];
    scene.write_orphan_plan(ops);
    scene.write_progress(&[0]);

    recover_orphans(&scene.journal, &scene.backups).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(scene.target("edited")).ok(),
        Some("user-bytes".to_owned()),
        "an Update op the crashed apply never started must keep its pre-apply bytes"
    );
}

#[test]
fn recovery_ignores_and_removes_a_leftover_partial_backup() {
    let scene = Scene::new();
    let op = scene.stage_update_unstarted("edited", "live-bytes");
    let target = scene.target("edited");
    let backup = mirror_backup_path(&scene.backups, TS, &target);
    let staged = Utf8PathBuf::from(format!("{backup}.partial.4242"));
    fs_err::create_dir_all(staged.parent().expect("staged backup parent"))
        .expect("create backup parent");
    fs_err::write(&staged, "live-b").expect("write a torn staged backup");
    scene.write_orphan_plan(vec![op]);
    scene.write_progress(&[]);

    recover_orphans(&scene.journal, &scene.backups).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(&target).ok(),
        Some("live-bytes".to_owned()),
        "a staged backup is not a backup, so the target keeps its live bytes"
    );
    assert!(
        fs_err::symlink_metadata(&staged).is_err(),
        "recovery must remove the leftover staged backup"
    );
}

#[test]
fn recovery_restores_a_reaped_target_from_its_backup() {
    let scene = Scene::new();
    let target = scene.target("reaped");
    let backup = mirror_backup_path(&scene.backups, TS, &target);
    fs_err::create_dir_all(backup.parent().expect("backup parent")).expect("create backup parent");
    fs_err::write(&backup, "reaped-bytes").expect("write the reap's backup");
    scene.write_orphan_plan(vec![PlannedOperation::remove(target.as_str())]);
    scene.write_progress(&[0]);

    recover_orphans(&scene.journal, &scene.backups).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(&target).ok(),
        Some("reaped-bytes".to_owned()),
        "a Remove the crashed apply completed must be undone from its backup"
    );
}

#[test]
fn recovery_leaves_the_target_of_an_unstarted_remove() {
    let scene = Scene::new();
    let target = scene.target("kept");
    fs_err::write(&target, "kept-bytes").expect("write the reap target");
    scene.write_orphan_plan(vec![PlannedOperation::remove(target.as_str())]);
    scene.write_progress(&[]);

    recover_orphans(&scene.journal, &scene.backups).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(&target).ok(),
        Some("kept-bytes".to_owned()),
        "a Remove the crashed apply never started must leave its target"
    );
}

#[test]
fn lying_progress_cursor_is_ignored_in_favour_of_the_filesystem() {
    let scene = Scene::new();
    let ops = vec![
        scene.stage_overwrite("a", "orig-a", "new-a"),
        scene.stage_fresh_unstarted("b"),
    ];
    scene.write_orphan_plan(ops);
    scene.write_progress(&[0, 1]);

    recover_orphans(&scene.journal, &scene.backups).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(scene.target("a")).expect("read a"),
        "orig-a",
        "the genuinely-overwritten target is restored"
    );
    assert!(
        !scene.target("b").exists(),
        "the cursor's lie about op 1 does not cause a phantom restore/delete"
    );
    assert!(!scene.plan_exists(), "orphan plan removed");
}
