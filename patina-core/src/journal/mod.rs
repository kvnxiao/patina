//! Crash-safe plan journal: the binary plan file plus the per-operation
//! progress cursor.
//!
//! Before the engine mutates any file on disk it computes the full plan
//! (the list of file operations and hook invocations) and durably
//! records it to `<state>/patina/journal/<ts>.plan`. The plan file is
//! `postcard`-encoded and prefixed with a fixed-size version envelope so
//! a future format change can be detected and refused rather than
//! mis-decoded. The single up-front `fsync` of the plan file, paired
//! with an `fsync` of its parent directory, is the durability point that
//! lets a `kill -9` mid-apply converge deterministically on the next run.
//!
//! As each operation completes the engine appends a record to
//! `<state>/patina/journal/<ts>.progress`. The progress cursor is
//! advisory: it is written through to the kernel page cache but is
//! deliberately **not** `fsync`-ed per operation, because crash recovery
//! probes the real filesystem rather than trusting the cursor.
//! After every operation settles the engine writes and
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
//! recovery suite can substitute a recording fake that counts
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

    /// A journal record (the plan file or a commit sentinel) was shorter
    /// than the fixed-size version envelope, so no major version could be
    /// read.
    #[error(
        "journal record is truncated: {got} bytes, need at least {need} for the version envelope"
    )]
    Truncated {
        /// Bytes actually present in the file.
        got: usize,
        /// Bytes required to read the version envelope.
        need: usize,
    },

    /// The plan file declares a major format version newer than this
    /// binary understands. Refusing it is intentional: a forward-compat
    /// decode would silently misread the plan.
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
    /// the extraction.
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
    /// may begin mutating the filesystem.
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
    /// progress cursor. Deliberately **not** `fsync`-ed: crash
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
    /// and recovery will skip its timestamp.
    ///
    /// The sentinel body is the encoded `record`: crash recovery
    /// keys on the sentinel's *existence* and never decodes the body, so
    /// the payload is invisible to it; `patina status` reads the
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

/// Every committed-and-not-rolled-back `<ts>` in `dir`, sorted newest-first.
///
/// "Newest" is the lexically greatest `<ts>` prefix, which is chronological
/// for the compact UTC timestamp the engine writes. A `<ts>` carrying a
/// `ROLLED_BACK` sentinel beside its `COMMIT` is excluded: it has been
/// reversed and no longer describes the live filesystem.
///
/// Returning the full descending list (not just the maximum) is what lets
/// [`read_latest_commit_with_ts`] fall back to the previous commit when the
/// newest sentinel's body is unreadable.
///
/// # Errors
///
/// Returns [`JournalError::Filesystem`] if the journal directory cannot be
/// read.
fn unrolled_commit_timestamps(dir: &Utf8Path) -> Result<Vec<String>, JournalError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut timestamps = Vec::new();
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
        timestamps.push(timestamp.to_owned());
    }
    // Lexical-descending is chronological newest-first for the compact `<ts>`.
    timestamps.sort_unstable_by(|a, b| b.cmp(a));
    Ok(timestamps)
}

/// Read the most recent committed apply in `dir` whose record actually
/// decodes, paired with its `<ts>`, or `None` when no decodable un-rolled-back
/// commit remains.
///
/// The newest un-rolled-back `<ts>.COMMIT` is tried first. A sentinel that is
/// present but **unreadable** — a torn or empty body
/// ([`JournalError::Truncated`]) or a corrupt same-version body
/// ([`JournalError::Decode`]) — is skipped with a `warn!` and the scan falls
/// back to the next-older commit. This keeps `patina status` and `patina
/// rollback` working after a `kill -9` between creating a `<ts>.COMMIT` file
/// and flushing its bytes leaves a torn sentinel, rather than failing the
/// whole command on one bad record.
///
/// A sentinel from a **newer** format major ([`JournalError::VersionMismatch`])
/// is deliberately **not** skipped: it propagates. The version envelope exists
/// precisely so an older binary refuses a newer apply instead of acting on
/// stale state, so skipping it (and silently reporting or reverting an older
/// commit) would defeat that guard.
///
/// This single scan backs both readers of "the last apply": `patina status`
/// via [`read_latest_commit`] and `patina rollback`, so the two cannot
/// disagree on which commit is current.
///
/// # Errors
///
/// - [`JournalError::Filesystem`] if the directory or a sentinel cannot be
///   read.
/// - [`JournalError::VersionMismatch`] if the newest readable sentinel is from
///   a newer format than this binary supports.
pub(crate) fn read_latest_commit_with_ts(
    dir: &Utf8Path,
) -> Result<Option<(String, ApplyRecord)>, JournalError> {
    for timestamp in unrolled_commit_timestamps(dir)? {
        let commit_path = dir.join(format!("{timestamp}{COMMIT_SUFFIX}"));
        let bytes = fs_err::read(&commit_path)?;
        match ApplyRecord::decode(&bytes) {
            Ok(record) => return Ok(Some((timestamp, record))),
            // A torn/empty (`Truncated`) or corrupt same-version (`Decode`)
            // sentinel is unreadable: warn and fall back to the previous
            // commit. `VersionMismatch` is intentionally NOT matched here so
            // it flows to the propagating arm below — refusing a newer apply
            // is the whole point of the version envelope.
            Err(err @ (JournalError::Truncated { .. } | JournalError::Decode(_))) => {
                tracing::warn!(
                    timestamp = %timestamp,
                    error = %err,
                    "skipping an unreadable journal commit sentinel; \
                     falling back to the previous committed apply"
                );
            }
            Err(err) => return Err(err),
        }
    }
    Ok(None)
}

