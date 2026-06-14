//! Core library for the patina cross-platform dotfile manager.
//!
//! The three public async entry points — [`apply`](fn@crate::apply),
//! [`status`](fn@crate::status), and [`rollback`](fn@crate::rollback) —
//! define the engine's outer contract. They return
//! [`Result<_, EngineError>`](EngineError); the CLI wraps that into
//! `anyhow::Result` at the call site (per the project rule that
//! `anyhow` lives only in the binary).

pub mod apply;
pub mod backups;
pub mod clock;
pub mod config;
pub mod discovery;
pub mod error;
mod fsx;
pub mod journal;
pub mod lock;
pub mod paths;
pub mod profile;
pub mod rollback;
pub mod state_dir;
pub mod status;
pub mod template;
pub mod variables;
pub mod version_envelope;
pub mod watch;
pub mod windows;

pub use apply::ClassifyError;
pub use apply::CompletionRecord;
pub use apply::ExecutorError;
pub use apply::ForceDeploy;
pub use apply::HookError;
pub use apply::HookOutcome;
pub use apply::Materialization;
pub use apply::ResolvedHook;
pub use apply::engine::ApplyRequest;
pub use apply::engine::ApplyResult;
pub use apply::engine::LockPolicy;
pub use apply::engine::ResolvedOperation;
pub use apply::engine::ResolvedPlan;
pub use apply::engine::TargetDisposition;
pub use apply::engine::execute as execute_plan;
pub use apply::engine::is_content_materialization;
pub use apply::engine::plan as plan_apply;
pub use apply::engine::plan_is_full_noop;
pub use apply::materialize;
pub use apply::resolve_on_path;
pub use apply::resolve_shells;
pub use apply::run_hook;
pub use apply::should_run;
pub use backups::BackupError;
pub use backups::RETENTION_COUNT;
pub use backups::backup_before_overwrite;
pub use backups::gc_retain;
pub use clock::current_timestamp;
pub use config::ConfigParseError;
pub use config::ConfigWriteError;
pub use config::FileEntry;
pub use config::FileMode;
pub use config::HookEntry;
pub use config::HookEvent;
pub use config::ModuleConfig;
pub use config::RootConfig;
pub use config::RootConfigError;
pub use config::append_directory_entry;
pub use config::append_file_entry;
pub use config::parse_module_config;
pub use config::parse_root_config;
pub use config::parse_root_config_str;
pub use config::remove_file_entry;
pub use config::scaffold_root_manifest;
pub use discovery::ModuleDiscoveryError;
pub use discovery::ModuleHandle;
pub use discovery::PERSISTED_DEFAULT_FILENAME;
pub use discovery::RepoDiscoveryError;
pub use discovery::default_repo_pointer_path;
pub use discovery::discover_modules;
pub use discovery::persisted_default_present;
pub use discovery::resolve_repository_root;
pub use discovery::write_persisted_default;
pub use error::EngineError;
pub use journal::ApplyRecord;
pub use journal::Disposition;
pub use journal::ExpectedTarget;
pub use journal::FILE_MAJOR_VERSION;
pub use journal::Journal;
pub use journal::JournalError;
pub use journal::LastApply;
pub use journal::Plan;
pub use journal::PlanRenderError;
pub use journal::PlannedOperation;
pub use journal::RecoveryReport;
pub use journal::content_hash;
pub use journal::load_plan_file;
pub use journal::prune_cycles;
pub use journal::read_latest_commit;
pub use journal::recover_orphans;
pub use journal::render_plan;
pub use lock::EXCLUSIVE_TIMEOUT;
pub use lock::EXCLUSIVE_TIMEOUT_ENV;
pub use lock::LockError;
pub use lock::LockGuard;
pub use lock::LockKind;
pub use lock::SHARED_TIMEOUT;
pub use lock::acquire as acquire_lock;
pub use lock::exclusive_timeout;
pub use paths::PathError;
pub use paths::canonicalize as canonicalize_path;
pub use paths::expand_tilde;
pub use profile::AutoMatchRule;
pub use profile::ProfileError;
pub use profile::ProfileSource;
pub use profile::Resolution as ProfileResolution;
pub use profile::load_auto_match_rules;
pub use profile::resolve as resolve_profile;
pub use rollback::RollbackError;
pub use rollback::run as run_rollback;
pub use state_dir::HostOs;
pub use state_dir::StateDirError;
pub use state_dir::resolve as resolve_state_dir;
pub use status::StatusEntry;
pub use status::StatusReport;
pub use status::TargetState;
pub use status::current_plan_targets;
pub use status::manage_key;
pub use status::report as status_report;
pub use template::Engine as TemplateEngine;
pub use template::TemplateError;
pub use variables::Builtins;
pub use variables::Resolver;
pub use variables::VariableError;
pub use version_envelope::ENVELOPE_LEN;
pub use version_envelope::EnvelopeError;
pub use version_envelope::decode_envelope;
pub use version_envelope::encode_with_envelope;
pub use watch::WatchError;
pub use watch::debounce::DEBOUNCE;
pub use watch::debounce::DebounceError;
pub use watch::debounce::EventBatch;
pub use watch::drift_cache::DRIFT_CACHE_MAJOR_VERSION;
pub use watch::drift_cache::DriftCache;
pub use watch::drift_cache::DriftCacheError;
pub use watch::drift_cache::DriftEntry;
pub use watch::drift_cache::load_drift_cache_file;
pub use watch::drift_cache::render_drift_cache;
pub use watch::logging::FileAppender;
pub use watch::logging::LoggingError;
pub use watch::logging::build_file_appender;
pub use watch::run_foreground;
pub use watch::service::LifecycleResult;
pub use watch::service::ServiceBackend;
pub use watch::service::ServiceError;
pub use watch::service::ServiceStatus;
pub use watch::service::current as current_service_backend;
pub use watch::subscriptions::compute_subscriptions;
pub use watch::watcher_config_warning;
pub use windows::DEV_MODE_REGISTRY_PATH;
pub use windows::DevModeProbe;
pub use windows::DevModeStatus;
pub use windows::GateDecision;
pub use windows::HostDevModeProbe;
pub use windows::WindowsError;
pub use windows::decide_symlink_gate;
pub use windows::dev_mode_status;
#[cfg(windows)]
pub use windows::elevate::ElevationOutcome;
#[cfg(windows)]
pub use windows::elevate::launch_elevate_helper;
pub use windows::is_elevated;
pub use windows::is_unc_path;
pub use windows::plan_has_symlink_op;
pub use windows::windows_build_supports_dev_mode;

