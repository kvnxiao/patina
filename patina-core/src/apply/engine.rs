//! End-to-end `patina apply` orchestration (REQ-017).
//!
//! The file-mode executors ([`crate::apply::materialize`]), the hook
//! runner ([`crate::apply::hooks`]), the plan journal
//! ([`crate::journal`]), and the backup tree ([`crate::backups`]) each
//! own one slice of an apply. This module is the orchestrator that wires
//! them together into the two-phase shape REQ-017 describes:
//!
//! 1. [`plan`] resolves the repository, enumerates modules, parses every module
//!    manifest, resolves the active profile and variable context, canonicalizes
//!    each `[[file]]` entry's source and targets, and produces a
//!    [`ResolvedPlan`]. Planning performs **no** filesystem mutation, so the
//!    CLI can render a diff and (in a non-TTY, or with `--json` and no `--yes`)
//!    exit without touching the user's `$HOME`.
//! 2. [`execute`] takes the [`ResolvedPlan`] and mutates: it recovers any
//!    orphan plan, takes the exclusive lock, flushes the journal, runs
//!    `pre_apply` hooks, materializes every operation (backing up each
//!    pre-existing target first), runs `post_apply` hooks, and either commits
//!    or rolls the file operations back.
//!
//! The CLI ([`patina-cli`]) owns the diff rendering, the TTY prompt, the
//! `--pager` plumbing, and the JSON envelope; this module owns the
//! engine semantics so those presentation concerns never reach into the
//! subsystem internals.

use crate::apply::CompletionRecord;
use crate::apply::ForceDeploy;
use crate::apply::HookOutcome;
use crate::apply::Materialization;
use crate::apply::ResolvedHook;
use crate::apply::hooks;
use crate::apply::materialize;
use crate::backups::backup_before_overwrite;
use crate::backups::gc_retain;
use crate::config::FileMode;
use crate::config::HookEntry;
use crate::config::HookEvent;
use crate::config::parse_module_config;
use crate::discovery::discover_modules;
use crate::discovery::resolve_repository_root;
use crate::error::EngineError;
use crate::journal::ApplyRecord;
use crate::journal::ExpectedTarget;
use crate::journal::Journal;
use crate::journal::LastApply;
use crate::journal::OsSyncer;
use crate::journal::Plan;
use crate::journal::PlannedOperation;
use crate::journal::fingerprint_bytes;
use crate::journal::recover_orphans;
use crate::journal::timestamp_to_rfc3339;
use crate::lock::EXCLUSIVE_TIMEOUT;
use crate::lock::LockKind;
use crate::lock::acquire as acquire_lock;
use crate::paths::canonicalize;
use crate::paths::expand_tilde;
use crate::profile::load_auto_match_rules;
use crate::profile::resolve as resolve_profile;
use crate::state_dir::HostOs;
use crate::state_dir::resolve as resolve_state_dir;
use crate::template::Engine;
use crate::variables::Builtins;
use crate::variables::Resolver;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Manifest filename for the repository root and each module.
const MANIFEST_FILENAME: &str = "patina.toml";

/// Invocation toggles for an apply, populated by the CLI from the parsed
/// flags. Plain-data so the CLI can construct it without reaching into
/// the engine internals.
#[derive(Debug, Clone)]
pub struct ApplyRequest {
    /// `--force-deploy`: override every hook to `must_succeed = false`.
    pub force_deploy: ForceDeploy,
    /// `-v key=value` CLI variable overrides, in declaration order.
    pub cli_overrides: Vec<(String, String)>,
}

impl Default for ApplyRequest {
    fn default() -> Self {
        Self {
            force_deploy: ForceDeploy::No,
            cli_overrides: Vec::new(),
        }
    }
}

