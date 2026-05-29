//! Per-`[[file]]`-entry atomic inverse-operation replay (REQ-019).
//!
//! [`replay_entry`] reverts every target of one `[[file]]` entry to its
//! pre-apply state as an atomic unit. The inverse-operation rule mirrors
//! crash recovery: a target with a backup is restored from it (the apply
//! overwrote a pre-existing file); a target with no backup is deleted (the
//! apply created it fresh).
//!
//! ## Atomicity mechanism
//!
//! Before mutating any target the entry first **snapshots** each target's
//! current post-apply state into a temporary staging directory beside the
//! backup root. It then reverts the targets in order. If any revert fails,
//! every target reverted so far is rolled forward from its snapshot to the
//! post-apply state it had on entry, so the whole `[[file]]` entry is left
//! exactly as the last apply left it — no partial restore (REQ-019). The
//! staging directory is removed on both the success and failure paths.

use super::RollbackError;
use crate::journal::mirror_backup_path;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Revert every target in one `[[file]]` entry to its pre-apply state,
/// atomically: either all targets reach pre-apply state, or the entry is
/// rolled forward to its post-apply state and
/// [`RollbackError::RollbackPartial`] is returned.
///
/// `entry` is the entry's index (for the error message); `targets` are the
/// canonical absolute target paths the entry materialized, in apply order.
///
/// # Errors
///
/// - [`RollbackError::RollbackPartial`] when a target's revert fails; the entry
///   is restored to its post-apply state before returning.
/// - [`RollbackError::Filesystem`] when snapshotting itself fails before any
///   target has been mutated (nothing to undo).
pub fn replay_entry(
    entry: u32,
    targets: &[&str],
    backups_dir: &Utf8Path,
    timestamp: &str,
) -> Result<(), RollbackError> {
    // Stage each target's post-apply state so a mid-entry failure can be
    // rolled forward. The stage lives beside the backup root and is removed
    // on every exit path.
    let stage = stage_dir(backups_dir, timestamp, entry);
    fs_err::create_dir_all(&stage).map_err(RollbackError::Filesystem)?;

    let snapshots = match snapshot_targets(&stage, targets) {
        Ok(snapshots) => snapshots,
        Err(err) => {
            remove_stage(&stage);
            return Err(RollbackError::Filesystem(err));
        }
    };

    let mut reverted: Vec<&Snapshot> = Vec::with_capacity(snapshots.len());
    for snapshot in &snapshots {
        match revert_target(backups_dir, timestamp, &snapshot.target) {
            Ok(()) => reverted.push(snapshot),
            Err(source) => {
                // Roll forward to the post-apply state so the entry is left
                // atomically untouched. `revert_target` removes the in-flight
                // target before restoring it, so a copy failure can leave that
                // target deleted/partial — include it in the roll-forward set
                // alongside the already-reverted targets.
                reverted.push(snapshot);
                roll_forward(&reverted);
                remove_stage(&stage);
                return Err(RollbackError::RollbackPartial { entry, source });
            }
        }
    }

    remove_stage(&stage);
    Ok(())
}

/// A target's staged post-apply state: either a regular file's bytes
/// (staged to `staged_path`), a symlink's link target, or absent.
struct Snapshot {
    target: Utf8PathBuf,
    state: SnapshotState,
}

enum SnapshotState {
    /// The target was a regular file; its bytes are staged at this path.
    File(Utf8PathBuf),
    /// The target was a symbolic link pointing at this path.
    Symlink(Utf8PathBuf),
    /// The target did not exist at snapshot time.
    Absent,
}

/// Snapshot every target's current on-disk state into `stage`, returning one
/// [`Snapshot`] per target in order.
fn snapshot_targets(stage: &Utf8Path, targets: &[&str]) -> std::io::Result<Vec<Snapshot>> {
    let mut snapshots = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        let target = Utf8PathBuf::from(*target);
        let captured = match fs_err::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let raw = fs_err::read_link(&target)?;
                let link = Utf8PathBuf::from_path_buf(raw).map_err(|bad| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("non-UTF-8 symlink target: {}", bad.display()),
                    )
                })?;
                SnapshotState::Symlink(link)
            }
            Ok(meta) if meta.is_dir() => {
                // A directory target (symlink-dir restored, or a copy-tree
                // root) is staged by recursive copy so it can be rolled
                // forward verbatim.
                let staged = stage.join(format!("{index}.dir"));
                crate::fsx::copy_tree(&target, &staged)?;
                SnapshotState::File(staged)
            }
            Ok(_) => {
                let staged = stage.join(format!("{index}.file"));
                fs_err::copy(&target, &staged)?;
                SnapshotState::File(staged)
            }
            // A target whose parent is not a directory reports `ENOTDIR`
            // (`NotADirectory`) on Unix and `NotFound` on Windows; either way
            // the target genuinely cannot exist, so there is nothing to
            // snapshot. Treating both alike lets the real restore failure —
            // `create_dir_all` over the non-directory parent in
            // `revert_target` — drive the per-entry `RollbackPartial` path
            // identically on every platform.
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                SnapshotState::Absent
            }
            Err(err) => return Err(err),
        };
        snapshots.push(Snapshot {
            target,
            state: captured,
        });
    }
    Ok(snapshots)
}