/// Options accepted by [`apply`](fn@crate::apply). The TTY-driven prompt,
/// `--json` envelope, and `--pager` plumbing live in the CLI ([`plan_apply`] /
/// [`execute_plan`] are the two engine primitives it drives); this
/// convenience entry point unconditionally plans and executes, mirroring
/// `patina apply --yes`.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct ApplyOptions {
    /// Invocation toggles forwarded to the engine (`--force-deploy`,
    /// `-v` overrides).
    pub request: ApplyRequest,
    /// Timestamp keying this run's journal and backup files. The CLI
    /// supplies a real UTC timestamp; tests supply a fixed string.
    pub timestamp: String,
}

/// Options accepted by [`status`](fn@crate::status).
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct StatusOptions {}

/// Options accepted by [`rollback`](fn@crate::rollback).
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct RollbackOptions {}

/// Compute and (depending on `options`) execute the apply plan for the
/// resolved dotfiles repository.
///
/// # Errors
///
/// Returns an [`EngineError`] when planning or execution fails. A hook
/// that fails under `must_succeed` is reported through the returned
/// [`ApplyResult`], not as an error.
pub async fn apply(options: ApplyOptions) -> Result<ApplyResult, EngineError> {
    let resolved = plan_apply(&options.request, options.timestamp)?;
    execute_plan(&resolved, &options.request, LockPolicy::default()).await
}

/// Report drift between the resolved dotfiles repository and the current
/// filesystem state, classifying every managed target as CLEAN / DRIFTED /
/// MISSING / ORPHANED against the last committed apply.
///
/// # Errors
///
/// Returns an [`EngineError`] when repository / state-directory
/// resolution, the current-plan computation, or the journal read fails. A
/// shared-lock timeout is downgraded to a warning in the returned
/// [`StatusReport`], not an error.
#[expect(
    clippy::unused_async,
    reason = "An async signature is required; the status read itself is synchronous."
)]
pub async fn status(_options: StatusOptions) -> Result<StatusReport, EngineError> {
    let targets = current_plan_targets()?;
    status_report(&targets)
}

/// Roll back the most recent committed apply to its pre-apply filesystem
/// state using the journaled backups.
///
/// Delegates to [`run_rollback`], which takes the exclusive lock, finds the
/// most recent committed-and-not-rolled-back apply, reverts each `[[file]]`
/// entry's inverse operations atomically, and marks the apply rolled back.
///
/// # Errors
///
/// Returns an [`EngineError`] when no prior apply remains
/// ([`RollbackError::NoPriorApply`]), a multi-target entry cannot be
/// reverted as a unit ([`RollbackError::RollbackPartial`]), or a
/// filesystem / lock / record-decode operation fails.
#[expect(
    clippy::unused_async,
    reason = "An async signature is required; the rollback itself is synchronous filesystem work."
)]
pub async fn rollback(_options: RollbackOptions) -> Result<(), EngineError> {
    run_rollback()
}
