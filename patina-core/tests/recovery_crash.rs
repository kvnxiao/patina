//! Integration tests for recovery crash.

#![cfg(test)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::Disposition;
use patina_core::journal::PLAN_SUFFIX;
use patina_core::journal::PROGRESS_SUFFIX;
use patina_core::journal::Plan;
use patina_core::journal::PlannedOperation;
use patina_core::journal::RecoveryReport;
use patina_core::journal::mirror_backup_path;
use patina_core::journal::recover_orphans;
use tempfile::TempDir;

struct Scene {
    _temp: TempDir,
    root: Utf8PathBuf,
    journal: Utf8PathBuf,
    backups: Utf8PathBuf,
    home: Utf8PathBuf,
}

const TS: &str = "20260528T120000Z";

impl Scene {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        let journal = root.join("journal");
        let backups = root.join("backups");
        let home = root.join("home");
        for d in [&journal, &backups, &home] {
            fs_err::create_dir_all(d).expect("create scene dir");
        }
        Self {
            _temp: temp,
            root,
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

fn first_kept(report: &RecoveryReport) -> &Utf8Path {
    report
        .changed_targets()
        .first()
        .and_then(|changed| changed.kept().first())
        .expect("recovery kept the first entry it replaced")
}

fn kept_contents(report: &RecoveryReport) -> Vec<String> {
    report
        .changed_targets()
        .iter()
        .flat_map(patina_core::journal::RecoveredTarget::kept)
        .map(|kept| fs_err::read_to_string(kept).expect("read a reported copy"))
        .collect()
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

    let report = recover_orphans(&scene.root).expect("recovery");
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

    recover_orphans(&scene.root).expect("recovery");

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

    let first = recover_orphans(&scene.root).expect("first recovery");
    assert!(first.recovered_any(), "first pass reverses the orphan");

    let a_after_first = fs_err::read_to_string(scene.target("a")).expect("read a");
    let b_exists_after_first = scene.target("b").exists();

    let second = recover_orphans(&scene.root).expect("second recovery");
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

    recover_orphans(&scene.root).expect("recovery");

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

    recover_orphans(&scene.root).expect("recovery");

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

    recover_orphans(&scene.root).expect("recovery");

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

    recover_orphans(&scene.root).expect("recovery");

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

    recover_orphans(&scene.root).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(&target).ok(),
        Some("kept-bytes".to_owned()),
        "a Remove the crashed apply never started must leave its target"
    );
}

#[test]
fn recovery_keeps_bytes_written_after_the_crash_before_restoring_the_backup() {
    let scene = Scene::new();
    let op = scene.stage_overwrite("edited", "original", "written-after-the-crash");
    scene.write_orphan_plan(vec![op]);
    scene.write_progress(&[0]);

    let report = recover_orphans(&scene.root).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(scene.target("edited")).expect("read the restored target"),
        "original"
    );
    let [changed] = report.changed_targets() else {
        panic!("recovery changed one target: {report:?}");
    };
    assert_eq!(changed.target(), scene.target("edited"));
    let [kept] = changed.kept() else {
        panic!("a live entry is kept once before the restore: {changed:?}");
    };
    assert!(
        kept.starts_with(scene.root.join("recovered")),
        "the copy lives under <state>/recovered: {kept}"
    );
    assert_eq!(
        fs_err::read_to_string(kept).expect("read the kept copy"),
        "written-after-the-crash"
    );
}

#[test]
fn recovery_keeps_a_live_create_target_before_removing_it() {
    let scene = Scene::new();
    let op = scene.stage_fresh_created("created", "user-bytes");
    scene.write_orphan_plan(vec![op]);
    scene.write_progress(&[]);

    let report = recover_orphans(&scene.root).expect("recovery");

    assert!(
        !scene.target("created").exists(),
        "the Create target is removed"
    );
    let kept = first_kept(&report);
    assert_eq!(
        fs_err::read_to_string(kept).expect("read the kept copy"),
        "user-bytes"
    );
}

