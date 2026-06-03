//! Crash-safe plan journal: the binary plan file plus the per-operation
//! progress cursor (REQ-011, REQ-012).
//!
//! Before the engine mutates any file on disk it computes the full plan
//! (the list of file operations and hook invocations) and durably
//! records it to `<state>/patina/journal/<ts>.plan`. The plan file is
//! `postcard`-encoded and prefixed with a fixed-size version envelope so
//! a future format change can be detected and refused rather than
//! mis-decoded. The single up-front `fsync` of the plan file, paired
//! with an `fsync` of its parent directory, is the durability point that
//! lets a `kill -9` mid-apply converge deterministically on the next run
//! (REQ-011).
//!
//! As each operation completes the engine appends a record to
//! `<state>/patina/journal/<ts>.progress`. The progress cursor is
//! advisory: it is written through to the kernel page cache but is
//! deliberately **not** `fsync`-ed per operation, because crash recovery
//! (T-011) probes the real filesystem rather than trusting the cursor
//! (REQ-012). After every operation settles the engine writes and
//! `fsync`s a `<ts>.COMMIT` sentinel, and only then deletes the plan and
//! progress files for that timestamp.
//!
//! ## Durability ordering
//!
//! ```text
//! 1. serialize plan -> <ts>.plan
//! 2. fsync <ts>.plan          ┐ both complete before any mutation
//! 3. fsync journal dir        ┘
//! 4. (engine mutates; appends to <ts>.progress, never fsync'd)
//! 5. write <ts>.COMMIT
//! 6. fsync <ts>.COMMIT
//! 7. fsync journal dir
//! 8. delete <ts>.plan and <ts>.progress
//! ```
//!
//! The [`Syncer`] trait abstracts the three durability syscalls
//! (`fsync` on a file, `fsync` on a directory) so the executor and the
//! T-011 recovery suite can substitute a recording fake that counts
//! calls and asserts the fsync shape without touching real hardware.
//!
//! # Examples
//!
//! ```no_run
//! use camino::Utf8Path;
//! use patina_core::Disposition;
//! use patina_core::journal::{Journal, OsSyncer, Plan, PlannedOperation};
//!
//! let dir = Utf8Path::new("/var/state/patina/journal");
//! let plan = Plan::new(vec![PlannedOperation::symlink("src/a", "~/.a", Disposition::Create)]);
//! // Records and fsyncs the plan before the first mutation.
//! let handle = Journal::flush_plan_and_fsync(dir, "20260528T120000Z", &plan, &OsSyncer)?;
//! # let _ = handle;
//! # Ok::<(), patina_core::journal::JournalError>(())
//! ```

mod disposition;
mod plan;
mod probe;
mod progress;
mod record;
mod recovery;
mod render;
mod sync;

use camino::Utf8Path;
use camino::Utf8PathBuf;
pub use disposition::Disposition;
pub use plan::FILE_MAJOR_VERSION;
pub use plan::Plan;
pub use plan::PlannedOperation;
pub use probe::Probe;
pub use probe::classify_target;
pub use probe::mirror_backup_path;
pub use progress::ProgressCursor;
pub use record::ApplyRecord;
pub use record::ExpectedTarget;
pub use record::LastApply;
pub use record::content_hash;
pub use record::read_symlink_target;
pub use record::timestamp_to_rfc3339;
pub use recovery::ROLLED_BACK_SUFFIX;
pub use recovery::RecoveryReport;
pub use recovery::recover_orphans;
pub use render::PlanRenderError;
pub use render::load_plan_file;
pub use render::render_plan;
pub use sync::OsSyncer;
pub use sync::Syncer;
use thiserror::Error;

/// Filename suffix for the binary plan file.
pub const PLAN_SUFFIX: &str = ".plan";
/// Filename suffix for the progress cursor.
pub const PROGRESS_SUFFIX: &str = ".progress";
/// Filename suffix for the commit sentinel.
pub const COMMIT_SUFFIX: &str = ".COMMIT";

