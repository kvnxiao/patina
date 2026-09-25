//! Crash-safe plan journal: the binary plan file plus the per-operation
//! progress cursor.
//!
//! Before the engine mutates any file on disk it computes the full plan
//! (the list of file operations and hook invocations) and durably
//! records it to `<state>/patina/journal/<ts>.plan`. The plan file is
//! `postcard`-encoded and prefixed with a fixed-size version envelope so
//! a future format change can be detected and refused rather than
//! mis-decoded. The single up-front `fsync` of the plan file is the
//! durability point, paired with an `fsync` of its parent directory. This
//! fsync ordering lets a `kill -9` mid-apply converge deterministically on
//! the next run.
//!
//! As each operation completes the engine appends a record to
//! `<state>/patina/journal/<ts>.progress`. The progress cursor is
//! advisory. It is written through to the kernel page cache but is
//! deliberately **not** `fsync`-ed per operation, because crash recovery
//! probes the real filesystem rather than trusting the cursor.
//! After every operation settles the engine writes and `fsync`s the
//! `<ts>.COMMIT` sentinel in a staged sibling, renames it into place, and only
//! then deletes the plan and progress files for that timestamp.
//!
//! ## Durability ordering
//!
//! ```text
//! 1. serialize plan -> <ts>.plan
//! 2. fsync <ts>.plan          ┐ both complete before any mutation
//! 3. fsync journal dir        ┘
//! 4. (engine mutates; appends to <ts>.progress, never fsync'd)
//! 5. write <ts>.COMMIT.partial.<pid>
//! 6. fsync <ts>.COMMIT.partial.<pid>
//! 7. rename it onto <ts>.COMMIT
//! 8. fsync journal dir
//! 9. delete <ts>.plan and <ts>.progress
//! ```
//!
//! A `<ts>.COMMIT` therefore always contains a whole record: a process killed
//! before the rename leaves only the staged sibling, the apply stays an orphan,
//! and recovery removes the sibling.
//!
//! The [`Syncer`] trait abstracts the two durability syscalls, `fsync` on a
//! file and `fsync` on a directory. The executor and the recovery suite can
//! therefore substitute a recording fake. That fake counts calls and asserts
//! the fsync shape without touching real hardware.
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
mod removal;
mod render;
mod sync;