#[test]
fn a_retried_recovery_keeps_its_copies_beside_the_earlier_ones() {
    let scene = Scene::new();
    let op = scene.stage_fresh_created("created", "second-bytes");
    let earlier = scene.root.join("recovered").join(format!("{TS}.1"));
    fs_err::create_dir_all(earlier.join("0")).expect("seed an earlier copy directory");
    fs_err::write(earlier.join("0").join("created"), "first-bytes").expect("seed an earlier copy");
    scene.write_orphan_plan(vec![op]);
    scene.write_progress(&[]);

    let report = recover_orphans(&scene.root).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(earlier.join("0").join("created")).expect("read the earlier copy"),
        "first-bytes",
        "a later recovery must not overwrite an earlier copy"
    );
    assert_eq!(kept_contents(&report), ["first-bytes", "second-bytes"]);
}

#[test]
fn a_retry_over_a_partial_restore_reports_the_earlier_copy_first() {
    let scene = Scene::new();
    let op = scene.stage_overwrite("a", "original", "partial-restore");
    let earlier = scene.root.join("recovered").join(format!("{TS}.1"));
    fs_err::create_dir_all(earlier.join("0")).expect("seed an earlier copy directory");
    fs_err::write(earlier.join("0").join("a"), "user-bytes").expect("seed an earlier copy");
    scene.write_orphan_plan(vec![op]);
    scene.write_progress(&[0]);

    let report = recover_orphans(&scene.root).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(scene.target("a")).expect("read the restored target"),
        "original"
    );
    assert_eq!(kept_contents(&report), ["user-bytes", "partial-restore"]);
}