/// Read the [`ApplyRecord`] from the most recent decodable committed apply in
/// `dir`, or `None` when the directory holds no readable, un-rolled-back
/// `<ts>.COMMIT` sentinel (no apply has ever committed, every commit has since
/// been rolled back, or every remaining sentinel is torn or corrupt).
///
/// `patina status` is the reader: it decodes the latest apply's
/// recorded targets and classifies each against the live filesystem. This is
/// the `<ts>`-less convenience wrapper over the crate-internal
/// `read_latest_commit_with_ts`, which owns the torn-sentinel fallback and the
/// version-mismatch carve-out.
///
/// # Errors
///
/// - [`JournalError::Filesystem`] if the directory or sentinel cannot be read.
/// - [`JournalError::VersionMismatch`] if the newest readable sentinel is from
///   a newer binary.
pub fn read_latest_commit(dir: impl AsRef<Utf8Path>) -> Result<Option<ApplyRecord>, JournalError> {
    Ok(read_latest_commit_with_ts(dir.as_ref())?.map(|(_ts, record)| record))
}

/// Drop every journal sentinel for the `timestamps` whose backup cycles
/// have been garbage-collected.
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

    /// A minimal decodable record; the body is irrelevant to commit selection.
    fn sample_record() -> ApplyRecord {
        ApplyRecord::new(
            LastApply {
                at: "2026-05-28T12:00:00Z".to_owned(),
                user: "u".to_owned(),
                host: "h".to_owned(),
            },
            Vec::new(),
        )
    }

    /// Write a valid `<ts>.COMMIT` sentinel carrying an encoded record.
    fn write_commit(dir: &Utf8Path, ts: &str) {
        fs_err::write(
            dir.join(format!("{ts}{COMMIT_SUFFIX}")),
            sample_record().encode().expect("encode record"),
        )
        .expect("write commit sentinel");
    }

    #[test]
    fn read_latest_commit_is_none_on_missing_or_empty_dir() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        assert!(
            read_latest_commit(dir.join("nope"))
                .expect("a missing journal dir is a clean none")
                .is_none()
        );
        assert!(
            read_latest_commit(dir)
                .expect("an empty journal dir is a clean none")
                .is_none()
        );
    }

    #[test]
    fn read_latest_commit_picks_the_newest_committed_apply() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        write_commit(dir, "20260101T000000Z");
        write_commit(dir, "20260102T000000Z");
        let (ts, _record) = read_latest_commit_with_ts(dir)
            .expect("scan")
            .expect("a committed apply");
        assert_eq!(ts, "20260102T000000Z");
    }

    #[test]
    fn read_latest_commit_skips_a_rolled_back_apply_and_picks_the_prior() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        write_commit(dir, "20260101T000000Z");
        write_commit(dir, "20260102T000000Z");
        fs_err::write(
            dir.join(format!("20260102T000000Z{ROLLED_BACK_SUFFIX}")),
            [],
        )
        .expect("rolled-back sentinel");
        let (ts, _record) = read_latest_commit_with_ts(dir)
            .expect("scan")
            .expect("the prior committed apply");
        assert_eq!(ts, "20260101T000000Z");
    }

    #[test]
    fn read_latest_commit_is_none_when_every_apply_is_rolled_back() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        write_commit(dir, "20260101T000000Z");
        fs_err::write(
            dir.join(format!("20260101T000000Z{ROLLED_BACK_SUFFIX}")),
            [],
        )
        .expect("rolled-back sentinel");
        assert!(read_latest_commit(dir).expect("scan").is_none());
    }

    #[test]
    fn read_latest_commit_skips_a_torn_newest_sentinel_and_falls_back() {
        // Regression: a `kill -9` between creating the `<ts>.COMMIT` file and
        // flushing its bytes leaves a 0-byte sentinel. Reading the latest
        // commit must skip it and report the previous, decodable apply rather
        // than failing the whole `status` / `rollback` with a Truncated error.
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        write_commit(dir, "20260101T000000Z");
        // A newer sentinel exists but is torn (empty body).
        fs_err::write(dir.join(format!("20260102T000000Z{COMMIT_SUFFIX}")), [])
            .expect("torn sentinel");
        let (ts, _record) = read_latest_commit_with_ts(dir)
            .expect("a torn newest sentinel must not error the scan")
            .expect("the prior valid commit is returned");
        assert_eq!(ts, "20260101T000000Z");
    }

    #[test]
    fn read_latest_commit_is_none_when_the_only_sentinel_is_torn() {
        // The exact shape that bricked `patina status`: a lone 0-byte
        // `.COMMIT`. It must read as "no committed apply", not a hard error.
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        fs_err::write(dir.join(format!("20260102T000000Z{COMMIT_SUFFIX}")), [])
            .expect("torn sentinel");
        assert!(
            read_latest_commit(dir)
                .expect("a torn sole sentinel reads as none, not an error")
                .is_none()
        );
    }

    #[test]
    fn read_latest_commit_propagates_a_newer_major_sentinel() {
        // A sentinel from a newer format major must NOT be skipped: the
        // version envelope's purpose is to make this binary refuse a newer
        // apply rather than silently fall back to an older commit and act on
        // stale state.
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        write_commit(dir, "20260101T000000Z");
        let mut bytes = sample_record().encode().expect("encode");
        bytes
            .get_mut(..2)
            .expect("the encoded record has a 2-byte envelope")
            .copy_from_slice(&(FILE_MAJOR_VERSION + 1).to_le_bytes());
        fs_err::write(dir.join(format!("20260102T000000Z{COMMIT_SUFFIX}")), bytes)
            .expect("newer-major sentinel");
        assert!(matches!(
            read_latest_commit(dir),
            Err(JournalError::VersionMismatch { .. })
        ));
    }
}