use crate::error::chain_message;
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
pub(crate) use recovery::Keeper;
pub use recovery::RECOVERED_DIR;
pub use recovery::ROLLED_BACK_SUFFIX;
pub use recovery::RecoveredTarget;
pub use recovery::RecoveryReport;
pub use recovery::orphan_plans;
pub use recovery::recover_orphans;
pub use removal::Removal;
pub use removal::pending_removal;
pub use render::PlanRenderError;
pub use render::load_commit_file;
pub use render::load_plan_file;
pub use render::render_plan;
pub use render::render_record;
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
    #[error("failed to encode plan to postcard")]
    Encode(#[source] postcard::Error),

    /// The plan body could not be `postcard`-decoded.
    #[error("failed to decode plan from postcard")]
    Decode(#[source] postcard::Error),

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

    /// The newest operation ID has exhausted its sequence number.
    #[error("no journal operation ID follows {newest}")]
    TimestampExhausted {
        /// The newest ID in the journal, backups, or recovered directories.
        newest: String,
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
    /// `fsync` the plan file and the journal directory, in that order,
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

    /// Write the committed [`ApplyRecord`] to a staged sibling of
    /// `<ts>.COMMIT`, `fsync` it, rename it onto `<ts>.COMMIT`, and `fsync` the
    /// journal directory, then delete this run's plan and progress files.
    /// After this returns the apply is durably committed and recovery will
    /// skip its timestamp.
    ///
    /// The sentinel body is the encoded `record`. Crash recovery keys on the
    /// sentinel's *existence* and never decodes the body, so the payload is
    /// invisible to it. `patina status` reads the body to classify the live
    /// filesystem against the last apply.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Encode`] if the record cannot be encoded,
    /// or [`JournalError::Filesystem`] if any write, `fsync`, or delete
    /// fails.
    pub fn commit(self, record: &ApplyRecord, syncer: &impl Syncer) -> Result<(), JournalError> {
        write_commit_sentinel(&self.dir, &self.timestamp, record, syncer)?;

        // The plan and progress files are removed only after COMMIT is
        // durable. A crash between the two leaves a recoverable (plan,
        // no-commit) pair, rather than an orphan commit.
        remove_plan_and_progress(&self.dir, &self.timestamp)
    }

    /// Delete this run's backup cycle under `backups_dir`, then its plan and
    /// progress files, leaving the journal as recovery leaves it.
    ///
    /// Call this after reverting every operation the run performed. The cycle
    /// is kept when a committed apply shares this run's timestamp, because
    /// that apply's rollback reads it. A process killed before the plan is
    /// deleted leaves an orphan, which recovery reverts again: restoring a
    /// reverted target is idempotent, a reverted `Create` target is already
    /// absent, and any other target without a backup is left in place.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Filesystem`] if a delete fails.
    pub fn discard(self, backups_dir: &Utf8Path) -> Result<(), JournalError> {
        let committed = self.dir.join(format!("{}{COMMIT_SUFFIX}", self.timestamp));
        if !crate::fsx::entry_present(&committed) {
            crate::fsx::remove_entry(&backups_dir.join(&self.timestamp))?;
        }
        remove_plan_and_progress(&self.dir, &self.timestamp)
    }

    /// The journal directory this handle writes into.
    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    /// The `<ts>` timestamp shared by this run's plan, progress, and
    /// commit files.
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }
}

/// Write `record` to `<dir>/<timestamp>.COMMIT` through a staged sibling:
/// write and `fsync` the sibling, rename it onto the sentinel, then `fsync`
/// `dir`. A process killed before the rename leaves only the sibling.
fn write_commit_sentinel(
    dir: &Utf8Path,
    timestamp: &str,
    record: &ApplyRecord,
    syncer: &impl Syncer,
) -> Result<(), JournalError> {
    let commit_path = dir.join(format!("{timestamp}{COMMIT_SUFFIX}"));
    let staged = crate::fsx::partial_sibling(&commit_path);
    fs_err::write(&staged, record.encode()?)?;
    syncer.sync_file(&staged)?;
    crate::apply::with_staged_rename_retry(|| fs_err::rename(&staged, &commit_path))?;
    syncer.sync_dir(dir)?;
    Ok(())
}

/// Save a rollback checkpoint for `targets` and return its operation ID.
/// - The caller must hold the exclusive state lock.
/// - The checkpoint preserves the supplied expectations without target writes.
/// - Rollback stops at the checkpoint and keeps its managed set current.
/// - Allocation advances the operation ID without waiting for the clock.
/// - History pruning failures warn after the checkpoint commits.
///
/// # Errors
///
/// - [`JournalError::Encode`] if encoding fails.
/// - [`JournalError::TimestampExhausted`] if the sequence is exhausted.
/// - [`JournalError::Filesystem`] if reading state or publishing the commit
///   fails.
pub fn commit_record_only(
    state_dir: impl AsRef<Utf8Path>,
    targets: Vec<ExpectedTarget>,
    syncer: &impl Syncer,
) -> Result<String, JournalError> {
    let state_dir = state_dir.as_ref();
    let journal_dir = state_dir.join("journal");
    let timestamp = next_operation_id(state_dir)?;
    let record = checkpoint_record(targets);
    fs_err::create_dir_all(&journal_dir)?;
    write_commit_sentinel(&journal_dir, &timestamp, &record, syncer)?;
    retain_history(state_dir);
    Ok(timestamp)
}

fn checkpoint_record(targets: Vec<ExpectedTarget>) -> ApplyRecord {
    let builtins = crate::variables::Builtins::current();
    let mut record = ApplyRecord::new(
        LastApply {
            at: crate::clock::current_rfc3339(),
            user: builtins.user,
            host: builtins.hostname,
        },
        targets
            .into_iter()
            .map(|target| target.with_disposition(Disposition::Unchanged))
            .collect(),
        Vec::new(),
    );
    record.checkpoint = true;
    record
}

/// Choose an unused, ordered operation ID while holding the exclusive lock.
///
/// # Errors
///
/// Return an error if stored IDs cannot be read or the sequence is exhausted.
pub(crate) fn next_operation_id(state_dir: &Utf8Path) -> Result<String, JournalError> {
    operation_id_at(state_dir, &crate::clock::current_timestamp())
}

fn operation_id_at(state_dir: &Utf8Path, now: &str) -> Result<String, JournalError> {
    let mut newest: Option<String> = None;
    for dir in [
        state_dir.join("journal"),
        state_dir.join("backups"),
        state_dir.join(RECOVERED_DIR),
    ] {
        if !crate::fsx::entry_present(&dir) {
            continue;
        }
        for entry in fs_err::read_dir(dir)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
            if operation_id_parts(stem).is_some() && newest.as_deref().is_none_or(|n| stem > n) {
                newest = Some(stem.to_owned());
            }
        }
    }
    let Some(newest) = newest else {
        return Ok(format!("{now}-{:020}", 0));
    };
    if now > newest.as_str() {
        return Ok(format!("{now}-{:020}", 0));
    }
    let (date, sequence) =
        operation_id_parts(&newest).ok_or_else(|| JournalError::TimestampExhausted {
            newest: newest.clone(),
        })?;
    let next = sequence
        .checked_add(1)
        .ok_or_else(|| JournalError::TimestampExhausted {
            newest: newest.clone(),
        })?;
    Ok(format!("{date}-{next:020}"))
}

