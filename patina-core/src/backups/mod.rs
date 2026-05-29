//! Pre-overwrite backups and count-based retention (REQ-014, REQ-015).
//!
//! Patina never clobbers a pre-existing user file without first stashing
//! the original. Before the executor overwrites any target that already
//! exists on disk — including replacing a regular file with a symlink —
//! it calls [`backup_before_overwrite`], which copies the original bytes
//! to `<state>/patina/backups/<ts>/<mirrored-target-path>`. The mirrored
//! path is the inverse map crash recovery (T-011) reads back, so both
//! agree on where an original lives; the mapping itself is owned by
//! [`crate::journal::mirror_backup_path`] and reused here verbatim
//! (REQ-014).
//!
//! A target that does *not* pre-exist produces no backup entry: there is
//! nothing to restore, and recovery's "no backup means fresh creation,
//! delete it" rule depends on the absence being meaningful.
//!
//! The backup tree lives entirely under the per-machine state directory.
//! Nothing in this module writes to the dotfiles repository (REQ-014's
//! "the repo is never written during apply" guarantee).
//!
//! ## Retention
//!
//! Backups accumulate one timestamped subdirectory per apply. After an
//! apply *commits* — i.e. once the `<ts>.COMMIT` sentinel is durable —
//! the engine calls [`gc_retain`] to keep only the [`RETENTION_COUNT`]
//! most recent cycles and remove the rest (REQ-015). Retention runs only
//! on a successful apply; a failed apply (no commit) never triggers GC,
//! so its caller simply does not invoke [`gc_retain`].
//!
//! There is no `patina gc` subcommand — this housekeeping is implicit and
//! has no CLI surface.
//!
//! # Examples
//!
//! ```no_run
//! use camino::Utf8Path;
//! use patina_core::backups::{backup_before_overwrite, gc_retain, RETENTION_COUNT};
//!
//! let backups = Utf8Path::new("/state/patina/backups");
//! // Before overwriting an existing target, stash the original.
//! backup_before_overwrite(backups, "20260528T120000Z", Utf8Path::new("/home/u/.zshrc"))?;
//! // After COMMIT, prune all but the ten newest cycles.
//! let removed = gc_retain(backups, RETENTION_COUNT)?;
//! # let _ = removed;
//! # Ok::<(), patina_core::backups::BackupError>(())
//! ```

mod mirror;
mod retention;

pub use mirror::backup_before_overwrite;
pub use retention::gc_retain;
use thiserror::Error;

/// Number of apply cycles whose backups are retained. The newest
/// [`RETENTION_COUNT`] timestamped subdirectories survive a [`gc_retain`]
/// pass; older ones are removed (REQ-015).
pub const RETENTION_COUNT: usize = 10;

/// Errors raised while taking a backup or running retention GC.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BackupError {
    /// A backup copy, directory read, or subdirectory removal failed. The
    /// wrapped `fs-err` error carries the offending path.
    #[error("backup filesystem operation failed")]
    Filesystem(#[from] std::io::Error),
}
