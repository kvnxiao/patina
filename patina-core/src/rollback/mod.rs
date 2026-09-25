//! `patina rollback`: reverse the most recent committed apply via the
//! journal and backups.
//!
//! Rollback is the inverse of apply. It finds the most recent committed
//! apply that has not already been rolled back, and replays each operation's
//! inverse using the per-apply backup directory the apply stashed originals
//! in. It then marks the apply rolled back, so it no longer participates in
//! `patina status`'s "last apply" computation.
//!
//! ## Inverse-operation rule
//!
//! Rollback first restores the targets the apply's reap removed, then reverts
//! the materialized targets in reverse apply order. A reaped target with a
//! backup under `<state>/patina/backups/<ts>/` is restored from it, and a
//! reaped target without a backup is left alone.
//!
//! For a materialized target, the commit-recorded disposition is consulted
//! first, then backup *presence* decides between restore and delete. The
//! three outcomes, in evaluation order:
//!
//! - A target the apply recorded as `Unchanged` is *left in place*. The apply
//!   skipped both its write and its backup, so its live state is already the
//!   pre-apply state, and the backup is never consulted.
//! - A target with a backup is an *overwrite*. The apply replaced a
//!   pre-existing file, so rollback restores the original bytes from the
//!   backup.
//! - A target with no backup is a *fresh creation*. The apply created it from
//!   nothing, so reversing it means deleting it.
//!
//! Either way the post-rollback state of each target matches the apply's
//! pre-apply state.
//!
//! Crash recovery ([`crate::journal`]'s `recover_orphans`) differs for a
//! target with no backup whose disposition is `Update`. A committed apply
//! wrote every `Create` and `Update` target, so a missing backup means the
//! target was absent when the apply wrote it, and rollback deletes it. An
//! interrupted apply may never have reached the target, so recovery leaves it
//! in place.
//!
//! Before rollback replaces or deletes a live entry that differs from what the
//! record expects, it copies that entry to
//! `<state>/patina/recovered/<ts>.<n>/<index>/<file name>` and reports the
//! copy. The record expects a symlink to its recorded link target, a regular
//! file with its recorded hash, or, for a reaped target, nothing. `<index>` is
//! the target's position in the record's targets, or, for a reaped target, the
//! number of targets plus its position in the reaped list.
//!
//! ## Atomicity
//!
//! The reaped targets revert as one atomic unit, and so does each managed
//! entry. Either every target in the unit reaches its pre-apply state, or the
//! unit fails and every target it already reverted is rolled forward to its
//! post-apply state, leaving the unit untouched. The atomicity is implemented
//! in `replay` by snapshotting each target's post-apply state before mutating,
//! then rolling the snapshot back in on any failure.
//!
//! ## Locking
//!
//! Rollback is mutating, so it takes the **exclusive** advisory lock for
//! its whole duration, exactly like apply. Under that lock it first reverts any
//! interrupted apply, so no orphan plan is left to restore its backups over the
//! rolled-back state later. It then removes the staging directories that a
//! killed rollback left under `<state>/patina/backups/`.

mod replay;

use crate::error::EngineError;
use crate::journal::ApplyRecord;
use crate::journal::OsSyncer;
use crate::journal::ROLLED_BACK_SUFFIX;
use crate::journal::RecoveredTarget;
use crate::journal::RecoveryReport;
use crate::journal::Syncer;
use crate::journal::recover_orphans;
use crate::lock::LockKind;
use crate::lock::acquire as acquire_lock;
use crate::lock::exclusive_timeout;
use crate::state_dir::resolve as resolve_state_dir;
use camino::Utf8Path;
use replay::Replay;
pub(crate) use replay::replaced_root_ancestor;
pub(crate) use replay::stashed_link_ancestor;
use thiserror::Error;

