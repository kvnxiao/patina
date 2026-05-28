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
}