/// One resolved operation: the durable [`PlannedOperation`] paired with
/// the canonical absolute paths and mode the executor needs, plus the
/// rendered target content used for diff display (template/copy modes).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedOperation {
    /// The mode the executor dispatches on.
    pub mode: FileMode,
    /// Canonical absolute source path.
    pub source: Utf8PathBuf,
    /// Canonical absolute target paths the source fans out to.
    pub targets: Vec<Utf8PathBuf>,
}

/// Everything an apply needs after planning, with no mutation performed
/// yet. Built by [`plan`]; consumed by [`execute`] and by the CLI's diff
/// and JSON rendering.
#[derive(Debug)]
#[non_exhaustive]
pub struct ResolvedPlan {
    /// Canonical absolute repository root.
    pub repo_root: Utf8PathBuf,
    /// Resolved active profile name (empty for the no-profile fallback).
    pub profile: String,
    /// The durable plan recorded to the journal before any mutation.
    pub plan: Plan,
    /// Per-operation resolved executor inputs, parallel to
    /// [`Plan::operations`].
    pub operations: Vec<ResolvedOperation>,
    /// Every `[[hook]]` entry across all modules, owned so the resolved
    /// hooks can borrow from it during [`execute`].
    pub hooks: Vec<HookEntry>,
    /// Per-machine state directory root (`<state>/patina`).
    pub state_dir: Utf8PathBuf,
    /// Resolved host OS family (drives hook shell defaults).
    pub host_os: HostOs,
    /// Timestamp keying this run's journal and backup files.
    pub timestamp: String,
    /// Fully-resolved variable context (built-ins + CLI overrides +
    /// per-module layers + resolved profile). Reused by the executors,
    /// the hook `when` evaluator, and the CLI diff renderer so all three
    /// agree on rendered template output.
    pub resolver: Resolver,
}

impl ResolvedPlan {
    /// The journal directory for this run.
    fn journal_dir(&self) -> Utf8PathBuf {
        self.state_dir.join("journal")
    }

    /// The backups directory for this run.
    fn backups_dir(&self) -> Utf8PathBuf {
        self.state_dir.join("backups")
    }

    /// The advisory-lock file path for this run.
    fn lock_path(&self) -> Utf8PathBuf {
        self.state_dir.join("lock")
    }
}

/// How an [`execute`] settled. The CLI maps this onto the `result` field
/// of the JSON envelope and onto the process exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyResult {
    /// Every operation and hook succeeded; the apply committed.
    Applied {
        /// Human-readable warnings from hooks that failed but were
        /// degraded (`must_succeed = false` or `--force-deploy`).
        warnings: Vec<String>,
    },
    /// A `must_succeed` `post_apply` hook failed; the file operations
    /// were reversed to the pre-apply state. Exit code 3.
    RolledBack {
        /// Name (command) of the hook whose failure triggered rollback.
        failed_hook: String,
    },
    /// A `must_succeed` `pre_apply` hook failed before any file
    /// operation ran; nothing was mutated. Exit code 2.
    Aborted {
        /// Name (command) of the `pre_apply` hook that failed.
        failed_hook: String,
    },
}