/// Errors raised while reading or writing the plan journal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum JournalError {
    /// A plan file (or sentinel/cursor) could not be written, read, or
    /// removed. The wrapped `fs-err` error carries the offending path.
    #[error("journal filesystem operation failed")]
    Filesystem(#[from] std::io::Error),

    /// The plan body could not be `postcard`-encoded.
    #[error("failed to encode plan to postcard: {0}")]
    Encode(postcard::Error),

    /// The plan body could not be `postcard`-decoded.
    #[error("failed to decode plan from postcard: {0}")]
    Decode(postcard::Error),

    /// The plan file was shorter than the fixed-size version envelope, so
    /// no major version could be read.
    #[error("plan file is truncated: {got} bytes, need at least {need} for the version envelope")]
    Truncated {
        /// Bytes actually present in the file.
        got: usize,
        /// Bytes required to read the version envelope.
        need: usize,
    },

    /// The plan file declares a major format version newer than this
    /// binary understands. Refusing it is intentional: a forward-compat
    /// decode would silently misread the plan (REQ-011 version envelope).
    #[error(
        "journal plan major version {found} is newer than supported version {supported}; \
         upgrade patina to read this plan"
    )]
    VersionMismatch {
        /// Major version read from the plan file's envelope.
        found: u16,
        /// Highest major version this binary can decode.
        supported: u16,
    },
}

impl From<crate::version_envelope::EnvelopeError> for JournalError {
    /// Map the shared envelope codec's failure arms onto the journal's own
    /// error vocabulary so the journal's public error type is unchanged by
    /// the extraction (REQ-007).
    fn from(err: crate::version_envelope::EnvelopeError) -> Self {
        match err {
            crate::version_envelope::EnvelopeError::Truncated { got, need } => {
                Self::Truncated { got, need }
            }
            crate::version_envelope::EnvelopeError::VersionMismatch { found, supported } => {
                Self::VersionMismatch { found, supported }
            }
        }
    }
}

/// A live handle to the journal for one apply run, bound to its `<ts>`
/// and journal directory. Created by [`Journal::flush_plan_and_fsync`]
/// once the plan is durable; subsequent calls record progress and write
/// the commit sentinel.
#[derive(Debug)]
#[must_use = "the journal handle owns the commit sentinel; dropping it without commit leaves an orphan plan"]
pub struct Journal {
    dir: Utf8PathBuf,
    timestamp: String,
    progress: ProgressCursor,
}

impl Journal {
    /// Serialize `plan`, write it to `<dir>/<timestamp>.plan`, then
    /// `fsync` the plan file and the journal directory — in that order —
    /// before returning. On return the plan is durable and the engine
    /// may begin mutating the filesystem (REQ-011).
    ///
    /// The journal directory is created if it does not yet exist (the
    /// `state_dir` module also creates it; this call is idempotent).
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Encode`] if the plan cannot be serialized,
    /// or [`JournalError::Filesystem`] if any write or `fsync` fails.
    pub fn flush_plan_and_fsync(
        dir: impl AsRef<Utf8Path>,
        timestamp: impl Into<String>,
        plan: &Plan,
        syncer: &impl Syncer,
    ) -> Result<Self, JournalError> {
        let dir = dir.as_ref();
        let timestamp = timestamp.into();
        fs_err::create_dir_all(dir)?;

        let plan_path = dir.join(format!("{timestamp}{PLAN_SUFFIX}"));
        let bytes = plan.encode()?;
        fs_err::write(&plan_path, &bytes)?;

        // The durability point: plan file first, then its parent dir, so
        // the directory entry pointing at the plan is itself durable.
        syncer.sync_file(&plan_path)?;
        syncer.sync_dir(dir)?;

        let progress = ProgressCursor::create(dir, &timestamp)?;
        Ok(Self {
            dir: dir.to_owned(),
            timestamp,
            progress,
        })
    }

    /// Append a completion record for operation index `op_index` to the
    /// progress cursor. Deliberately **not** `fsync`-ed (REQ-012): crash
    /// recovery probes the filesystem rather than trusting this cursor.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Filesystem`] if the append fails.
    pub fn record_progress(&mut self, op_index: u32) -> Result<(), JournalError> {
        self.progress.record(op_index)
    }

    /// Write `<ts>.COMMIT` carrying the committed [`ApplyRecord`], `fsync`
    /// it and the journal directory, then delete this run's plan and
    /// progress files. After this returns the apply is durably committed
    /// and recovery will skip its timestamp (REQ-011 `<behavior>`).
    ///
    /// The sentinel body is the encoded `record`: crash recovery (T-011)
    /// keys on the sentinel's *existence* and never decodes the body, so
    /// the payload is invisible to it; `patina status` (T-017) reads the
    /// body to classify the live filesystem against the last apply.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Encode`] if the record cannot be encoded,
    /// or [`JournalError::Filesystem`] if any write, `fsync`, or delete
    /// fails.
    pub fn commit(self, record: &ApplyRecord, syncer: &impl Syncer) -> Result<(), JournalError> {
        let commit_path = self.dir.join(format!("{}{COMMIT_SUFFIX}", self.timestamp));
        fs_err::write(&commit_path, record.encode()?)?;
        syncer.sync_file(&commit_path)?;
        syncer.sync_dir(&self.dir)?;

        // The plan and progress files are removed only after COMMIT is
        // durable, so a crash between the two leaves a recoverable
        // (plan, no-commit) pair rather than an orphan commit.
        let plan_path = self.dir.join(format!("{}{PLAN_SUFFIX}", self.timestamp));
        let progress_path = self
            .dir
            .join(format!("{}{PROGRESS_SUFFIX}", self.timestamp));
        remove_if_present(&plan_path)?;
        remove_if_present(&progress_path)?;
        Ok(())
    }

    /// The journal directory this handle writes into.
    #[must_use = "the journal directory locates the plan, progress, and commit files"]
    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    /// The `<ts>` timestamp shared by this run's plan, progress, and
    /// commit files.
    #[must_use = "the timestamp keys this run's journal files"]
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }
}

