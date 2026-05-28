//! Top-level engine error type returned from every public entry point in
//! [`crate`].
//!
//! Later phases extend [`EngineError`] with one variant per failure
//! domain (discovery, config parse, journal, lock, hooks, …). For now the
//! enum carries only the [`EngineError::NotImplemented`] placeholder so
//! the async entry points in [`crate`] have a typed return without
//! resorting to `todo!()` / `panic!()` (forbidden by REQ-024).

use thiserror::Error;

/// Errors returned from [`crate::apply`], [`crate::status`], and
/// [`crate::rollback`].
///
/// Variants are added per task as their owning subsystems land. The
/// `non_exhaustive` attribute keeps downstream `match` arms forward
/// compatible.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    /// Placeholder for entry points whose real implementation has not
    /// yet landed. Removed once every subsystem is wired through in
    /// later tasks.
    #[error("patina-core operation not yet implemented: {0}")]
    NotImplemented(&'static str),

    /// Repository-root resolution failed (REQ-003).
    #[error(transparent)]
    RepoDiscovery(#[from] crate::discovery::RepoDiscoveryError),

    /// Module enumeration failed (REQ-004).
    #[error(transparent)]
    ModuleDiscovery(#[from] crate::discovery::ModuleDiscoveryError),

    /// `[[file]]` / `[[hook]]` schema parse failed (REQ-005 / REQ-006).
    #[error(transparent)]
    ConfigParse(#[from] crate::config::ConfigParseError),

    /// Per-machine state directory resolution failed (REQ-016).
    #[error(transparent)]
    StateDir(#[from] crate::state_dir::StateDirError),

    /// Variable layer ingestion or CLI override parsing failed (REQ-007).
    #[error(transparent)]
    Variable(#[from] crate::variables::VariableError),

    /// Active-profile resolution failed (REQ-008).
    #[error(transparent)]
    Profile(#[from] crate::profile::ProfileError),

    /// Template rendering or `when` predicate evaluation failed under
    /// strict-undefined semantics (REQ-009).
    #[error(transparent)]
    Template(#[from] crate::template::TemplateError),

    /// Path canonicalization failed (REQ-010).
    #[error(transparent)]
    Path(#[from] crate::paths::PathError),

    /// Plan-journal write, read, or version check failed (REQ-011 /
    /// REQ-012).
    #[error(transparent)]
    Journal(#[from] crate::journal::JournalError),

    /// Pre-overwrite backup or retention GC failed (REQ-014 / REQ-015).
    #[error(transparent)]
    Backup(#[from] crate::backups::BackupError),

    /// Advisory file-lock acquisition timed out or failed (REQ-023).
    #[error(transparent)]
    Lock(#[from] crate::lock::LockError),

    /// A file-mode executor failed to materialize a source at a target
    /// (REQ-005).
    #[error(transparent)]
    Executor(#[from] crate::apply::ExecutorError),

    /// Hook shell resolution, `when` evaluation, or execution failed
    /// (REQ-006).
    #[error(transparent)]
    Hook(#[from] crate::apply::HookError),
}
