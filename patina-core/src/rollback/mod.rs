//! `patina rollback`: reverse the most recent committed apply via the
//! journal and backups (REQ-019, T-018).
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
//! `recover_orphans`): it is driven by backup *presence*, not the journaled
//! materialization kind.
//!
//! - A target with a backup under `<state>/patina/backups/<ts>/` is an
//!   *overwrite*: the apply replaced a pre-existing file, so the original bytes
//!   are restored from the backup.
//! - A target with no backup is a *fresh creation*: the apply created it from
//!   nothing, so reversing means deleting it.
//!
//! Either way the post-rollback state of each target matches the apply's
//! pre-apply state (REQ-019 `<done-when>`).
//!
//! ## Per-`[[file]]`-entry atomicity (REQ-019)
//!
//! A multi-target `[[file]]` entry reverts as an atomic unit: every target
//! in the entry reaches its pre-apply state, or the entry fails and every
//! target it already reverted is restored to its post-apply state, leaving
//! the entry untouched. This mirrors the all-or-nothing semantic the engine
//! applies per-entry during apply and crash recovery. The atomicity is
//! implemented in [`replay`] by snapshotting each target's post-apply state
//! before mutating, then rolling the snapshot back in on any failure.
//!
//! ## Locking (REQ-023)
//!
//! Rollback is mutating, so it takes the **exclusive** advisory lock for
//! its whole duration, exactly like apply.

mod replay;

use crate::error::EngineError;
use crate::journal::ApplyRecord;
use crate::journal::COMMIT_SUFFIX;
use crate::journal::OsSyncer;
use crate::journal::ROLLED_BACK_SUFFIX;
use crate::journal::Syncer;
use crate::lock::LockKind;
use crate::lock::acquire as acquire_lock;
use crate::lock::exclusive_timeout;
use crate::state_dir::resolve as resolve_state_dir;
use camino::Utf8Path;
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
    /// restore is left behind (REQ-019). The CLI surfaces this as exit
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

    let Some(timestamp) = latest_rollbackable(&journal_dir)? else {
        return Err(RollbackError::NoPriorApply.into());
    };

    let commit_path = journal_dir.join(format!("{timestamp}{COMMIT_SUFFIX}"));
    let bytes = fs_err::read(&commit_path).map_err(RollbackError::Filesystem)?;
    let record = ApplyRecord::decode(&bytes).map_err(RollbackError::Journal)?;

    reverse_record(&record, &backups_dir, &timestamp)?;
    mark_rolled_back(&journal_dir, &timestamp, &OsSyncer)?;
    Ok(())
}

/// Find the `<ts>` of the most recent committed apply that has not already
/// been rolled back, or `None` when no such apply exists.
///
/// "Most recent" is the lexically greatest `<ts>` prefix, which is
/// chronological for the compact UTC timestamp format the engine writes. A
/// `<ts>` with both a `COMMIT` and a `ROLLED_BACK` sentinel is skipped: it
/// has already been reversed, so rollback walks back to the prior one.
fn latest_rollbackable(journal_dir: &Utf8Path) -> Result<Option<String>, RollbackError> {
    if !journal_dir.exists() {
        return Ok(None);
    }

    let mut latest: Option<String> = None;
    for entry in fs_err::read_dir(journal_dir).map_err(RollbackError::Filesystem)? {
        let entry = entry.map_err(RollbackError::Filesystem)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(timestamp) = name.strip_suffix(COMMIT_SUFFIX) else {
            continue;
        };
        let already_rolled_back = journal_dir
            .join(format!("{timestamp}{ROLLED_BACK_SUFFIX}"))
            .exists();
        if already_rolled_back {
            continue;
        }
        if latest.as_deref().is_none_or(|current| timestamp > current) {
            latest = Some(timestamp.to_owned());
        }
    }
    Ok(latest)
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
struct EntryGroup<'a> {
    entry: u32,
    targets: Vec<&'a str>,
}

/// Group a record's targets by their `entry` index, preserving the order in
/// which entries (and targets within an entry) were applied. Consecutive
/// targets sharing an entry index belong to the same `[[file]]` entry and
/// revert as one atomic unit (REQ-019).
fn group_by_entry(record: &ApplyRecord) -> Vec<EntryGroup<'_>> {
    let mut groups: Vec<EntryGroup<'_>> = Vec::new();
    for expected in &record.targets {
        let entry = expected.entry();
        match groups.last_mut() {
            Some(last) if last.entry == entry => last.targets.push(expected.target()),
            _ => groups.push(EntryGroup {
                entry,
                targets: vec![expected.target()],
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
                fingerprint: 0,
                entry: 0,
            },
            ExpectedTarget::Content {
                target: "/b1".to_owned(),
                fingerprint: 0,
                entry: 1,
            },
            ExpectedTarget::Content {
                target: "/b2".to_owned(),
                fingerprint: 0,
                entry: 1,
            },
        ]);
        let groups = group_by_entry(&rec);
        let shape: Vec<(u32, Vec<&str>)> = groups
            .into_iter()
            .map(|group| (group.entry, group.targets))
            .collect();
        assert_eq!(
            shape,
            vec![(0, vec!["/a"]), (1, vec!["/b1", "/b2"])],
            "single-target entry 0 and two-target entry 1 group correctly"
        );
    }

    #[test]
    fn latest_rollbackable_is_none_on_missing_dir() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        assert!(
            latest_rollbackable(&root.join("nope"))
                .expect("missing dir is a clean none")
                .is_none()
        );
    }

    #[test]
    fn latest_rollbackable_skips_already_rolled_back_and_picks_prior() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        // Two committed applies; the newer is already rolled back.
        for ts in ["20260101T000000Z", "20260102T000000Z"] {
            fs_err::write(dir.join(format!("{ts}{COMMIT_SUFFIX}")), []).expect("commit");
        }
        fs_err::write(
            dir.join(format!("20260102T000000Z{ROLLED_BACK_SUFFIX}")),
            [],
        )
        .expect("rolled-back sentinel");

        assert_eq!(
            latest_rollbackable(dir).expect("scan"),
            Some("20260101T000000Z".to_owned())
        );
    }

    #[test]
    fn latest_rollbackable_is_none_when_all_rolled_back() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let ts = "20260101T000000Z";
        fs_err::write(dir.join(format!("{ts}{COMMIT_SUFFIX}")), []).expect("commit");
        fs_err::write(dir.join(format!("{ts}{ROLLED_BACK_SUFFIX}")), []).expect("rolled-back");
        assert!(latest_rollbackable(dir).expect("scan").is_none());
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
