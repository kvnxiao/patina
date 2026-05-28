//! Core library for the patina cross-platform dotfile manager.
//!
//! The three public async entry points — [`apply`], [`status`], and
//! [`rollback`] — define the engine's outer contract. They return
//! [`Result<_, EngineError>`](EngineError); the CLI wraps that into
//! `anyhow::Result` at the call site (per the project rule that
//! `anyhow` lives only in the binary).
//!
//! T-001 lands the async signatures and the [`EngineError`] enum.
//! Subsystem wiring (discovery, journal, executors, hooks, lock, …)
//! lands in subsequent tasks.

pub mod config;
pub mod discovery;
pub mod error;
pub mod journal;
pub mod paths;
pub mod profile;
pub mod state_dir;
pub mod template;
pub mod variables;

pub use config::ConfigParseError;
pub use config::FileEntry;
pub use config::FileMode;
pub use config::HookEntry;
pub use config::HookEvent;
pub use config::ModuleConfig;
pub use config::parse_module_config;
pub use discovery::ModuleDiscoveryError;
pub use discovery::ModuleHandle;
pub use discovery::RepoDiscoveryError;
pub use discovery::discover_modules;
pub use discovery::resolve_repository_root;
pub use error::EngineError;
pub use journal::Journal;
pub use journal::JournalError;
pub use journal::Plan;
pub use journal::PlannedOperation;
pub use paths::PathError;
pub use paths::canonicalize as canonicalize_path;
pub use paths::expand_tilde;
pub use profile::AutoMatchRule;
pub use profile::ProfileError;
pub use profile::ProfileSource;
pub use profile::Resolution as ProfileResolution;
pub use profile::load_auto_match_rules;
pub use profile::resolve as resolve_profile;
pub use state_dir::HostOs;
pub use state_dir::StateDirError;
pub use state_dir::resolve as resolve_state_dir;
pub use template::Engine as TemplateEngine;
pub use template::TemplateError;
pub use variables::Builtins;
pub use variables::Resolver;
pub use variables::VariableError;

/// Options accepted by [`apply`]. Subsequent tasks extend this struct
/// with the resolved repository root, profile, CLI variable overrides,
/// and the `--yes` / `--force-deploy` / `--json` toggles.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct ApplyOptions {}

/// Options accepted by [`status`]. Subsequent tasks extend this with
/// the resolved repository root and output-format toggle.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct StatusOptions {}

/// Options accepted by [`rollback`]. Subsequent tasks extend this with
/// the journal timestamp selector and confirmation toggles.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct RollbackOptions {}

/// Compute and (depending on `options`) execute the apply plan for the
/// resolved dotfiles repository.
///
/// # Errors
///
/// Returns [`EngineError::NotImplemented`] until later tasks land the
/// discovery, plan, executor, and journal subsystems.
#[expect(
    clippy::unused_async,
    reason = "REQ-002 mandates an async signature; bodies become await-ful in later tasks."
)]
pub async fn apply(_options: ApplyOptions) -> Result<(), EngineError> {
    Err(EngineError::NotImplemented("apply"))
}

/// Report drift between the resolved dotfiles repository and the
/// current filesystem state.
///
/// # Errors
///
/// Returns [`EngineError::NotImplemented`] until later tasks land the
/// status subsystem.
#[expect(
    clippy::unused_async,
    reason = "REQ-002 mandates an async signature; bodies become await-ful in later tasks."
)]
pub async fn status(_options: StatusOptions) -> Result<(), EngineError> {
    Err(EngineError::NotImplemented("status"))
}

/// Roll back a prior apply to its pre-apply filesystem state using the
/// journaled backups.
///
/// # Errors
///
/// Returns [`EngineError::NotImplemented`] until later tasks land the
/// rollback subsystem.
#[expect(
    clippy::unused_async,
    reason = "REQ-002 mandates an async signature; bodies become await-ful in later tasks."
)]
pub async fn rollback(_options: RollbackOptions) -> Result<(), EngineError> {
    Err(EngineError::NotImplemented("rollback"))
}
