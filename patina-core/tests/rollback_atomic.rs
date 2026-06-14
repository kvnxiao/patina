#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use .expect()/panic! on fixtures; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Per-`[[file]]`-entry atomic rollback coverage.
//!
//! A multi-target `[[file]]` entry reverts as a unit: when restoring one
//! target fails, every target the entry already reverted is rolled forward
//! to its post-apply state and `RollbackError::RollbackPartial` is returned,
//! so the entry is left atomically post-apply with no partial restore.
//!
//! The failure is injected without privileges: the second target's parent
//! directory is replaced by a *regular file*, so the restore's
//! `create_dir_all(parent)` fails with a deterministic, cross-platform IO
//! error — the harness's stand-in for a "permission error".

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::Disposition;
use patina_core::RollbackError;
use patina_core::journal::mirror_backup_path;
use patina_core::rollback::RevertTarget;
use patina_core::rollback::replay_entry;
use tempfile::TempDir;

/// A `Create` revert target for `path` (no backup → delete on revert).
fn create(path: &Utf8Path) -> RevertTarget<'_> {
    RevertTarget {
        target: path.as_str(),
        disposition: Disposition::Create,
    }
}

/// An `Update` revert target for `path` (backup → restore on revert).
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

/// Stash an "original" backup for `target` under `<backups>/<ts>/` so the
/// revert treats the target as a pre-existing overwrite to restore.
fn write_backup(backups: &Utf8Path, ts: &str, target: &Utf8Path, bytes: &[u8]) {
    let path = mirror_backup_path(backups, ts, target);
    fs_err::create_dir_all(path.parent().expect("backup parent")).expect("mkdir backup parent");
    fs_err::write(&path, bytes).expect("write backup");
}

#[test]
fn failed_second_target_rolls_the_first_forward_and_reports_partial() {
    let e = env();
    let ts = "20260528T120000Z";

    // Target 1: a normal pre-existing overwrite that can be reverted.
    let t1 = e.root.join("first");
    fs_err::write(&t1, "post-apply-1").expect("write t1 post-apply");
    write_backup(&e.backups, ts, &t1, b"original-1");

    // Target 2: also a recorded overwrite (a backup exists), but its parent
    // directory is occupied by a regular file, so restoring it fails when
    // the revert tries to create the parent chain.
    let blocked_parent = e.root.join("blocked");
    fs_err::write(&blocked_parent, "i am a file, not a dir").expect("occupy parent path");
    let t2 = blocked_parent.join("second");
    write_backup(&e.backups, ts, &t2, b"original-2");

    let result = replay_entry(3, &[update(&t1), update(&t2)], &e.backups, ts);

    // The entry fails atomically with a typed RollbackPartial naming entry 3.
    match result {
        Err(RollbackError::RollbackPartial { entry, .. }) => assert_eq!(entry, 3),
        other => panic!("expected RollbackPartial for entry 3, got {other:?}"),
    }

    // Target 1 was reverted then rolled forward: it is back to its
    // post-apply state, NOT the pre-apply backup. No partial restore.
    assert_eq!(
        fs_err::read_to_string(&t1).expect("read t1 after abort"),
        "post-apply-1",
        "the first target must be rolled forward to its post-apply state"
    );
}

#[test]
fn fully_revertible_multi_target_entry_succeeds() {
    // The happy-path counterpart: when every target reverts cleanly the
    // entry returns Ok and both reach their pre-apply state.
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