/// Errors raised while rolling back a prior apply.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RollbackError {
    /// No committed apply remains to roll back. The journal holds no
    /// `<ts>.COMMIT` sentinel without a matching `<ts>.ROLLED_BACK`. The
    /// CLI surfaces this as exit code 1 with "no prior apply found".
    #[error("no prior apply found")]
    NoPriorApply,

    /// A multi-target managed entry could not be reverted as a unit. A
    /// target's restore or delete failed, and the entry's already-reverted
    /// targets were rolled forward to their post-apply state, so no partial
    /// restore is left behind. The CLI surfaces this as exit
    /// code 1.
    #[error(
        "rollback of managed entry {entry} failed and was reverted to its \
         post-apply state to preserve per-entry atomicity"
    )]
    RollbackPartial {
        /// Index of the managed entry whose rollback failed.
        entry: u32,
        /// The underlying filesystem error that triggered the abort.
        #[source]
        source: std::io::Error,
    },

    /// The targets the apply's reap removed could not be restored as a unit.
    /// A restore failed, and the targets already restored were rolled forward
    /// to their post-apply state. The CLI surfaces this as exit code 1.
    #[error(
        "restoring the targets the apply removed failed and was reverted, so \
         none of them was restored"
    )]
    ReapedPartial {
        /// The underlying filesystem error that triggered the abort.
        #[source]
        source: std::io::Error,
    },

    /// A filesystem operation outside an atomic region failed (reading the
    /// journal directory, removing a leftover staging directory, writing the
    /// rolled-back sentinel).
    #[error("rollback filesystem operation failed")]
    Filesystem(#[from] std::io::Error),

    /// Reading or decoding the committed apply record failed.
    #[error(transparent)]
    Journal(#[from] crate::journal::JournalError),
}

/// A step of [`run`] that the caller reports.
#[derive(Debug)]
#[non_exhaustive]
pub enum RollbackEvent<'a> {
    /// Recovery of interrupted applies, which runs before the rollback, has
    /// finished.
    Recovered(&'a RecoveryReport),
    /// The rollback copied the live entry at a target aside, and is about to
    /// replace or delete it.
    Kept(&'a RecoveredTarget),
    /// Rollback reached the managed state saved by remove or promote.
    Checkpoint,
}

/// Roll back the most recent committed apply to its pre-apply filesystem
/// state, using the journaled backups under `<state>/patina/backups/<ts>/`.
///
/// Resolves the state directory and takes the exclusive lock. Reverts every
/// interrupted apply first and passes the report to `on_event` before reading
/// the committed record, so `on_event` receives the report even when the
/// rollback then fails. Removes leftover staging directories, then finds the
/// most recent committed-and-not-rolled-back apply. Restores the targets that
/// its reap removed as one atomic unit, then replays each managed entry's
/// inverse operations, atomically per entry. Reports each copy it keeps of a
/// live entry to `on_event` before replacing or deleting that entry, including
/// a copy made before a later unit fails. Then writes and fsyncs a
/// `<ts>.ROLLED_BACK` sentinel. The apply therefore drops out of status's
/// last-apply computation, and recovery never re-reverses it.
/// If the latest record is a checkpoint, emit [`RollbackEvent::Checkpoint`]
/// without reversing targets or writing a rolled-back sentinel.
///
/// # Errors
///
/// - [`RollbackError::NoPriorApply`] when no committed apply remains.
/// - [`RollbackError::ReapedPartial`] when the reaped targets could not be
///   restored as a unit.
/// - [`RollbackError::RollbackPartial`] when a multi-target entry could not be
///   reverted as a unit.
/// - [`RollbackError::Filesystem`] / [`RollbackError::Journal`] for IO or
///   record-decode failures.
/// - [`EngineError::Journal`] when an interrupted apply cannot be recovered.
/// - An [`EngineError`] when state-directory resolution or lock acquisition
///   fails.
pub fn run(mut on_event: impl FnMut(RollbackEvent<'_>)) -> Result<(), EngineError> {
    let state_dir = resolve_state_dir()?;
    let journal_dir = state_dir.join("journal");
    let lock_path = state_dir.join("lock");

    // Mutating subcommands take the exclusive lock for the whole rollback.
    let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())?;
    on_event(RollbackEvent::Recovered(&recover_orphans(&state_dir)?));
    remove_stages(&state_dir.join("backups")).map_err(RollbackError::Filesystem)?;

    // The shared "last apply" selection (also used by `patina status`) skips a
    // torn/unreadable newest `<ts>.COMMIT` and falls back to the previous
    // decodable commit, so a damaged sentinel does not block rollback.
    // A newer-format sentinel still propagates (surfaced as
    // [`RollbackError::Journal`]) rather than being silently skipped.
    let Some((timestamp, record)) =
        crate::journal::read_latest_commit_with_ts(&journal_dir).map_err(RollbackError::Journal)?
    else {
        return Err(RollbackError::NoPriorApply.into());
    };

    if record.checkpoint {
        on_event(RollbackEvent::Checkpoint);
        return Ok(());
    }

    let mut on_kept = |kept: &RecoveredTarget| on_event(RollbackEvent::Kept(kept));
    reverse_record(&record, &state_dir, &timestamp, &mut on_kept)?;
    mark_rolled_back(&journal_dir, &timestamp, &OsSyncer)?;
    Ok(())
}

/// Restore the targets that `record`'s reap removed, then reverse every
/// managed entry recorded in `record`, one atomic entry at a time, in reverse
/// apply order so later entries are undone first.
pub(crate) fn reverse_record(
    record: &ApplyRecord,
    state_dir: &Utf8Path,
    timestamp: &str,
    on_kept: &mut dyn FnMut(&RecoveredTarget),
) -> Result<(), RollbackError> {
    let mut replay = Replay::new(state_dir, timestamp, on_kept)?;
    replay.reaped(&record.reaped, record.targets.len())?;
    let mut first_index = record.targets.len();
    for entry in record.targets.chunk_by(|a, b| a.entry() == b.entry()).rev() {
        first_index = first_index.saturating_sub(entry.len());
        replay.entry(entry, first_index)?;
    }
    Ok(())
}

/// Remove every staging directory that a rollback left under `backups_dir`. A
/// later rollback of the same apply would otherwise stage into a leftover
/// directory.
fn remove_stages(backups_dir: &Utf8Path) -> std::io::Result<()> {
    if !crate::fsx::entry_present(backups_dir) {
        return Ok(());
    }
    for entry in fs_err::read_dir(backups_dir)? {
        let name = entry?.file_name();
        if let Some(name) = name.to_str()
            && name.starts_with(replay::STAGE_PREFIX)
        {
            crate::fsx::remove_entry(&backups_dir.join(name))?;
        }
    }
    Ok(())
}

/// Write `<ts>.ROLLED_BACK`, fsync it and the journal directory so the
/// sentinel is durable. After this the `<ts>` is excluded from status's
/// last-apply computation and crash recovery treats it as closed.
fn mark_rolled_back(
    journal_dir: &Utf8Path,
    timestamp: &str,
    syncer: &impl Syncer,
) -> Result<(), RollbackError> {
    let sentinel = journal_dir.join(format!("{timestamp}{ROLLED_BACK_SUFFIX}"));
    fs_err::write(&sentinel, []).map_err(RollbackError::Filesystem)?;
    syncer.sync_file(&sentinel)?;
    syncer.sync_dir(journal_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Disposition;
    use crate::journal::ExpectedTarget;
    use crate::journal::LastApply;
    use tempfile::TempDir;

    fn record(targets: Vec<ExpectedTarget>) -> ApplyRecord {
        ApplyRecord::new(
            LastApply {
                at: "2026-05-28T12:00:00Z".to_owned(),
                user: "u".to_owned(),
                host: "h".to_owned(),
            },
            targets,
            Vec::new(),
        )
    }

    #[test]
    fn reverse_record_names_each_kept_copy_by_its_record_index() {
        let temp = TempDir::new().expect("tempdir");
        let state = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let ts = "20260101T000000Z";
        let home = state.join("home");
        fs_err::create_dir_all(&home).expect("mkdir home");
        let targets: Vec<ExpectedTarget> = ["a", "b1", "b2"]
            .into_iter()
            .zip([0, 1, 1])
            .map(|(name, entry)| {
                let target = home.join(name);
                fs_err::write(&target, b"edited").expect("edit a target after the apply");
                ExpectedTarget::Content {
                    target: target.to_string(),
                    source: format!("/repo/{name}"),
                    hash: crate::journal::content_hash(b"applied"),
                    entry,
                    disposition: Disposition::Create,
                }
            })
            .collect();
        let mut kept: Vec<camino::Utf8PathBuf> = Vec::new();

        reverse_record(&record(targets), state, ts, &mut |copy| {
            kept.extend(copy.kept().iter().cloned());
        })
        .expect("reverse the record");

        let pass = state
            .join(crate::journal::RECOVERED_DIR)
            .join(format!("{ts}.1"));
        kept.sort();
        assert_eq!(
            kept,
            [
                pass.join("0").join("a"),
                pass.join("1").join("b1"),
                pass.join("2").join("b2"),
            ]
        );
    }

    #[test]
    fn mark_rolled_back_writes_the_sentinel() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        mark_rolled_back(dir, "20260101T000000Z", &OsSyncer).expect("mark");
        assert!(
            dir.join(format!("20260101T000000Z{ROLLED_BACK_SUFFIX}"))
                .exists()
        );
    }

    #[test]
    fn remove_stages_removes_only_the_staging_directories() {
        let temp = TempDir::new().expect("tempdir");
        let backups = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let stage = backups.join(format!("{}20260101T000000Z-3", replay::STAGE_PREFIX));
        fs_err::create_dir_all(&stage).expect("mkdir a leftover stage");
        fs_err::write(stage.join("0.file"), b"stale").expect("stage a stale snapshot");
        let cycle = backups.join("20260101T000000Z");
        fs_err::create_dir_all(&cycle).expect("mkdir a backup cycle");

        remove_stages(backups).expect("remove the stages");

        assert!(!crate::fsx::entry_present(&stage));
        assert!(crate::fsx::entry_present(&cycle), "a backup cycle survives");
    }

    #[test]
    fn reverse_record_restores_a_reaped_target_from_its_backup() {
        let temp = TempDir::new().expect("tempdir");
        let state = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let ts = "20260101T000000Z";
        let target = state.join("home").join(".rc");
        let backup = crate::journal::mirror_backup_path(&state.join("backups"), ts, &target);
        fs_err::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir backup tree");
        fs_err::write(&backup, b"reaped-bytes").expect("write the reap's backup");
        let mut rec = record(Vec::new());
        rec.reaped = vec![target.to_string()];

        reverse_record(&rec, state, ts, &mut |_| {}).expect("reverse the record");

        assert_eq!(
            fs_err::read(&target).expect("read the restored target"),
            b"reaped-bytes"
        );
    }
}