/// Revert one target to its pre-apply state: restore from its backup if one
/// exists (overwrite case), otherwise delete it (fresh-creation case). This
/// is the same rule crash recovery applies.
fn revert_target(
    backups_dir: &Utf8Path,
    timestamp: &str,
    target: &Utf8Path,
) -> std::io::Result<()> {
    let backup = mirror_backup_path(backups_dir, timestamp, target);
    if crate::fsx::entry_present(&backup) {
        // Overwrite case: restore the original entry, kind-preserving — a
        // symlink comes back a symlink, a directory a directory, a file a
        // file. Presence is probed with `entry_present` rather than
        // `exists`, so a backed-up symlink whose destination is gone is
        // still restored instead of being mistaken for "no backup → delete".
        crate::fsx::clone_entry(&backup, target)
    } else {
        // Fresh-creation case: nothing was backed up, so reverting deletes
        // whatever the apply created.
        crate::fsx::remove_entry(target)
    }
}

/// Intentionally discard an IO result on a best-effort recovery path. The
/// entry is already being abandoned and there is no better state to
/// converge on than a best-effort restore, so a secondary failure here is
/// deliberately swallowed (and keeps the `must_use` lint satisfied without
/// a bare `let _`).
fn ignore_io<T>(_result: std::io::Result<T>) {}

/// Roll already-reverted targets forward to the post-apply state captured in
/// their snapshots, so a failed entry is left atomically untouched.
fn roll_forward(reverted: &[&Snapshot]) {
    for snapshot in reverted.iter().rev() {
        ignore_io(restore_snapshot(snapshot));
    }
}

/// Restore one target to the post-apply state captured in `snapshot`.
fn restore_snapshot(snapshot: &Snapshot) -> std::io::Result<()> {
    let target = &snapshot.target;
    ignore_io(crate::fsx::remove_entry(target));
    if let Some(parent) = target.parent()
        && !parent.as_str().is_empty()
    {
        fs_err::create_dir_all(parent)?;
    }
    match &snapshot.state {
        SnapshotState::File(staged) => {
            if fs_err::symlink_metadata(staged)?.is_dir() {
                crate::fsx::copy_tree(staged, target)
            } else {
                fs_err::copy(staged, target).map(|_| ())
            }
        }
        SnapshotState::Symlink(link) => crate::fsx::symlink_to(link, target),
        SnapshotState::Absent => Ok(()),
    }
}

/// The per-entry staging directory under the backup root.
fn stage_dir(backups_dir: &Utf8Path, timestamp: &str, entry: u32) -> Utf8PathBuf {
    backups_dir.join(format!(".rollback-stage-{timestamp}-{entry}"))
}

/// Remove the per-entry staging directory, swallowing errors: a leftover
/// stage is harmless and never read by any other code path.
fn remove_stage(stage: &Utf8Path) {
    ignore_io(fs_err::remove_dir_all(stage));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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

    /// Write a backup for `target` under `<backups>/<ts>/` so revert treats
    /// it as an overwrite to restore.
    fn write_backup(backups: &Utf8Path, ts: &str, target: &Utf8Path, bytes: &[u8]) {
        let path = mirror_backup_path(backups, ts, target);
        if let Some(parent) = path.parent() {
            fs_err::create_dir_all(parent).expect("mkdir backup parent");
        }
        fs_err::write(&path, bytes).expect("write backup");
    }

    #[test]
    fn fresh_creation_is_deleted() {
        let e = env();
        let ts = "TS";
        let target = e.root.join("created");
        fs_err::write(&target, b"new").expect("write target");

        replay_entry(0, &[target.as_str()], &e.backups, ts).expect("revert");
        assert!(!target.exists(), "a fresh creation must be deleted");
    }

    #[test]
    fn overwrite_is_restored_from_backup() {
        let e = env();
        let ts = "TS";
        let target = e.root.join("over");
        fs_err::write(&target, b"new").expect("write post-apply target");
        write_backup(&e.backups, ts, &target, b"original");

        replay_entry(0, &[target.as_str()], &e.backups, ts).expect("revert");
        assert_eq!(
            fs_err::read(&target).expect("read restored"),
            b"original",
            "an overwrite must be restored from its backup"
        );
    }

    #[test]
    fn multi_target_entry_reverts_every_target() {
        let e = env();
        let ts = "TS";
        let pre_existing = e.root.join("had-backup");
        let fresh = e.root.join("fresh");
        fs_err::write(&pre_existing, b"new").expect("write t1");
        fs_err::write(&fresh, b"new").expect("write t2");
        write_backup(&e.backups, ts, &pre_existing, b"original");

        replay_entry(7, &[pre_existing.as_str(), fresh.as_str()], &e.backups, ts)
            .expect("revert entry");

        assert_eq!(
            fs_err::read(&pre_existing).expect("read restored"),
            b"original"
        );
        assert!(!fresh.exists(), "the fresh target must be deleted");
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_of_a_pre_existing_symlink_is_restored_as_a_symlink() {
        // C1 regression at the rollback layer: a target that was a symlink
        // before the apply must revert to a symlink, not a regular file
        // holding the destination's bytes. The backup is the original
        // symlink (what `backup_before_overwrite` stashes), and its
        // destination need not exist for the revert to recreate the link.
        let e = env();
        let ts = "TS";
        let target = e.root.join("link-target");
        // Post-apply state: a regular file the apply wrote over the link.
        fs_err::write(&target, b"new").expect("write post-apply target");
        let backup = mirror_backup_path(&e.backups, ts, &target);
        fs_err::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir backup tree");
        fs_err::os::unix::fs::symlink("/original/dest", &backup).expect("stash original symlink");

        replay_entry(0, &[target.as_str()], &e.backups, ts).expect("revert");

        let meta = fs_err::symlink_metadata(&target).expect("stat reverted target");
        assert!(
            meta.file_type().is_symlink(),
            "a pre-existing symlink must revert to a symlink, not a regular file"
        );
        assert_eq!(
            fs_err::read_link(&target).expect("readlink reverted target"),
            std::path::Path::new("/original/dest")
        );
    }
}