/// The `<ts>` of the most recent committed apply in `dir` that has not also
/// been rolled back, or `None` when none exists.
///
/// "Most recent" is the lexically greatest `<ts>` prefix, which is
/// chronological for the compact UTC timestamp the engine writes. A `<ts>`
/// carrying a `ROLLED_BACK` sentinel beside its `COMMIT` (T-018) is skipped:
/// it has been reversed and no longer describes the live filesystem.
///
/// This single scan backs both readers of "the last apply": `patina status`
/// via [`read_latest_commit`] (which then decodes the winner's record) and
/// `patina rollback` (which reverts it), so the two cannot disagree on which
/// commit is current.
///
/// # Errors
///
/// Returns [`JournalError::Filesystem`] if the journal directory cannot be
/// read.
pub(crate) fn latest_unrolled_commit(dir: &Utf8Path) -> Result<Option<String>, JournalError> {
    if !dir.exists() {
        return Ok(None);
    }

    let mut latest: Option<String> = None;
    for entry in fs_err::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(timestamp) = name.strip_suffix(COMMIT_SUFFIX) else {
            continue;
        };
        if dir
            .join(format!("{timestamp}{ROLLED_BACK_SUFFIX}"))
            .exists()
        {
            continue;
        }
        if latest.as_deref().is_none_or(|current| timestamp > current) {
            latest = Some(timestamp.to_owned());
        }
    }
    Ok(latest)
}

/// Read the [`ApplyRecord`] from the most recent committed apply in `dir`,
/// or `None` when the directory holds no live `<ts>.COMMIT` sentinel (no
/// apply has ever committed, or every commit has since been rolled back).
///
/// `patina status` (T-017) is the reader: it decodes the latest apply's
/// recorded targets and classifies each against the live filesystem. The
/// "most recent un-rolled-back `<ts>`" selection is shared with rollback via
/// the crate-internal `latest_unrolled_commit`.
///
/// # Errors
///
/// - [`JournalError::Filesystem`] if the directory or sentinel cannot be read.
/// - [`JournalError::Truncated`] / [`JournalError::VersionMismatch`] /
///   [`JournalError::Decode`] if the latest sentinel cannot be decoded (a
///   sentinel from a newer binary, or a corrupt one).
pub fn read_latest_commit(dir: impl AsRef<Utf8Path>) -> Result<Option<ApplyRecord>, JournalError> {
    let dir = dir.as_ref();
    let Some(timestamp) = latest_unrolled_commit(dir)? else {
        return Ok(None);
    };
    let commit_path = dir.join(format!("{timestamp}{COMMIT_SUFFIX}"));
    let bytes = fs_err::read(&commit_path)?;
    Ok(Some(ApplyRecord::decode(&bytes)?))
}