/// Resolve the repository, modules, profile, variables, and paths into a
/// [`ResolvedPlan`] without mutating the filesystem.
///
/// # Errors
///
/// Returns an [`EngineError`] when repository discovery, module
/// enumeration, manifest parsing, state-directory resolution, profile
/// resolution, variable ingestion, or path canonicalization fails.
pub fn plan(
    request: &ApplyRequest,
    timestamp: impl Into<String>,
) -> Result<ResolvedPlan, EngineError> {
    let repo_root = resolve_repository_root()?;
    let state_dir = resolve_state_dir()?;
    let host_os = HostOs::current();
    let builtins = Builtins::current();

    let root_manifest = repo_root.join(MANIFEST_FILENAME);
    let auto_match_rules = load_auto_match_rules(&root_manifest)?;
    let profile = resolve_profile(
        std::env::var("PATINA_PROFILE").ok(),
        &state_dir.join("profile"),
        &auto_match_rules,
        &builtins,
    )?;

    let home = Utf8PathBuf::from(builtins.home.clone());
    let mut resolver = Resolver::new(builtins)
        .with_profile(profile.name.clone())
        .with_cli_overrides(request.cli_overrides.iter().cloned())?;

    let modules = discover_modules(&repo_root)?;

    let mut operations: Vec<PlannedOperation> = Vec::new();
    let mut resolved_ops: Vec<ResolvedOperation> = Vec::new();
    let mut hooks: Vec<HookEntry> = Vec::new();

    for module in &modules {
        let manifest = module.path.join(MANIFEST_FILENAME);
        let config = parse_module_config(&manifest)?;

        if let Some(table) = config.variables.as_ref() {
            let layer: Vec<(String, String)> = table
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect();
            resolver = resolver.with_per_module(layer)?;
        }

        for entry in &config.files {
            let abs_source = canonicalize(&module.path.join(&entry.source))?;
            let mut abs_targets = Vec::with_capacity(entry.targets.len());
            for target in &entry.targets {
                let expanded = expand_tilde(target, &home);
                abs_targets.push(canonicalize(&expanded)?);
            }

            for target in &abs_targets {
                operations.push(planned_operation(entry.mode, &abs_source, target));
            }
            resolved_ops.push(ResolvedOperation {
                mode: entry.mode,
                source: abs_source,
                targets: abs_targets,
            });
        }

        hooks.extend(config.hooks.iter().cloned());
    }

    Ok(ResolvedPlan {
        repo_root,
        profile: profile.name,
        plan: Plan::new(operations),
        operations: resolved_ops,
        hooks,
        state_dir,
        host_os,
        timestamp: timestamp.into(),
        resolver,
    })
}

/// Build the durable [`PlannedOperation`] for one resolved
/// `(mode, source, target)`.
fn planned_operation(mode: FileMode, source: &Utf8Path, target: &Utf8Path) -> PlannedOperation {
    match mode {
        FileMode::Symlink | FileMode::SymlinkDir => {
            PlannedOperation::symlink(source.as_str(), target.as_str())
        }
        FileMode::Copy | FileMode::CopyTree => {
            PlannedOperation::copy(source.as_str(), target.as_str())
        }
        FileMode::TemplateRender => PlannedOperation::render(source.as_str(), target.as_str()),
    }
}

