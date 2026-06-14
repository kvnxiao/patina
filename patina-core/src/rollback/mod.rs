//! `patina rollback`: reverse the most recent committed apply via the
//! journal and backups.
//!
//! Rollback is the inverse of apply. It finds the most recent committed
//! apply that has not already been rolled back, replays each operation's
//! inverse using the per-apply backup directory the apply stashed
//! originals in, and marks the apply rolled back so it no longer
//! participates in `patina status`'s "last apply" computation.
//!
//! ## Inverse-operation rule
//!
//! The reversal decision mirrors crash recovery ([`crate::journal`]'s
//! `recover_orphans`): the commit-recorded per-target disposition is consulted
//! first, then backup *presence* decides between restore and delete. The three
//! outcomes, in evaluation order:
//!
//! - A target the apply recorded as `Unchanged` is *left in place*: the apply
//!   skipped both its write and its backup, so its live state already is the
//!   pre-apply state. The backup is never consulted.
//! - A target with a backup under `<state>/patina/backups/<ts>/` is an
//!   *overwrite*: the apply replaced a pre-existing file, so the original bytes
//!   are restored from the backup.
//! - A target with no backup is a *fresh creation*: the apply created it from
//!   nothing, so reversing means deleting it.
//!
//! Either way the post-rollback state of each target matches the apply's
//! pre-apply state.
//!
//! ## Per-`[[file]]`-entry atomicity
//!
//! A multi-target `[[file]]` entry reverts as an atomic unit: every target
//! in the entry reaches its pre-apply state, or the entry fails and every
//! target it already reverted is restored to its post-apply state, leaving
//! the entry untouched. This mirrors the all-or-nothing semantic the engine
//! applies per-entry during apply and crash recovery. The atomicity is
//! implemented in `replay` by snapshotting each target's post-apply state
//! before mutating, then rolling the snapshot back in on any failure.
//!
//! ## Locking
//!
//! Rollback is mutating, so it takes the **exclusive** advisory lock for
//! its whole duration, exactly like apply.

mod replay;

use crate::error::EngineError;
use crate::journal::ApplyRecord;
use crate::journal::OsSyncer;
use crate::journal::ROLLED_BACK_SUFFIX;
use crate::journal::Syncer;
use crate::lock::LockKind;
use crate::lock::acquire as acquire_lock;
use crate::lock::exclusive_timeout;
use crate::state_dir::resolve as resolve_state_dir;
use camino::Utf8Path;
pub use replay::RevertTarget;
pub use replay::replay_entry;
use thiserror::Error;

/// Errors raised while rolling back a prior apply.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RollbackError {
    /// No committed apply remains to roll back: the journal holds no
    /// `<ts>.COMMIT` sentinel without a matching `<ts>.ROLLED_BACK`. The
    /// CLI surfaces this as exit code 1 with "no prior apply found".
    #[error("no prior apply found")]
    NoPriorApply,

    /// A multi-target `[[file]]` entry could not be reverted as a unit: a
    /// target's restore/delete failed and the entry's already-reverted
    /// targets were rolled forward to their post-apply state, so no partial
    /// restore is left behind. The CLI surfaces this as exit
    /// code 1.
    #[error(
        "rollback of `[[file]]` entry {entry} failed and was reverted to its \
         post-apply state to preserve per-entry atomicity: {source}"
    )]
    RollbackPartial {
        /// Index of the `[[file]]` entry whose rollback failed.
        entry: u32,
        /// The underlying filesystem error that triggered the abort.
        #[source]
        source: std::io::Error,
    },

    /// A filesystem operation outside the per-entry atomic region failed
    /// (reading the journal directory, writing the rolled-back sentinel).
    #[error("rollback filesystem operation failed")]
    Filesystem(#[from] std::io::Error),

    /// Reading or decoding the committed apply record failed.
    #[error(transparent)]
    Journal(#[from] crate::journal::JournalError),
}

/// Roll back the most recent committed apply to its pre-apply filesystem
/// state, using the journaled backups under `<state>/patina/backups/<ts>/`.
///
/// Resolves the state directory, takes the exclusive lock, finds the most
/// recent committed-and-not-rolled-back apply, replays each `[[file]]`
/// entry's inverse operations (atomically per entry), then writes and
/// fsyncs a `<ts>.ROLLED_BACK` sentinel so the apply drops out of status's
/// last-apply computation and recovery never re-reverses it.
///
/// # Errors
///
/// - [`RollbackError::NoPriorApply`] when no committed apply remains.
/// - [`RollbackError::RollbackPartial`] when a multi-target entry could not be
///   reverted as a unit.
/// - [`RollbackError::Filesystem`] / [`RollbackError::Journal`] for IO or
///   record-decode failures.
/// - An [`EngineError`] when state-directory resolution or lock acquisition
///   fails.
pub fn run() -> Result<(), EngineError> {
    let state_dir = resolve_state_dir()?;
    let journal_dir = state_dir.join("journal");
    let backups_dir = state_dir.join("backups");
    let lock_path = state_dir.join("lock");

    // Mutating subcommands take the exclusive lock for the whole rollback.
    let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())?;

    // The shared "last apply" selection (also used by `patina status`) skips a
    // torn/unreadable newest `<ts>.COMMIT` and falls back to the previous
    // decodable commit, so a `kill -9`-torn sentinel does not strand rollback.
    // A newer-format sentinel still propagates (surfaced as
    // [`RollbackError::Journal`]) rather than being silently skipped.
    let Some((timestamp, record)) =
        crate::journal::read_latest_commit_with_ts(&journal_dir).map_err(RollbackError::Journal)?
    else {
        return Err(RollbackError::NoPriorApply.into());
    };

    reverse_record(&record, &backups_dir, &timestamp)?;
    mark_rolled_back(&journal_dir, &timestamp, &OsSyncer)?;
    Ok(())
}

