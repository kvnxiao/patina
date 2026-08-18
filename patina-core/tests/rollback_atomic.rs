//! Integration tests for rollback atomic.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use .expect()/panic! on fixtures; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::Disposition;
use patina_core::RollbackError;
use patina_core::journal::mirror_backup_path;
use patina_core::rollback::RevertTarget;
use patina_core::rollback::replay_entry;
use tempfile::TempDir;

fn create(path: &Utf8Path) -> RevertTarget<'_> {
    RevertTarget {
        target: path.as_str(),
        disposition: Disposition::Create,
    }
}

fn update(path: &Utf8Path) -> RevertTarget<'_> {
    RevertTarget {
        target: path.as_str(),
        disposition: Disposition::Update,
    }
}

struct Env {
    _temp: TempDir,
    root: Utf8PathBuf,
    backups: Utf8PathBuf,
}

fn env() -> Env {
    let temp = TempDir::new().expect("tempdir");
    let root = Utf8Path::from_path(temp.path())
        .expect("utf8 temp path")
        .to_owned();
    let backups = root.join("backups");
    fs_err::create_dir_all(&backups).expect("mkdir backups");
    Env {
        _temp: temp,
        root,
        backups,
    }
}

fn write_backup(backups: &Utf8Path, ts: &str, target: &Utf8Path, bytes: &[u8]) {
    let path = mirror_backup_path(backups, ts, target);
    fs_err::create_dir_all(path.parent().expect("backup parent")).expect("mkdir backup parent");
    fs_err::write(&path, bytes).expect("write backup");
}

#[test]
fn failed_second_target_rolls_the_first_forward_and_reports_partial() {
    let e = env();
    let ts = "20260528T120000Z";

    let t1 = e.root.join("first");
    fs_err::write(&t1, "post-apply-1").expect("write t1 post-apply");
    write_backup(&e.backups, ts, &t1, b"original-1");

    let blocked_parent = e.root.join("blocked");
    fs_err::write(&blocked_parent, "i am a file, not a dir").expect("occupy parent path");
    let t2 = blocked_parent.join("second");
    write_backup(&e.backups, ts, &t2, b"original-2");

    let result = replay_entry(3, &[update(&t1), update(&t2)], &e.backups, ts);

    match result {
        Err(RollbackError::RollbackPartial { entry, .. }) => assert_eq!(entry, 3),
        other => panic!("expected RollbackPartial for entry 3, got {other:?}"),
    }

    assert_eq!(
        fs_err::read_to_string(&t1).expect("read t1 after abort"),
        "post-apply-1",
        "the first target must be rolled forward to its post-apply state"
    );
}

#[test]
fn fully_revertible_multi_target_entry_succeeds() {
    let e = env();
    let ts = "20260528T120000Z";

    let pre_existing = e.root.join("had-backup");
    let fresh = e.root.join("fresh");
    fs_err::write(&pre_existing, "post-apply").expect("write pre-existing");
    fs_err::write(&fresh, "post-apply").expect("write fresh");
    write_backup(&e.backups, ts, &pre_existing, b"original");

    replay_entry(0, &[update(&pre_existing), create(&fresh)], &e.backups, ts)
        .expect("entry reverts cleanly");

    assert_eq!(
        fs_err::read_to_string(&pre_existing).expect("read restored"),
        "original"
    );
    assert!(!fresh.exists(), "the fresh target must be deleted");
}