/// Execute a [`ResolvedPlan`] against the filesystem.
///
/// Recovers any orphan plan, takes the exclusive lock, flushes the
/// journal, runs `pre_apply` hooks, materializes every operation (backing
/// up each pre-existing target first), runs `post_apply` hooks, and
/// commits — or rolls the file operations back when a `must_succeed`
/// `post_apply` hook fails.
///
/// # Errors
///
/// Returns an [`EngineError`] when recovery, locking, journal flushing,
/// an executor, hook execution, backup, or retention GC fails. A hook
/// that *fails* under `must_succeed` is not an error: it is reported via
/// the returned [`ApplyResult`] so the CLI can map it to the right exit
/// code.
pub async fn execute(
    resolved: &ResolvedPlan,
    request: &ApplyRequest,
) -> Result<ApplyResult, EngineError> {
    let journal_dir = resolved.journal_dir();
    let backups_dir = resolved.backups_dir();
    let template_engine = Engine::new();

    // Recover any prior partial apply before computing fresh work.
    recover_orphans(&journal_dir, &backups_dir)?;

    // Mutating subcommands take the exclusive lock for the whole apply.
    let _guard = acquire_lock(
        &resolved.lock_path(),
        LockKind::Exclusive,
        EXCLUSIVE_TIMEOUT,
    )?;

    // Resolve every hook's shell up front so an unresolved explicit shell
    // aborts before any file operation runs.
    let resolved_hooks = hooks::resolve_shells(&resolved.hooks, resolved.host_os)?;

    // pre_apply hooks run before any file operation. Reuse the resolver
    // built during planning so hook `when` predicates and template
    // renders see the same CLI overrides and module variables.
    let vars = &resolved.resolver;
    if let Some(failed) = run_hook_phase(
        &resolved_hooks,
        HookEvent::PreApply,
        &template_engine,
        vars,
        request.force_deploy,
    )
    .await?
    {
        return Ok(ApplyResult::Aborted {
            failed_hook: failed,
        });
    }

    // Flush the plan journal — the durability point before mutation.
    let mut journal = Journal::flush_plan_and_fsync(
        &journal_dir,
        &resolved.timestamp,
        &resolved.plan,
        &OsSyncer,
    )?;

    // Materialize every operation, backing up each pre-existing target
    // first. Track completion records so a post_apply hook failure can
    // reverse them.
    let mut completed: Vec<CompletionRecord> = Vec::new();
    let mut op_index: u32 = 0;
    for op in &resolved.operations {
        for target in &op.targets {
            backup_before_overwrite(&backups_dir, &resolved.timestamp, target)?;
        }
        let records = materialize(op.mode, &op.source, &op.targets, &template_engine, vars)?;
        for _ in &records {
            journal.record_progress(op_index)?;
            op_index = op_index.saturating_add(1);
        }
        completed.extend(records);
    }

    // post_apply hooks run after the file operations.
    let mut warnings = Vec::new();
    let post_failure = run_hook_phase_collecting(
        &resolved_hooks,
        HookEvent::PostApply,
        &template_engine,
        vars,
        request.force_deploy,
        &mut warnings,
    )
    .await?;

    if let Some(failed) = post_failure {
        // Reverse the file operations to the pre-apply state, then mark
        // the journal rolled back rather than committed. The journal
        // handle is consumed by commit; for a rollback we drop it after
        // the reversal so recovery treats it as an orphan that has
        // already been reversed on disk. Re-running recovery is
        // idempotent.
        reverse_completed(&completed, &backups_dir, &resolved.timestamp)?;
        drop(journal);
        Ok(ApplyResult::RolledBack {
            failed_hook: failed,
        })
    } else {
        let record = build_apply_record(resolved, &completed)?;
        journal.commit(&record, &OsSyncer)?;
        gc_retain(&backups_dir, crate::backups::RETENTION_COUNT)?;
        Ok(ApplyResult::Applied { warnings })
    }
}

/// Build the [`ApplyRecord`] persisted in this run's COMMIT sentinel from
/// the resolved plan's `last_apply` metadata and the completed
/// materializations. `patina status` (T-017) decodes this to classify the
/// live filesystem against the last committed apply.
///
/// Each completed object becomes one [`ExpectedTarget`]: a symlink records
/// its canonical link target; a copy or render records a fingerprint of
/// the bytes that were just written, read back from the live target so the
/// recorded fingerprint matches exactly what `status` will compute.
fn build_apply_record(
    resolved: &ResolvedPlan,
    completed: &[CompletionRecord],
) -> Result<ApplyRecord, EngineError> {
    let vars = &resolved.resolver;
    let last_apply = LastApply {
        at: timestamp_to_rfc3339(&resolved.timestamp),
        user: vars.get("patina.user").unwrap_or_default(),
        host: vars.get("patina.hostname").unwrap_or_default(),
    };

    let mut targets = Vec::with_capacity(completed.len());
    for record in completed {
        let target = record.target.as_str().to_owned();
        match &record.materialization {
            Materialization::Symlink { link_target } => {
                targets.push(ExpectedTarget::Symlink {
                    target,
                    link_target: link_target.as_str().to_owned(),
                });
            }
            Materialization::Copy | Materialization::Render => {
                let bytes = fs_err::read(&record.target).map_err(|source| {
                    EngineError::Journal(crate::journal::JournalError::Filesystem(source))
                })?;
                targets.push(ExpectedTarget::Content {
                    target,
                    fingerprint: fingerprint_bytes(&bytes),
                });
            }
        }
    }
    Ok(ApplyRecord::new(last_apply, targets))
}