fn operation_id_parts(id: &str) -> Option<(&str, u64)> {
    let (date, sequence) = match id.split_once('-') {
        Some((date, sequence))
            if sequence.len() == 20 && sequence.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (date, sequence.parse().ok()?)
        }
        Some(_) => return None,
        None => (id, 0),
    };
    crate::clock::is_timestamp(date).then_some((date, sequence))
}

/// Prune committed history under the exclusive lock, warning on failure.
pub(crate) fn retain_history(state_dir: &Utf8Path) {
    if let Err(error) = prune_history(state_dir) {
        tracing::warn!(error = %chain_message(&error), "failed to prune journal history; the operation is committed");
    }
}

fn prune_history(state_dir: &Utf8Path) -> Result<(), JournalError> {
    let journal = state_dir.join("journal");
    if !journal.exists() {
        return Ok(());
    }
    let mut commits = Vec::new();
    for entry in fs_err::read_dir(&journal)? {
        let name = entry?.file_name();
        if let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_suffix(COMMIT_SUFFIX))
        {
            commits.push(id.to_owned());
        }
    }
    commits.sort();
    let removed = commits
        .len()
        .saturating_sub(crate::backups::RETENTION_COUNT);
    prune_cycles(&journal, commits.get(..removed).unwrap_or_default())?;
    let retained = commits.get(removed..).unwrap_or_default();
    let pending = orphan_plans(&journal)?;
    let backups = state_dir.join("backups");
    if backups.exists() {
        for entry in fs_err::read_dir(&backups)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name.to_str() else { continue };
            if operation_id_parts(id).is_some()
                && !retained.iter().any(|kept| kept == id)
                && !pending.iter().any(|kept| kept == id)
            {
                crate::fsx::remove_entry(&backups.join(id))?;
            }
        }
    }
    Ok(())
}

/// Every committed-and-not-rolled-back `<ts>` in `dir`, sorted newest-first.
///
/// "Newest" is the lexically greatest operation ID. A `<ts>` with a
/// `ROLLED_BACK` sentinel beside its `COMMIT` is excluded: it has been
/// reversed and no longer describes the live filesystem.
///
/// Returning the full descending list, not just the maximum, lets
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
    timestamps.sort_unstable_by(|a, b| b.cmp(a));
    Ok(timestamps)
}

/// Read the most recent committed apply in `dir` whose record actually
/// decodes, paired with its `<ts>`, or `None` when no decodable un-rolled-back
/// commit remains.
///
/// The newest un-rolled-back `<ts>.COMMIT` is tried first. A sentinel that is
/// present but **unreadable** is skipped with a `warn!`, and the scan falls
/// back to the next-older commit. Unreadable means a torn or empty body
/// ([`JournalError::Truncated`]), or a corrupt same-version body
/// ([`JournalError::Decode`]). [`Journal::commit`] never leaves such a
/// sentinel, so an unreadable sentinel was damaged outside the staged write.
/// Skipping it keeps `patina status` and `patina rollback` working rather than
/// failing the whole command on one bad record.
///
/// A sentinel from a **newer** format major ([`JournalError::VersionMismatch`])
/// is deliberately **not** skipped: it propagates. The version envelope exists
/// precisely so an older binary refuses a newer apply instead of acting on
/// stale state. Skipping it would silently report or revert an older commit,
/// and defeat that guard.
///
/// One scan backs both readers of "the last apply": `patina status`
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
            // it flows to the propagating arm below: refusing a newer apply
            // is the whole point of the version envelope.
            Err(err @ (JournalError::Truncated { .. } | JournalError::Decode(_))) => {
                tracing::warn!(
                    timestamp = %timestamp,
                    error = %chain_message(&err),
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
/// `<ts>.COMMIT` sentinel. That covers three cases: no apply has ever
/// committed, every commit has since been rolled back, or every remaining
/// sentinel is torn or corrupt.
///
/// `patina status` is the reader: it decodes the latest apply's
/// recorded targets and classifies each against the live filesystem. The
/// `<ts>`-less convenience wrapper calls the crate-internal
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

/// Delete plans and terminal sentinels for the supplied operation IDs.
/// - The caller must hold the exclusive state lock.
/// - Delete each plan before its sentinels so interruption cannot orphan it.
/// - Delete associated backups only after this call succeeds.
/// - Missing journal files are tolerated.
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
        remove_plan_and_progress(journal_dir, ts)?;
        remove_if_present(&journal_dir.join(format!("{ts}{COMMIT_SUFFIX}")))?;
        remove_if_present(&journal_dir.join(format!("{ts}{ROLLED_BACK_SUFFIX}")))?;
    }
    Ok(())
}