#[test]
fn recovery_of_an_absent_create_target_reports_no_change() {
    let scene = Scene::new();
    scene.write_orphan_plan(vec![scene.stage_fresh_unstarted("never")]);
    scene.write_progress(&[]);

    let report = recover_orphans(&scene.root).expect("recovery");

    assert!(report.recovered_any());
    assert!(report.changed_targets().is_empty(), "{report:?}");
    assert!(
        !scene.root.join("recovered").exists(),
        "no copy directory is created when nothing is kept"
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

    recover_orphans(&scene.root).expect("recovery");

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

#[test]
fn a_retry_after_a_failed_recovery_reports_the_copies_the_failed_pass_kept() {
    let scene = Scene::new();
    let removed = scene.stage_fresh_created("created", "user-created");
    let restored = scene.stage_overwrite("a", "orig-a", "user-a");
    let blocker = scene.home.join("blocker");
    fs_err::write(&blocker, "a file where recovery needs a directory").expect("write the blocker");
    let unreachable = blocker.join("x");
    let backup = mirror_backup_path(&scene.backups, TS, &unreachable);
    fs_err::create_dir_all(backup.parent().expect("backup parent")).expect("create backup parent");
    fs_err::write(&backup, "x-bytes").expect("write the reap's backup");
    scene.write_orphan_plan(vec![
        removed,
        restored,
        PlannedOperation::remove(unreachable.as_str()),
    ]);
    scene.write_progress(&[0, 1, 2]);

    let failed = recover_orphans(&scene.root);
    assert!(
        failed.is_err(),
        "a restore under a file must fail the pass: {failed:?}"
    );
    fs_err::remove_file(&blocker).expect("clear the blocker");
    let report = recover_orphans(&scene.root).expect("the retry recovers");

    let kept: Vec<(Utf8PathBuf, String)> = report
        .changed_targets()
        .iter()
        .flat_map(|changed| {
            changed.kept().iter().map(|kept| {
                let bytes = fs_err::read_to_string(kept).expect("read a reported copy");
                (changed.target().to_path_buf(), bytes)
            })
        })
        .collect();
    assert_eq!(
        kept,
        vec![
            (scene.target("created"), "user-created".to_owned()),
            (scene.target("a"), "user-a".to_owned()),
        ],
        "the retry must report the copies of the bytes the failed pass replaced"
    );
    let copies: Vec<String> = fs_err::read_dir(scene.root.join("recovered"))
        .expect("read the recovered copies")
        .map(|entry| {
            entry
                .expect("read a recovered copy")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        copies,
        vec![format!("{TS}.1")],
        "the retry must not copy the restored bytes again"
    );
}

#[test]
fn recovery_reports_and_keeps_only_the_targets_it_changed() {
    let scene = Scene::new();
    let ops = vec![
        scene.stage_overwrite("written", "orig-written", "new-written"),
        scene.stage_overwrite("unwritten", "drifted-bytes", "drifted-bytes"),
    ];
    scene.write_orphan_plan(ops);
    scene.write_progress(&[0]);

    let report = recover_orphans(&scene.root).expect("recovery");

    let changed: Vec<&Utf8Path> = report
        .changed_targets()
        .iter()
        .map(patina_core::journal::RecoveredTarget::target)
        .collect();
    assert_eq!(changed, vec![scene.target("written").as_path()]);
    let kept_ops: Vec<String> =
        fs_err::read_dir(scene.root.join("recovered").join(format!("{TS}.1")))
            .expect("read the copy directory")
            .map(|entry| {
                entry
                    .expect("read a copy entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
    assert_eq!(
        kept_ops,
        vec!["0".to_owned()],
        "only the written target is kept"
    );
    assert_eq!(
        fs_err::read_to_string(scene.target("unwritten")).expect("read the unwritten target"),
        "drifted-bytes"
    );
}

#[cfg(unix)]
fn link_dir(destination: &Utf8Path, link: &Utf8Path) {
    std::os::unix::fs::symlink(destination, link).expect("create a directory link");
}

#[cfg(windows)]
fn link_dir(destination: &Utf8Path, link: &Utf8Path) {
    std::os::windows::fs::symlink_dir(destination, link).expect("create a directory link");
}

#[test]
fn recovery_restores_a_stashed_root_link_instead_of_writing_through_it() {
    let scene = Scene::new();
    let stashed = scene.root.join("stashed");
    let current = scene.root.join("current");
    for (dir, bytes) in [(&stashed, "STASHED-F"), (&current, "CURRENT-F")] {
        fs_err::create_dir_all(dir).expect("mkdir a link destination");
        fs_err::write(dir.join("f"), bytes).expect("write a destination file");
    }
    fs_err::hard_link(current.join("f"), current.join("f.hard")).expect("hard-link the file");
    let app = scene.target("app");
    let backup = mirror_backup_path(&scene.backups, TS, &app);
    fs_err::create_dir_all(backup.parent().expect("backup parent")).expect("create backup parent");
    link_dir(&stashed, &backup);
    link_dir(&current, &app);
    scene.write_orphan_plan(vec![
        PlannedOperation::copy("repo/f", app.join("f").as_str(), Disposition::Update),
        PlannedOperation::copy("repo/app", app.as_str(), Disposition::Update),
    ]);
    scene.write_progress(&[]);

    recover_orphans(&scene.root).expect("recovery");

    assert_eq!(
        fs_err::read_link(&app).expect("read the restored link"),
        stashed.as_std_path()
    );
    assert_eq!(
        fs_err::read_to_string(stashed.join("f")).expect("read the stashed destination"),
        "STASHED-F"
    );
    let mut file = fs_err::OpenOptions::new()
        .append(true)
        .open(current.join("f"))
        .expect("open the current destination file");
    std::io::Write::write_all(&mut file, b"+").expect("append a marker");
    drop(file);
    assert_eq!(
        fs_err::read_to_string(current.join("f.hard")).expect("read the hard link"),
        "CURRENT-F+",
        "recovery must neither rewrite nor replace a file behind the live link"
    );
}