/// Run every hook whose event matches `event`, returning the command of
/// the first hook that *fails* under `must_succeed` (so the orchestrator
/// can abort or roll back). Hooks that warn are silently tolerated here;
/// the post-apply collector path records their warnings instead.
async fn run_hook_phase(
    hooks: &[ResolvedHook<'_>],
    event: HookEvent,
    engine: &Engine,
    resolver: &Resolver,
    force_deploy: ForceDeploy,
) -> Result<Option<String>, EngineError> {
    let mut sink = Vec::new();
    run_hook_phase_collecting(hooks, event, engine, resolver, force_deploy, &mut sink).await
}

/// Run every hook whose event matches `event`, pushing a human-readable
/// warning for each [`HookOutcome::Warned`] into `warnings` and returning
/// the command of the first [`HookOutcome::Failed`] hook.
async fn run_hook_phase_collecting(
    hooks: &[ResolvedHook<'_>],
    event: HookEvent,
    engine: &Engine,
    resolver: &Resolver,
    force_deploy: ForceDeploy,
    warnings: &mut Vec<String>,
) -> Result<Option<String>, EngineError> {
    for hook in hooks {
        if hook.entry.event != event {
            continue;
        }
        if !hooks::should_run(hook, engine, resolver)? {
            continue;
        }
        match hooks::run_hook(hook, force_deploy).await? {
            HookOutcome::Succeeded => {}
            HookOutcome::Warned => {
                warnings.push(format!(
                    "hook `{}` exited non-zero but was treated as non-fatal",
                    hook.entry.command
                ));
            }
            HookOutcome::Failed => {
                return Ok(Some(hook.entry.command.clone()));
            }
        }
    }
    Ok(None)
}

/// Reverse every completed materialization to the pre-apply state.
///
/// A target that had a backup is restored from it; a freshly created
/// target (no backup) is removed. This mirrors crash recovery's reversal
/// rule, applied in-process when a `post_apply` hook fails.
fn reverse_completed(
    completed: &[CompletionRecord],
    backups_dir: &Utf8Path,
    timestamp: &str,
) -> Result<(), EngineError> {
    use crate::journal::mirror_backup_path;

    // Reverse in inverse order so later operations are undone first.
    for record in completed.iter().rev() {
        let backup = mirror_backup_path(backups_dir, timestamp, &record.target);
        if backup.exists() {
            // The target pre-existed; restore the original bytes.
            if let Some(parent) = record.target.parent()
                && !parent.as_str().is_empty()
            {
                fs_err::create_dir_all(parent).map_err(|source| {
                    EngineError::Journal(crate::journal::JournalError::Filesystem(source))
                })?;
            }
            remove_target(&record.target)?;
            fs_err::copy(&backup, &record.target).map_err(|source| {
                EngineError::Journal(crate::journal::JournalError::Filesystem(source))
            })?;
        } else {
            // Freshly created; delete it.
            remove_target(&record.target)?;
        }
    }
    Ok(())
}

/// Remove a target file or symlink, tolerating its absence.
fn remove_target(target: &Utf8Path) -> Result<(), EngineError> {
    match fs_err::symlink_metadata(target) {
        Ok(meta) => {
            let result = if meta.is_dir() {
                fs_err::remove_dir_all(target)
            } else {
                fs_err::remove_file(target)
            };
            result.map_err(|source| {
                EngineError::Journal(crate::journal::JournalError::Filesystem(source))
            })
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(EngineError::Journal(
            crate::journal::JournalError::Filesystem(source),
        )),
    }
}

/// Whether a materialization wrote rendered/copied content (as opposed to
/// a symlink). Used by the CLI diff renderer to decide between a content
/// diff and a link-target diff.
#[must_use = "the materialization kind selects the diff rendering"]
pub fn is_content_materialization(materialization: &Materialization) -> bool {
    matches!(
        materialization,
        Materialization::Copy | Materialization::Render
    )
}
