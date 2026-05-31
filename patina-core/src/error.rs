//! Top-level engine error type returned from every public entry point in
//! [`crate`].
//!
//! [`EngineError`] aggregates one variant per failure domain (repository
//! discovery, module discovery, config parse, state directory, variables,
//! profile, template, path, journal, backup, lock, executor, hook,
//! rollback). Each wraps its subsystem's typed error via `#[from]`, so `?`
//! threads a subsystem failure up to the async entry points without
//! `todo!()` / `panic!()` (forbidden by REQ-024). The `non_exhaustive`
//! attribute keeps downstream `match` arms forward compatible.

use thiserror::Error;

/// Errors returned from [`apply`](fn@crate::apply),
/// [`status`](fn@crate::status), and [`rollback`](fn@crate::rollback).
///
/// Variants are added per task as their owning subsystems land. The
/// `non_exhaustive` attribute keeps downstream `match` arms forward
/// compatible.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    /// Repository-root resolution failed (REQ-003).
    #[error(transparent)]
    RepoDiscovery(#[from] crate::discovery::RepoDiscoveryError),

    /// Module enumeration failed (REQ-004).
    #[error(transparent)]
    ModuleDiscovery(#[from] crate::discovery::ModuleDiscoveryError),

    /// `[[file]]` / `[[hook]]` schema parse failed (REQ-005 / REQ-006).
    #[error(transparent)]
    ConfigParse(#[from] crate::config::ConfigParseError),

    /// Writing or editing a `patina.toml` manifest failed (SPEC-0002
    /// REQ-002 / REQ-003).
    #[error(transparent)]
    ConfigWrite(#[from] crate::config::ConfigWriteError),

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

    /// `patina rollback` failed to reverse a prior apply (REQ-019).
    #[error(transparent)]
    Rollback(#[from] crate::rollback::RollbackError),

    /// On Windows, the plan creates symbolic links but Developer Mode is
    /// disabled and the process is not elevated, so the engine refused to
    /// mutate the filesystem (SPEC-0002 REQ-007). This is the engine-side
    /// backstop: the CLI normally drives the one-time UAC elevation flow
    /// *before* calling `execute`, so this variant only surfaces when the
    /// gate is reached without that orchestration. The message names
    /// Developer Mode and `patina doctor --fix` so even the backstop path
    /// is actionable.
    #[error(
        "Developer Mode is disabled; creating symbolic links requires it. \
         Run `patina doctor --fix` to enable Developer Mode, or re-run \
         `patina apply` and accept the elevation prompt"
    )]
    DevModeRequired,
}