/// Drop every journal sentinel for the `timestamps` whose backup cycles
/// have been garbage-collected (REQ-015).
///
/// After [`backups::gc_retain`](crate::backups::gc_retain) prunes an apply's
/// backup directory, that apply can no longer be faithfully reversed — the
/// original bytes its overwrites would restore are gone — so rolling back to
/// it would *delete* targets it can no longer restore. Removing the
/// `<ts>.COMMIT` (and any `<ts>.ROLLED_BACK`) sentinel drops the apply from
/// both `patina status`'s "last apply" search ([`read_latest_commit`]) and
/// `patina rollback`'s walk-back, so a commit and its backups are retained
/// or vanish as one unit.
///
/// Only timestamps whose backup *directory* was pruned are passed here. An
/// all-fresh apply (no overwrites) writes no backup directory, so it is
/// never pruned and remains rollbackable — and rolling back to it correctly
/// deletes its fresh-created targets, with nothing to restore. Absent
/// sentinels are tolerated, so the call is idempotent.
///
/// # Errors
///
/// Returns [`JournalError::Filesystem`] if a sentinel cannot be removed for
/// a reason other than already being absent.
pub fn prune_cycles(
    journal_dir: impl AsRef<Utf8Path>,
    timestamps: &[String],
) -> Result<(), JournalError> {
    let journal_dir = journal_dir.as_ref();
    for ts in timestamps {
        remove_if_present(&journal_dir.join(format!("{ts}{COMMIT_SUFFIX}")))?;
        remove_if_present(&journal_dir.join(format!("{ts}{ROLLED_BACK_SUFFIX}")))?;
        // The plan and progress files are normally deleted at commit;
        // remove them defensively so a pruned cycle leaves nothing behind.
        remove_if_present(&journal_dir.join(format!("{ts}{PLAN_SUFFIX}")))?;
        remove_if_present(&journal_dir.join(format!("{ts}{PROGRESS_SUFFIX}")))?;
    }
    Ok(())
}

/// Remove a file, treating an already-absent file as success. Used on the
/// commit path where a prior partial run may have removed one of the
/// pair already, and by the `recovery` sibling when cleaning up orphan
/// plan + progress files.
pub(super) fn remove_if_present(path: &Utf8Path) -> Result<(), JournalError> {
    match fs_err::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(JournalError::Filesystem(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn prune_cycles_drops_commit_and_rolled_back_sentinels_for_the_named_timestamps() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        fs_err::write(dir.join(format!("OLD{COMMIT_SUFFIX}")), b"x").expect("old commit");
        fs_err::write(dir.join(format!("OLD{ROLLED_BACK_SUFFIX}")), b"x").expect("old rolled-back");
        fs_err::write(dir.join(format!("NEW{COMMIT_SUFFIX}")), b"x").expect("new commit");

        prune_cycles(dir, &["OLD".to_owned()]).expect("prune the old cycle");

        assert!(
            !dir.join(format!("OLD{COMMIT_SUFFIX}")).exists(),
            "the pruned cycle's commit sentinel must be gone so it is no longer rollback-eligible"
        );
        assert!(
            !dir.join(format!("OLD{ROLLED_BACK_SUFFIX}")).exists(),
            "the pruned cycle's rolled-back sentinel must be gone too"
        );
        assert!(
            dir.join(format!("NEW{COMMIT_SUFFIX}")).exists(),
            "a retained cycle's sentinel must survive"
        );
    }

    #[test]
    fn prune_cycles_tolerates_absent_sentinels() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        // A timestamp with no sentinels at all (the all-fresh-apply shape, or
        // a partially pruned cycle) is a clean no-op.
        prune_cycles(dir, &["GHOST".to_owned()]).expect("absent sentinels are tolerated");
    }
}
