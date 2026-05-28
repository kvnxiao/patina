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
//! use patina_core::journal::{Journal, OsSyncer, Plan, PlannedOperation};
//!
//! let dir = Utf8Path::new("/var/state/patina/journal");
//! let plan = Plan::new(vec![PlannedOperation::symlink("src/a", "~/.a")]);
//! // Records and fsyncs the plan before the first mutation.
//! let handle = Journal::flush_plan_and_fsync(dir, "20260528T120000Z", &plan, &OsSyncer)?;
//! # let _ = handle;
//! # Ok::<(), patina_core::journal::JournalError>(())
//! ```

mod plan;
mod probe;
mod progress;
mod recovery;
mod sync;

use camino::Utf8Path;
use camino::Utf8PathBuf;
pub use plan::FILE_MAJOR_VERSION;
pub use plan::Plan;
pub use plan::PlannedOperation;
pub use probe::Probe;
pub use probe::classify_target;
pub use probe::mirror_backup_path;
pub use progress::ProgressCursor;
pub use recovery::ROLLED_BACK_SUFFIX;
pub use recovery::RecoveryReport;
pub use recovery::recover_orphans;
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

    /// Write `<ts>.COMMIT`, `fsync` it and the journal directory, then
    /// delete this run's plan and progress files. After this returns the
    /// apply is durably committed and recovery will skip its timestamp
    /// (REQ-011 `<behavior>`).
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Filesystem`] if any write, `fsync`, or
    /// delete fails.
    pub fn commit(self, syncer: &impl Syncer) -> Result<(), JournalError> {
        let commit_path = self.dir.join(format!("{}{COMMIT_SUFFIX}", self.timestamp));
        fs_err::write(&commit_path, [])?;
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