pub(super) fn remove_plan_and_progress(
    dir: &Utf8Path,
    timestamp: &str,
) -> Result<(), JournalError> {
    remove_if_present(&dir.join(format!("{timestamp}{PLAN_SUFFIX}")))?;
    remove_if_present(&dir.join(format!("{timestamp}{PROGRESS_SUFFIX}")))
}

/// Remove a file, treating an already-absent file as success.
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
    fn discard_keeps_a_backup_cycle_a_committed_apply_shares() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let journal_dir = root.join("journal");
        let backups_dir = root.join("backups");
        let ts = "20260528T120000Z";
        let shared = backups_dir.join(ts).join("home").join(".rc");
        fs_err::create_dir_all(shared.parent().expect("backup parent")).expect("mkdir cycle");
        fs_err::write(&shared, b"committed-backup").expect("seed the committed backup");
        let journal =
            Journal::flush_plan_and_fsync(&journal_dir, ts, &Plan::new(vec![]), &OsSyncer)
                .expect("flush the plan");
        write_commit(&journal_dir, ts);

        journal.discard(&backups_dir).expect("discard");

        assert_eq!(
            fs_err::read(&shared).expect("read the committed backup"),
            b"committed-backup"
        );
        assert!(!journal_dir.join(format!("{ts}{PLAN_SUFFIX}")).exists());
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
        // The exact shape that broke `patina status`: a lone 0-byte
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

    fn content(target: &str, disposition: Disposition) -> ExpectedTarget {
        ExpectedTarget::Content {
            target: target.to_owned(),
            source: format!("/repo{target}"),
            hash: content_hash(target.as_bytes()),
            entry: 0,
            disposition,
        }
    }

    #[test]
    fn operation_ids_advance_with_a_frozen_or_backward_clock() {
        let temp = TempDir::new().expect("tempdir");
        let state = Utf8Path::from_path(temp.path()).expect("utf8 state");
        let journal = state.join("journal");
        fs_err::create_dir_all(&journal).expect("create journal");
        let now = "20260101T000000Z";
        let first = operation_id_at(state, now).expect("first ID");
        fs_err::write(journal.join(format!("{first}.COMMIT")), []).expect("reserve first ID");
        let second = operation_id_at(state, now).expect("second ID");
        assert!(second > first);
        fs_err::write(journal.join(format!("{second}.ROLLED_BACK")), [])
            .expect("reserve second ID");
        let third = operation_id_at(state, "20250101T000000Z").expect("ID after clock rollback");
        assert!(third > second);
        fs_err::create_dir_all(state.join(RECOVERED_DIR).join(format!("{third}.1")))
            .expect("reserve recovered ID");
        assert!(operation_id_at(state, now).expect("ID after recovery") > third);
    }

    #[test]
    fn malformed_operation_ids_are_not_allocated_from() {
        let temp = TempDir::new().expect("tempdir");
        let state = Utf8Path::from_path(temp.path()).expect("utf8 state");
        let journal = state.join("journal");
        fs_err::create_dir_all(&journal).expect("create journal");
        for name in [
            "99990101T000000Z-short.COMMIT",
            "99990101T000000Z-99999999999999999999.COMMIT",
            "99990101T000000Z-0000000000000000000x.COMMIT",
        ] {
            fs_err::write(journal.join(name), []).expect("write malformed name");
        }
        assert!(
            operation_id_at(state, "20260101T000000Z")
                .expect("allocate ID")
                .starts_with("20260101T000000Z-")
        );
    }

    #[test]
    fn retention_preserves_pending_operations_and_newest_backups() {
        let temp = TempDir::new().expect("tempdir");
        let state = Utf8Path::from_path(temp.path()).expect("utf8 state");
        let journal = state.join("journal");
        fs_err::create_dir_all(&journal).expect("create journal");
        let mut ids = Vec::new();
        for i in 0..(crate::backups::RETENTION_COUNT + 2) {
            let id = format!("20260101T000000Z-{i:020}");
            write_commit(&journal, &id);
            fs_err::write(
                journal.join(format!("{id}.plan")),
                Plan::new(Vec::new()).encode().expect("encode plan"),
            )
            .expect("leave a committed plan");
            let backup = state.join("backups").join(&id);
            fs_err::create_dir_all(&backup).expect("create backup");
            fs_err::write(backup.join("payload"), b"preserved").expect("write backup");
            ids.push(id);
        }
        let pending = "20200101T000000Z";
        fs_err::write(journal.join(format!("{pending}.plan")), []).expect("write pending plan");
        fs_err::create_dir_all(state.join("backups").join(pending)).expect("create pending backup");
        prune_history(state).expect("prune history");
        assert!(state.join("backups").join(pending).exists());
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(journal.join(format!("{id}.COMMIT")).exists(), i >= 2);
            assert_eq!(state.join("backups").join(id).exists(), i >= 2);
        }
    }

    #[test]
    fn a_failed_plan_cleanup_keeps_the_commit_authoritative() {
        let temp = TempDir::new().expect("tempdir");
        let journal = Utf8Path::from_path(temp.path()).expect("utf8 journal");
        let id = "20260101T000000Z";
        write_commit(journal, id);
        fs_err::create_dir(journal.join(format!("{id}.plan"))).expect("block plan deletion");

        assert!(matches!(
            prune_cycles(journal, &[id.to_owned()]),
            Err(JournalError::Filesystem(_))
        ));

        assert!(
            read_latest_commit(journal)
                .expect("read retained commit")
                .is_some()
        );
        assert!(
            orphan_plans(journal)
                .expect("read pending plans")
                .is_empty()
        );
    }

    #[test]
    fn a_record_only_commit_records_every_target_unchanged_and_nothing_reaped() {
        let temp = TempDir::new().expect("tempdir");
        let state = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let targets = vec![
            content("/home/u/.a", Disposition::Create),
            content("/home/u/.b", Disposition::Update),
        ];

        let ts = commit_record_only(state, targets, &OsSyncer).expect("commit");

        let (latest, record) = read_latest_commit_with_ts(&state.join("journal"))
            .expect("scan")
            .expect("the record-only commit");
        assert_eq!(latest, ts);
        assert_eq!(
            record.targets,
            [
                content("/home/u/.a", Disposition::Unchanged),
                content("/home/u/.b", Disposition::Unchanged),
            ]
        );
        assert!(record.reaped.is_empty());
    }

    #[test]
    fn a_record_only_commit_after_the_last_representable_sequence_is_refused() {
        let temp = TempDir::new().expect("tempdir");
        let state = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        fs_err::create_dir_all(
            state
                .join("backups")
                .join("99991231T235959Z-18446744073709551615"),
        )
        .expect("mkdir a backup cycle");

        let result = commit_record_only(state, Vec::new(), &OsSyncer);

        assert!(
            matches!(result, Err(JournalError::TimestampExhausted { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_record_only_commit_follows_a_journal_or_backup_timestamp_the_clock_has_not_passed() {
        let temp = TempDir::new().expect("tempdir");
        let state = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let journal = state.join("journal");
        fs_err::create_dir_all(&journal).expect("mkdir journal");
        write_commit(&journal, "29990101T000000Z");
        fs_err::create_dir_all(state.join("backups").join("29990101T000005Z"))
            .expect("mkdir a backup cycle");

        let ts = commit_record_only(state, Vec::new(), &OsSyncer).expect("commit");

        assert_eq!(ts, "29990101T000005Z-00000000000000000001");
        let (latest, _record) = read_latest_commit_with_ts(&journal)
            .expect("scan")
            .expect("a commit");
        assert_eq!(latest, ts, "the record-only commit is the latest");
    }

    #[test]
    fn read_latest_commit_propagates_a_newer_major_sentinel() {
        // A sentinel from a newer format major must NOT be skipped. The
        // version envelope exists so this binary refuses a newer apply,
        // instead of silently falling back to an older, stale commit.
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