/// Reverse every `[[file]]` entry recorded in `record`, one atomic entry at
/// a time, in reverse apply order so later entries are undone first.
fn reverse_record(
    record: &ApplyRecord,
    backups_dir: &Utf8Path,
    timestamp: &str,
) -> Result<(), RollbackError> {
    for group in group_by_entry(record).into_iter().rev() {
        replay_entry(group.entry, &group.targets, backups_dir, timestamp)?;
    }
    Ok(())
}

/// One `[[file]]` entry's recorded targets, grouped for atomic rollback.
/// Each target carries its commit-recorded disposition so [`replay_entry`]
/// can leave `Unchanged` targets in place.
struct EntryGroup<'a> {
    entry: u32,
    targets: Vec<RevertTarget<'a>>,
}

/// Group a record's targets by their `entry` index, preserving the order in
/// which entries (and targets within an entry) were applied. Consecutive
/// targets sharing an entry index belong to the same `[[file]]` entry and
/// revert as one atomic unit. Each target carries its recorded
/// disposition so an `Unchanged` target is left untouched on rollback.
fn group_by_entry(record: &ApplyRecord) -> Vec<EntryGroup<'_>> {
    let mut groups: Vec<EntryGroup<'_>> = Vec::new();
    for expected in &record.targets {
        let entry = expected.entry();
        let target = RevertTarget {
            target: expected.target(),
            disposition: expected.disposition(),
        };
        match groups.last_mut() {
            Some(last) if last.entry == entry => last.targets.push(target),
            _ => groups.push(EntryGroup {
                entry,
                targets: vec![target],
            }),
        }
    }
    groups
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
        )
    }

    #[test]
    fn group_by_entry_keeps_multi_target_entries_together() {
        let rec = record(vec![
            ExpectedTarget::Content {
                target: "/a".to_owned(),
                source: "/repo/a".to_owned(),
                hash: [0u8; 32],
                entry: 0,
                disposition: Disposition::Update,
            },
            ExpectedTarget::Content {
                target: "/b1".to_owned(),
                source: "/repo/b1".to_owned(),
                hash: [0u8; 32],
                entry: 1,
                disposition: Disposition::Update,
            },
            ExpectedTarget::Content {
                target: "/b2".to_owned(),
                source: "/repo/b2".to_owned(),
                hash: [0u8; 32],
                entry: 1,
                disposition: Disposition::Update,
            },
        ]);
        let groups = group_by_entry(&rec);
        let shape: Vec<(u32, Vec<&str>)> = groups
            .into_iter()
            .map(|group| {
                (
                    group.entry,
                    group.targets.iter().map(|t| t.target).collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![(0, vec!["/a"]), (1, vec!["/b1", "/b2"])],
            "single-target entry 0 and two-target entry 1 group correctly"
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
}
