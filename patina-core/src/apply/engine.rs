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
//! The CLI (`patina-cli`) owns the diff rendering, the TTY prompt, the
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
use crate::config::EntryKind;
use crate::config::FileMode;
use crate::config::HookEntry;
use crate::config::HookEvent;
use crate::config::ManagedEntry;
use crate::config::parse_module_config;
use crate::config::parse_root_config;
use crate::discovery::discover_modules;
use crate::discovery::resolve_repository_root;
use crate::error::EngineError;
use crate::journal::ApplyRecord;
use crate::journal::Disposition;
use crate::journal::ExpectedTarget;
use crate::journal::Journal;
use crate::journal::LastApply;
use crate::journal::OsSyncer;
use crate::journal::Plan;
use crate::journal::PlannedOperation;
use crate::journal::content_hash;
use crate::journal::prune_cycles;
use crate::journal::recover_orphans;
use crate::journal::timestamp_to_rfc3339;
use crate::lock::LockGuard;
use crate::lock::LockKind;
use crate::lock::acquire as acquire_lock;
use crate::lock::exclusive_timeout;
use crate::lock::try_acquire as try_acquire_lock;
use crate::paths::canonicalize;
use crate::paths::expand_tilde;
use crate::paths::resolve_location;
use crate::profile::load_auto_match_rules;
use crate::profile::resolve as resolve_profile;
use crate::state_dir::HostOs;
use crate::state_dir::resolve as resolve_state_dir;
use crate::template::Engine;
use crate::variables::Builtins;
use crate::variables::Resolver;
use crate::windows::GateDecision;
use crate::windows::HostDevModeProbe;
use crate::windows::decide_symlink_gate;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::collections::BTreeSet;
use tracing::warn;

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

/// How [`execute`] obtains the exclusive advisory lock guarding the apply
/// (REQ-030).
///
/// The default ([`LockPolicy::Blocking`]) reproduces the pre-amendment
/// behaviour byte-for-byte: acquire exclusive with [`exclusive_timeout`],
/// mapping a timeout to exit code 4. The two added strategies let callers
/// outside the CLI's `apply` path drive an apply differently:
///
/// - [`LockPolicy::NonBlocking`] — make a single non-blocking attempt and, on
///   contention, return [`crate::lock::LockError::Contended`] before any
///   filesystem mutation. The SPEC-0003 watcher uses this to skip a reapply
///   while a CLI run holds the lock.
/// - [`LockPolicy::Held`] — reuse a guard the caller already acquired,
///   acquiring nothing further. The SPEC-0002 `remove` / `promote` commands use
///   this to re-journal while already holding the exclusive lock, without
///   deadlocking against their own held lock.
///
/// The guard variant carries a non-`Clone` [`LockGuard`], so the policy is
/// passed to [`execute`] as a distinct argument rather than living on the
/// `Clone` [`ApplyRequest`].
#[derive(Debug, Default)]
#[non_exhaustive]
pub enum LockPolicy {
    /// Acquire the exclusive lock, waiting up to [`exclusive_timeout`]; a
    /// timeout maps to exit code 4. The default and the only policy the
    /// CLI's `apply` / `rollback` paths use.
    #[default]
    Blocking,
    /// Make exactly one non-blocking acquisition attempt; on contention
    /// return [`crate::lock::LockError::Contended`] with zero mutation.
    NonBlocking,
    /// Use the caller's already-acquired exclusive guard for the run;
    /// acquire nothing.
    Held(LockGuard),
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
    /// Index of the managed entry that produced this operation, assigned
    /// at plan time over the full declared sequence (every `[[file]]`
    /// entry across all modules first, then every `[[directory]]` entry)
    /// as a single monotonic `u32` space. This index — not a re-derivation
    /// from operation position — is what [`execute`] records on each
    /// [`ExpectedTarget`], so a `[[file]]` and a `[[directory]]` entry can
    /// never collide on an index and per-entry atomic rollback (REQ-019 /
    /// DEC-009) groups targets by their declared entry.
    pub entry_index: u32,
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
    let host_os = HostOs::current();
    let PlanningContext {
        repo_root,
        state_dir,
        home,
        profile,
        mut resolver,
        engine,
        modules,
    } = build_planning_context(&request.cli_overrides)?;

    // Resolve every managed entry into its canonical source/targets, kept
    // in two ordered buckets — `[[file]]` entries and `[[directory]]`
    // entries — each in declaration order across all modules as the modules
    // are iterated. Emitting from these buckets files-then-directories
    // gives the single deterministic order REQ-009 mandates while a managed
    // entry's canonicalization stays where it always was (per-module, under
    // that module's tilde/home context).
    //
    // Each bucket slot is an `Option<ResolvedEntry>`: a `when`-false entry
    // contributes `None`, so it still occupies its position in the declared
    // sequence (and thus its `entry_index`, REQ-009) but emits no operation
    // and no diff line (DEC-004). The `when` gate runs at the top of the
    // per-entry body, before `resolve_entry` canonicalizes the source — so a
    // gated-off entry whose source is absent or wrong-kind on this OS is
    // never canonicalized or validated (REQ-009 ordering).
    let mut file_entries: Vec<Option<ResolvedEntry>> = Vec::new();
    let mut directory_entries: Vec<Option<ResolvedEntry>> = Vec::new();
    let mut hooks: Vec<HookEntry> = Vec::new();

    for module in &modules {
        let manifest = module.path.join(MANIFEST_FILENAME);
        let config = parse_module_config(&manifest)?;

        if let Some(table) = config.variables.as_ref() {
            resolver = resolver.with_per_module(table_to_layer(table))?;
        }

        for entry in &config.files {
            file_entries.push(gate_and_resolve_entry(
                entry,
                &module.path,
                &home,
                &engine,
                &resolver,
            )?);
        }
        for entry in &config.directories {
            directory_entries.push(gate_and_resolve_entry(
                entry,
                &module.path,
                &home,
                &engine,
                &resolver,
            )?);
        }

        hooks.extend(config.hooks.iter().cloned());
    }

    let (operations, resolved_ops) = assemble_plan_operations(file_entries, directory_entries);

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

/// The repository, profile, variable resolver, and `when` engine shared by
/// the two passes that must agree on which entries are active and how their
/// `when` predicates resolve: [`plan`] (which builds the apply plan) and
/// [`current_managed_targets`] (which recomputes the managed-target set for
/// `patina status` and the apply-time orphan reap).
///
/// Everything up to — but not including — the per-module entry loop is
/// identical between the two passes (REQ-005's repo-shared / per-profile
/// layer pushes, the active-profile resolution, the shared `MiniJinja`
/// engine). Factoring it here keeps the `when` gate seeing the same variable
/// context in planning and in status, so an entry that plans on this host is
/// the same entry status counts as managed (and the reap leaves alone).
struct PlanningContext {
    repo_root: Utf8PathBuf,
    state_dir: Utf8PathBuf,
    home: Utf8PathBuf,
    profile: crate::profile::Resolution,
    resolver: Resolver,
    engine: Engine,
    modules: Vec<crate::discovery::ModuleHandle>,
}

/// Build the [`PlanningContext`] both [`plan`] and [`current_managed_targets`]
/// consume.
///
/// Resolves the repository and state directory, the active profile, and the
/// resolver's repo-shared (`[variables]`) and active-profile
/// (`[profiles.<name>.variables]`) layers (REQ-005). The per-module layer is
/// *not* pushed here — each pass pushes it during its own module loop, in
/// declaration order, so a module's `[variables]` is in scope for that
/// module's entries' `when` predicates.
///
/// # Errors
///
/// Returns an [`EngineError`] when repository / state-directory resolution,
/// profile resolution, root-manifest parsing, module enumeration, or a
/// reserved-key violation in a variable layer fails.
fn build_planning_context(
    cli_overrides: &[(String, String)],
) -> Result<PlanningContext, EngineError> {
    let repo_root = resolve_repository_root()?;
    let state_dir = resolve_state_dir()?;
    let builtins = Builtins::current();

    // The shared `MiniJinja` engine that evaluates every `when` predicate
    // (REQ-004 / DEC-006): `[[file]]` / `[[directory]]` / `[[hook]]` and
    // `[[auto_match]]` all route through this one instance. It is built
    // before profile resolution because auto-match `when` predicates are
    // evaluated through it (against a built-ins-only resolver, since the
    // user variable layers are not yet assembled). It is cheap and
    // clone-shares one `Arc` environment.
    let engine = Engine::new();

    let root_manifest = repo_root.join(MANIFEST_FILENAME);
    let auto_match_rules = load_auto_match_rules(&root_manifest)?;
    let profile = resolve_profile(
        std::env::var("PATINA_PROFILE").ok(),
        &state_dir.join("profile"),
        &auto_match_rules,
        &builtins,
        &engine,
    )?;

    // The root manifest's repo-shared `[variables]` table and the active
    // profile's `[profiles.<name>.variables]` table are the two layers this
    // pass populates (REQ-005). Resolution precedence is fixed by the
    // resolver's layer order (CLI > per-machine > per-profile > per-module >
    // repo-shared > built-ins); pushing them here changes no precedence.
    let root_config = parse_root_config(&root_manifest)?;

    let home = Utf8PathBuf::from(builtins.home.clone());
    let mut resolver = Resolver::new(builtins)
        .with_profile(profile.name.clone())
        .with_cli_overrides(cli_overrides.iter().cloned())?
        .with_repo_shared(table_to_layer(&root_config.repo_shared))?;

    // The no-profile fallback (empty profile name) selects no per-profile
    // table; a named profile selects its table when the root declares one.
    if let Some(table) = root_config.per_profile.get(&profile.name) {
        resolver = resolver.with_per_profile(table_to_layer(table))?;
    }

    let modules = discover_modules(&repo_root)?;

    Ok(PlanningContext {
        repo_root,
        state_dir,
        home,
        profile,
        resolver,
        engine,
        modules,
    })
}

/// Recompute the set of canonical target paths the *current* repository plan
/// manages, keyed by [`crate::status::manage_key`] for cross-time comparison
/// against the recorded commit (REQ-007 / REQ-003).
///
/// This is the `when`-aware, `symlink-tree`-aware managed set both
/// `patina status` (to classify a dropped target ORPHANED) and the apply-time
/// orphan reap consume. It mirrors [`plan`]'s entry walk with two
/// differences that make it safe to run for status, where the plan would
/// refuse:
///
/// - **`when` gating (REQ-003).** An entry whose `when` is false on this host
///   contributes no managed target, so a `[[file]]` whose `when` has been
///   edited to false has its prior target fall out of the set and classify
///   ORPHANED (CHK-019). The gate uses the same [`Engine::eval_when`] and
///   layered resolver as planning, so the two passes agree on which entries are
///   active.
/// - **Tree-mode leaf expansion (REQ-007).** A `symlink-tree` or `copy-tree`
///   `[[directory]]` entry is expanded into one managed key per *live* source
///   leaf, walked in the same `walk_files` order the executor used, so a
///   deleted source leaf is absent from the set and its recorded target leaf
///   classifies ORPHANED (CHK-014). Both modes materialize one object per leaf
///   and journal each leaf as its own target, so both must expand here; every
///   other mode contributes its declared target(s) directly.
///
/// Unlike [`plan`], this never canonicalizes the source or kind-checks it:
/// status must not fail because a `when`-true entry's source is missing or
/// wrong-shaped (that is the apply plan's job to report). A `symlink-tree`
/// source that is missing simply yields no leaves.
///
/// # Errors
///
/// Returns an [`EngineError`] when repository discovery, profile resolution,
/// module enumeration, manifest parsing, a reserved-key violation, or a
/// `when` predicate evaluation fails.
pub fn current_managed_targets() -> Result<BTreeSet<Utf8PathBuf>, EngineError> {
    let PlanningContext {
        home,
        mut resolver,
        engine,
        modules,
        ..
    } = build_planning_context(&[])?;

    let mut targets = BTreeSet::new();
    for module in &modules {
        let manifest = module.path.join(MANIFEST_FILENAME);
        let config = parse_module_config(&manifest)?;

        if let Some(table) = config.variables.as_ref() {
            resolver = resolver.with_per_module(table_to_layer(table))?;
        }

        for entry in config.files.iter().chain(&config.directories) {
            // `when`-false entries manage nothing this run (REQ-003): their
            // prior targets fall out of the set and classify ORPHANED.
            if let Some(expr) = entry.when.as_deref()
                && !engine.eval_when(expr, &resolver)?
            {
                continue;
            }
            insert_managed_targets(entry, &module.path, &home, &mut targets);
        }
    }
    Ok(targets)
}

/// Insert the managed `manage_key`(s) for one surviving (`when`-true) entry.
///
/// A tree-mode entry (`symlink-tree` or `copy-tree`) expands to one key per
/// live source leaf, mirrored under each declared target the same way the
/// executor materializes them (`target.join(rel)`); a missing source
/// contributes no leaves. Every other mode contributes its declared targets
/// directly.
fn insert_managed_targets(
    entry: &ManagedEntry,
    module_path: &Utf8Path,
    home: &Utf8Path,
    targets: &mut BTreeSet<Utf8PathBuf>,
) {
    use crate::status::manage_key;

    // Tree modes (`symlink-tree` and `copy-tree`) materialize one object per
    // live source leaf and the journal records each leaf as its own target, so
    // the managed set must expand to those same leaves. Recording only the
    // declared directory would make every committed leaf look orphaned on the
    // next apply — the reap pass would delete it (and `copy-tree`'s journal
    // hashing would then fail on the just-reaped file).
    if matches!(entry.mode, FileMode::SymlinkTree | FileMode::CopyTree) {
        let source = module_path.join(&entry.source);
        // The executor walks the live source for leaves; a source that no
        // longer exists yields none, so every recorded leaf is then orphaned.
        let Ok(leaves) = crate::apply::walk_files(&source) else {
            return;
        };
        for target in &entry.targets {
            let expanded = expand_tilde(target, home);
            for rel in &leaves {
                targets.insert(manage_key(&expanded.join(rel)));
            }
        }
        return;
    }

    for target in &entry.targets {
        let expanded = expand_tilde(target, home);
        targets.insert(manage_key(&expanded));
    }
}

/// Project a raw TOML variable table into the resolver's string-keyed
/// layer form, keeping only string-valued entries.
///
/// The variable layers are string→string; a non-string TOML value (an
/// array or sub-table) is not a variable binding and is dropped here, the
/// same way the per-module ingestion has always treated its `[variables]`
/// table. Shared by the repo-shared, per-profile, and per-module pushes in
/// [`plan`] so the three sites agree on the projection.
fn table_to_layer(table: &toml::value::Table) -> Vec<(String, String)> {
    table
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
        .collect()
}

/// A managed entry with its source and targets canonicalized, held in the
/// declared-order buckets before the single monotonic entry-index space is
/// assigned. Kept private to [`plan`]: the public per-operation surface is
/// [`ResolvedOperation`], which additionally carries the assigned index.
struct ResolvedEntry {
    /// The executor mode the entry resolves to.
    mode: FileMode,
    /// Canonical absolute source path.
    source: Utf8PathBuf,
    /// Canonical absolute target paths the source fans out to.
    targets: Vec<Utf8PathBuf>,
}

/// Impose REQ-009's single deterministic order on the resolved entries and
/// assign each managed entry its index over the full declared sequence.
///
/// Every `[[file]]` entry (in declaration order across all modules) is
/// emitted before every `[[directory]]` entry, and each managed entry is
/// assigned a single monotonic `u32` `entry_index` (files first, then
/// directories). The index advances for **every** declared entry,
/// including a `when`-false one (passed as `None`): a gated-off entry
/// occupies its index but emits no [`PlannedOperation`] and no
/// [`ResolvedOperation`] (REQ-009 / DEC-004). That index is carried on each
/// [`ResolvedOperation`] so [`execute`] records the planned index rather
/// than re-deriving one from operation position — guaranteeing no
/// `[[file]]` and `[[directory]]` entry collide on an index and that
/// targets sharing an entry form one atomic rollback unit (DEC-009 /
/// REQ-019). The returned [`PlannedOperation`] vec is the per-target
/// durable plan, parallel to the engine's existing wire format; the index
/// lives on the resolved-op side only, so the `entry: u32` journal layout
/// is unchanged (no version bump).
fn assemble_plan_operations(
    file_entries: Vec<Option<ResolvedEntry>>,
    directory_entries: Vec<Option<ResolvedEntry>>,
) -> (Vec<PlannedOperation>, Vec<ResolvedOperation>) {
    let mut operations: Vec<PlannedOperation> = Vec::new();
    let mut resolved_ops: Vec<ResolvedOperation> = Vec::new();
    let mut entry_index: u32 = 0;
    for slot in file_entries.into_iter().chain(directory_entries) {
        // A `when`-false entry still consumes its index but emits nothing.
        if let Some(resolved) = slot {
            for target in &resolved.targets {
                operations.push(planned_operation(resolved.mode, &resolved.source, target));
            }
            resolved_ops.push(ResolvedOperation {
                mode: resolved.mode,
                source: resolved.source,
                targets: resolved.targets,
                entry_index,
            });
        }
        entry_index = entry_index.saturating_add(1);
    }
    (operations, resolved_ops)
}

/// Evaluate one managed entry's `when` predicate, then — only if it holds —
/// canonicalize the entry's source and resolve its targets by declared
/// location.
///
/// This enforces the REQ-009 per-entry order: step (1) the `when` gate runs
/// first, so a `when`-false entry returns `Ok(None)` and is **never**
/// canonicalized; step (2) canonicalization happens only for a surviving
/// (`when`-true or no-`when`) entry. Returning `None` lets the caller keep
/// the entry's slot in the declared sequence (and thus its `entry_index`)
/// while emitting no operation or diff line (DEC-004). For a multi-target
/// entry the gate is above the target loop, so `when` gates all targets
/// together (REQ-003).
///
/// Step (3) of the REQ-009 order — the plan-time source existence-and-kind
/// validation — runs inside [`resolve_entry`], right after the source is
/// canonicalized, so a `when`-false entry (which returns `Ok(None)` here
/// before `resolve_entry` is ever called) is never canonicalized or
/// validated (REQ-002 / CHK-022).
fn gate_and_resolve_entry(
    entry: &ManagedEntry,
    module_path: &Utf8Path,
    home: &Utf8Path,
    engine: &Engine,
    resolver: &Resolver,
) -> Result<Option<ResolvedEntry>, EngineError> {
    if let Some(expr) = entry.when.as_deref()
        && !engine.eval_when(expr, resolver)?
    {
        return Ok(None);
    }
    Ok(Some(resolve_entry(entry, module_path, home)?))
}

/// Canonicalize one managed entry's source and resolve its targets under
/// `module_path` and `home`, then validate the canonical source's existence
/// and kind against the entry's declared table (REQ-002, step 3 of the
/// REQ-009 order). The source is canonicalized through the filesystem; each
/// target is resolved by *declared location* via [`resolve_location`] so a
/// symlink already occupying the target is never followed back to the source.
/// The file/directory order and the entry-index space are imposed by the
/// caller; this resolves paths and performs the plan-time source check.
///
/// # Errors
///
/// Returns [`EngineError::SourceNotFound`] when the canonical source does
/// not exist on disk, and [`EngineError::SourceKindMismatch`] when a
/// `[[file]]` entry's source is a directory or a `[[directory]]` entry's
/// source is a file. Both are raised here, in the plan phase, before any
/// mutation. Path canonicalization failures surface as [`EngineError::Path`].
fn resolve_entry(
    entry: &ManagedEntry,
    module_path: &Utf8Path,
    home: &Utf8Path,
) -> Result<ResolvedEntry, EngineError> {
    let source = canonicalize(&module_path.join(&entry.source))?;
    validate_source_kind(&source, entry.kind)?;
    let mut targets = Vec::with_capacity(entry.targets.len());
    for target in &entry.targets {
        let expanded = expand_tilde(target, home);
        // Resolve by declared location, never following a symlink that already
        // occupies the leaf: a prior apply (or a foreign tool's symlink during
        // migration) would otherwise canonicalize the target back to its
        // repository source and the executor would delete the source. See
        // `paths::resolve_location`.
        targets.push(resolve_location(&expanded)?);
    }
    Ok(ResolvedEntry {
        mode: entry.mode,
        source,
        targets,
    })
}

/// Validate a canonical source path against the kind declared by its
/// table-array (REQ-002 / DEC-008).
///
/// `paths::canonicalize` falls back to a *lexical* resolution for a
/// non-existent path, so a missing source does not fail at canonicalization;
/// the existence check is therefore an explicit `symlink_metadata` probe on
/// the canonical source rather than a reliance on canonicalization failing.
/// The kind check (`is_dir` / `is_file`) reads the same already-fetched
/// metadata, so this adds a single `stat`, not a second IO pass. A symlinked
/// source resolves through the metadata follow so the *kind it points at* is
/// what is validated — the same kind the executor will materialize.
///
/// # Errors
///
/// Returns [`EngineError::SourceNotFound`] when the source does not exist,
/// and [`EngineError::SourceKindMismatch`] when a `[[file]]` source is a
/// directory or a `[[directory]]` source is a file.
fn validate_source_kind(source: &Utf8Path, kind: EntryKind) -> Result<(), EngineError> {
    // `metadata` follows symlinks, so the kind validated is the kind the
    // source ultimately resolves to — matching what the executor materializes.
    let metadata = match fs_err::metadata(source) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(EngineError::SourceNotFound {
                path: source.to_path_buf(),
            });
        }
        Err(err) => {
            return Err(EngineError::Path(crate::paths::PathError::Filesystem {
                path: source.to_path_buf(),
                source: err,
            }));
        }
    };

    match kind {
        EntryKind::File if metadata.is_dir() => Err(EngineError::SourceKindMismatch {
            path: source.to_path_buf(),
            found: "directory",
            declared_table: "[[file]]",
            expected_table: "[[directory]]",
        }),
        EntryKind::Directory if !metadata.is_dir() => Err(EngineError::SourceKindMismatch {
            path: source.to_path_buf(),
            found: "file",
            declared_table: "[[directory]]",
            expected_table: "[[file]]",
        }),
        EntryKind::File | EntryKind::Directory => Ok(()),
    }
}

/// Placeholder disposition threaded onto every planned operation and
/// recorded target by this task (T-002). T-002 is field-only and
/// behavior-neutral: it carries the field through the wire types and call
/// sites so the workspace compiles. T-004 replaces this with the real
/// plan-time classification on the operation and durable plan, and T-005
/// records the real per-leaf disposition in the commit.
const PLACEHOLDER_DISPOSITION: Disposition = Disposition::Create;

/// Build the durable [`PlannedOperation`] for one resolved
/// `(mode, source, target)`.
fn planned_operation(mode: FileMode, source: &Utf8Path, target: &Utf8Path) -> PlannedOperation {
    match mode {
        // A `[[directory]]` `symlink` (the atomic whole-directory
        // `SymlinkDir`) maps to the same durable symlink op shape as a
        // `[[file]]` `symlink`. `SymlinkTree` is the clearly-marked
        // dispatch point T-007's per-leaf executor fills in; until then it
        // shares the symlink op shape so the plan is well-formed.
        FileMode::Symlink | FileMode::SymlinkDir | FileMode::SymlinkTree => {
            PlannedOperation::symlink(source.as_str(), target.as_str(), PLACEHOLDER_DISPOSITION)
        }
        FileMode::Copy | FileMode::CopyTree => {
            PlannedOperation::copy(source.as_str(), target.as_str(), PLACEHOLDER_DISPOSITION)
        }
        FileMode::TemplateRender => {
            PlannedOperation::render(source.as_str(), target.as_str(), PLACEHOLDER_DISPOSITION)
        }
    }
}

/// Execute a [`ResolvedPlan`] against the filesystem.
///
/// Takes the exclusive lock, recovers any orphan plan under that held
/// lock, flushes the
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
    policy: LockPolicy,
) -> Result<ApplyResult, EngineError> {
    let journal_dir = resolved.journal_dir();
    let backups_dir = resolved.backups_dir();
    let template_engine = Engine::new();

    // Whether this run reaps targets a prior apply committed that the current
    // plan no longer manages (REQ-007 / REQ-003). A full `apply` (`Blocking`)
    // and a watcher re-apply (`NonBlocking`) reconcile the whole plan, so they
    // reap. The `Held` path is a surgical single-target re-journal driven by
    // `patina remove` / `patina promote` under a caller-held lock: those
    // commands intentionally convert one managed target into an owned regular
    // file and drop its entry, so reaping would delete the very file they just
    // promoted — they must not reap.
    let reap = !matches!(policy, LockPolicy::Held(_));

    // Resolve the exclusive lock per policy BEFORE any filesystem
    // mutation, including orphan recovery. On the `NonBlocking`
    // contention path this returns early — before `recover_orphans` and
    // the plan flush below — so a contended attempt mutates nothing
    // (no recovery, no plan, no COMMIT, no backup), upholding REQ-030's
    // zero-write guarantee. Recovering only under the held lock also
    // prevents a second apply from reversing a live in-flight apply's
    // operations (REQ-013 / REQ-030).
    let _guard = match policy {
        LockPolicy::Blocking => acquire_lock(
            &resolved.lock_path(),
            LockKind::Exclusive,
            exclusive_timeout(),
        )?,
        LockPolicy::NonBlocking => try_acquire_lock(&resolved.lock_path(), LockKind::Exclusive)?,
        LockPolicy::Held(guard) => guard,
    };

    // Recover any prior partial apply, under the held lock, before
    // computing fresh work.
    recover_orphans(&journal_dir, &backups_dir)?;

    // Windows-only symlink-elevation gate (SPEC-0002 REQ-007). Runs after
    // recovery and BEFORE the first backup / materialize, so a plan that
    // needs Developer Mode cannot mutate the filesystem without consent.
    // This is the engine-side backstop: the CLI normally drives the UAC
    // prompt before calling `execute`, so a `RequireElevation` verdict here
    // means the gate was reached without that orchestration — refuse to
    // proceed with a typed signal (REQ-007). On a host that is already
    // elevated, proceed but warn (running Patina elevated is discouraged).
    // On macOS / Linux `HostDevModeProbe` reports `NotWindows`, so the
    // decision is always `Proceed`: no registry read, no early return.
    match decide_symlink_gate(resolved, &HostDevModeProbe::default()) {
        GateDecision::Proceed => {}
        GateDecision::ProceedElevatedWarning => {
            warn!(
                "running Patina elevated to create symbolic links; prefer enabling \
                 Developer Mode (`patina doctor --fix`) and running unelevated"
            );
        }
        GateDecision::RequireElevation => return Err(EngineError::DevModeRequired),
    }

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
    // first. Track completion records (paired with the index of the
    // `[[file]]` entry that produced them) so a post_apply hook failure can
    // reverse them and the commit record can group targets into atomic
    // rollback units (REQ-019).
    let mut completed: Vec<(u32, CompletionRecord)> = Vec::new();
    let mut op_index: u32 = 0;
    for op in &resolved.operations {
        // Use the entry index assigned at plan time over the full declared
        // sequence (files then directories) rather than re-deriving one from
        // operation position, so a `[[file]]` and a `[[directory]]` entry can
        // never collide on an index and rollback groups targets by their
        // declared entry (DEC-009).
        let entry_index = op.entry_index;
        for target in &op.targets {
            backup_before_overwrite(&backups_dir, &resolved.timestamp, target)?;
        }
        let records = materialize(op.mode, &op.source, &op.targets, &template_engine, vars)?;
        for record in records {
            journal.record_progress(op_index)?;
            op_index = op_index.saturating_add(1);
            completed.push((entry_index, record));
        }
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
        // Reap targets a prior apply committed that the current plan no
        // longer manages — a removed entry, a `when` flipped to false
        // (REQ-003), or a deleted `symlink-tree` source leaf (REQ-007). Each
        // orphan's prior bytes are backed up into this run's backup tree
        // before it is removed (REQ-014); a directory is never removed
        // (DEC-005). Runs after the post_apply hooks succeed, so a hook
        // failure rolls back the materializations without having reaped.
        // Skipped on the `Held` path (`patina remove` / `promote`), which
        // re-journals one surgically-modified target and must not reap.
        if reap {
            reap_orphans(resolved, &backups_dir)?;
        }
        let record = build_apply_record(resolved, &completed)?;
        journal.commit(&record, &OsSyncer)?;
        // Retention prunes the oldest backup cycles, then the journal
        // sentinels for exactly those cycles are dropped in lockstep: a
        // commit whose backups are gone can no longer be faithfully reversed
        // (its overwrite-restores are gone), so it must not remain
        // rollback- or status-eligible (REQ-015 / REQ-019). An all-fresh
        // apply writes no backup directory and so is never pruned here —
        // rolling back to it correctly deletes its fresh targets.
        let pruned = gc_retain(&backups_dir, crate::backups::RETENTION_COUNT)?;
        prune_cycles(&journal_dir, &pruned)?;
        Ok(ApplyResult::Applied { warnings })
    }
}

/// Build the [`ApplyRecord`] persisted in this run's COMMIT sentinel from
/// the resolved plan's `last_apply` metadata and the completed
/// materializations. `patina status` (T-017) decodes this to classify the
/// live filesystem against the last committed apply.
///
/// Each completed object becomes one [`ExpectedTarget`]: a symlink records
/// its canonical link target (which is also its source); a copy or render
/// records its canonical source path and a `blake3` hash of the bytes that
/// were just written, read back from the live target so the recorded hash
/// matches exactly what `status` will compute (REQ-029).
fn build_apply_record(
    resolved: &ResolvedPlan,
    completed: &[(u32, CompletionRecord)],
) -> Result<ApplyRecord, EngineError> {
    let vars = &resolved.resolver;
    let last_apply = LastApply {
        at: timestamp_to_rfc3339(&resolved.timestamp),
        user: vars.get("patina.user").unwrap_or_default(),
        host: vars.get("patina.hostname").unwrap_or_default(),
    };

    let mut targets = Vec::with_capacity(completed.len());
    for (entry, record) in completed {
        let entry = *entry;
        let target = record.target.as_str().to_owned();
        match &record.materialization {
            Materialization::Symlink { link_target } => {
                targets.push(ExpectedTarget::Symlink {
                    target,
                    link_target: link_target.as_str().to_owned(),
                    entry,
                    disposition: PLACEHOLDER_DISPOSITION,
                });
            }
            Materialization::Copy | Materialization::Render => {
                let bytes = fs_err::read(&record.target).map_err(|source| {
                    EngineError::Journal(crate::journal::JournalError::Filesystem(source))
                })?;
                targets.push(ExpectedTarget::Content {
                    target,
                    source: record.source.as_str().to_owned(),
                    hash: content_hash(&bytes),
                    entry,
                    disposition: PLACEHOLDER_DISPOSITION,
                });
            }
        }
    }
    Ok(ApplyRecord::new(last_apply, targets))
}

/// Reap targets a prior committed apply materialized that the current plan
/// no longer manages (REQ-007 / REQ-003).
///
/// Reads the last committed [`ApplyRecord`] and the current managed-target
/// set ([`current_managed_targets`], the same `when`-aware /
/// `symlink-tree`-aware set `patina status` classifies against). A recorded
/// target whose [`manage_key`](crate::status::manage_key) is absent from the
/// current set is an orphan: the entry was removed, its `when` flipped false
/// (CHK-019), or — for a `symlink-tree` leaf — its source leaf was deleted
/// (CHK-014, CHK-015). Each orphan still present on disk is backed up into
/// this run's backup tree — the same never-overwrite-without-backup
/// guarantee every mutating path upholds (REQ-014) — and then removed.
///
/// A directory is never removed, even one left empty after its last leaf
/// link is reaped: Patina cannot prove it owns a directory that may also
/// hold files written outside Patina (DEC-005). The check is on the live
/// entry's kind, so an intermediate `symlink-tree` directory survives while
/// its orphaned leaf links are removed.
///
/// # Errors
///
/// Returns an [`EngineError`] when the commit read, the managed-set
/// recomputation, a backup, or a removal fails.
fn reap_orphans(resolved: &ResolvedPlan, backups_dir: &Utf8Path) -> Result<(), EngineError> {
    use crate::status::manage_key;

    let journal_dir = resolved.journal_dir();
    let Some(record) = crate::journal::read_latest_commit(&journal_dir)? else {
        // No prior committed apply: nothing was ever materialized to orphan.
        return Ok(());
    };

    let managed = current_managed_targets()?;

    for expected in &record.targets {
        let target = Utf8PathBuf::from(expected.target());
        if managed.contains(&manage_key(&target)) {
            // Still managed by the current plan: leave it for materialize /
            // status to handle. Never reaped.
            continue;
        }
        // The current plan dropped this target. Only act on one that is still
        // on disk; an already-gone orphan needs no work.
        let Ok(meta) = fs_err::symlink_metadata(&target) else {
            continue;
        };
        // Never remove a directory (DEC-005): Patina cannot prove it owns a
        // directory that may also hold files written by another tool. This
        // is the guard that keeps a `symlink-tree` intermediate directory in
        // place while its orphaned leaf links are reaped.
        if meta.is_dir() {
            continue;
        }
        // Record the prior bytes in a backup before removal (CHK-019 /
        // REQ-014). The stash uses this run's timestamped backup tree, the
        // same one materialize stashes overwrites into.
        backup_before_overwrite(backups_dir, &resolved.timestamp, &target)?;
        remove_target(&target)?;
    }
    Ok(())
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
    completed: &[(u32, CompletionRecord)],
    backups_dir: &Utf8Path,
    timestamp: &str,
) -> Result<(), EngineError> {
    use crate::journal::mirror_backup_path;

    // Reverse in inverse order so later operations are undone first.
    for (_entry, record) in completed.iter().rev() {
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
///
/// Wraps the shared [`crate::fsx::remove_entry`] helper into [`EngineError`].
fn remove_target(target: &Utf8Path) -> Result<(), EngineError> {
    crate::fsx::remove_entry(target)
        .map_err(|source| EngineError::Journal(crate::journal::JournalError::Filesystem(source)))
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

#[cfg(test)]
mod tests {
    //! Unit coverage for the REQ-030 lock-acquisition policy (T-027)
    //! and the REQ-013/REQ-030 acquire-then-recover orphan-safety
    //! reorder (T-028).
    //!
    //! These drive [`execute`] in-process so a `Held` policy can pass a
    //! test-controlled [`LockGuard`] and a `NonBlocking` policy can be
    //! observed returning before any mutation — neither is expressible
    //! through the CLI binary, which cannot share a guard across processes.
    //! Each test builds a minimal empty-operation [`ResolvedPlan`] over a
    //! tempdir state directory, so no repository discovery or process-env
    //! mutation is needed (the workspace forbids `unsafe`, and env mutation
    //! is `unsafe` under edition 2024). An empty plan still flushes a
    //! `<ts>.plan`, commits a `<ts>.COMMIT`, and then deletes the plan —
    //! enough surface to assert the journal side effects the scenarios name.
    //!
    //! CHK-067 (the default `Blocking` policy preserves REQ-021
    //! byte-identical stdout across two `patina apply --yes` runs) is
    //! covered end-to-end through the CLI in
    //! `patina-cli/tests/deterministic_stdout.rs`; that suite already drives
    //! the `Blocking` path, so it is not re-proved here.

    use super::*;
    use crate::error::EngineError;
    use crate::lock::LockError;
    use crate::lock::acquire as acquire_lock;
    use std::time::Duration;
    use tempfile::TempDir;

    const TS: &str = "20260530T120000Z";

    /// Build a synthetic resolved entry with the given mode, a distinct
    /// source tag, and one target per `target_tag`. Lets the ordering /
    /// index tests assert over identifiable paths without touching the
    /// filesystem or repo discovery.
    fn resolved_entry(mode: FileMode, source_tag: &str, target_tags: &[&str]) -> ResolvedEntry {
        ResolvedEntry {
            mode,
            source: Utf8PathBuf::from(format!("/repo/{source_tag}")),
            targets: target_tags
                .iter()
                .map(|t| Utf8PathBuf::from(format!("/home/{t}")))
                .collect(),
        }
    }

    // REQ-009 / DEC-009 ordering: with two `[[file]]` entries and one
    // `[[directory]]` entry, both file operations are emitted before the
    // directory operation, each block in declaration order. The directory
    // entry is the `[[directory]]` `symlink` default (atomic `SymlinkDir`).
    #[test]
    fn files_are_planned_before_directories_in_declaration_order() {
        let files = vec![
            Some(resolved_entry(FileMode::Symlink, "f0", &["f0.target"])),
            Some(resolved_entry(FileMode::Copy, "f1", &["f1.target"])),
        ];
        let directories = vec![Some(resolved_entry(
            FileMode::SymlinkDir,
            "d0",
            &["d0.target"],
        ))];

        let (operations, _resolved) = assemble_plan_operations(files, directories);

        // Three single-target entries → three operations, in files-then-
        // directories declared order.
        let sources: Vec<&str> = operations
            .iter()
            .map(|op| match op {
                PlannedOperation::Symlink { source, .. }
                | PlannedOperation::Copy { source, .. }
                | PlannedOperation::Render { source, .. } => source.as_str(),
            })
            .collect();
        assert_eq!(
            sources,
            vec!["/repo/f0", "/repo/f1", "/repo/d0"],
            "both `[[file]]` operations must precede the `[[directory]]` operation, each in declaration order"
        );
    }

    // REQ-009 / DEC-009 index space: entry indices form a single monotonic
    // sequence across both tables (all `[[file]]` entries, then all
    // `[[directory]]` entries), and no `[[file]]` and `[[directory]]` entry
    // share an index.
    #[test]
    fn entry_indices_are_a_single_monotonic_space_across_both_tables() {
        let files = vec![
            Some(resolved_entry(FileMode::Symlink, "f0", &["f0a", "f0b"])),
            Some(resolved_entry(FileMode::Copy, "f1", &["f1.target"])),
        ];
        let directories = vec![
            Some(resolved_entry(FileMode::SymlinkDir, "d0", &["d0.target"])),
            Some(resolved_entry(FileMode::CopyTree, "d1", &["d1.target"])),
        ];

        let (_operations, resolved) = assemble_plan_operations(files, directories);

        // One resolved op per managed entry (target fan-out lives inside the
        // op), indexed 0..N over the declared file-then-directory sequence.
        let indices: Vec<u32> = resolved.iter().map(|op| op.entry_index).collect();
        assert_eq!(
            indices,
            vec![0, 1, 2, 3],
            "indices must be a gapless monotonic 0..N over files-then-directories"
        );

        // The two file entries own indices 0,1; the two directory entries own
        // 2,3 — disjoint, so no file/directory entry collides on an index.
        let file_indices: Vec<u32> = resolved
            .iter()
            .filter(|op| matches!(op.mode, FileMode::Symlink | FileMode::Copy))
            .map(|op| op.entry_index)
            .collect();
        let dir_indices: Vec<u32> = resolved
            .iter()
            .filter(|op| matches!(op.mode, FileMode::SymlinkDir | FileMode::CopyTree))
            .map(|op| op.entry_index)
            .collect();
        assert_eq!(file_indices, vec![0, 1]);
        assert_eq!(dir_indices, vec![2, 3]);
        assert!(
            file_indices.iter().all(|fi| !dir_indices.contains(fi)),
            "no `[[file]]` and `[[directory]]` entry may share an entry index"
        );
    }

    // REQ-009 / DEC-004: a `when`-false entry (a `None` slot) occupies its
    // index in the declared sequence but emits no operation and no resolved
    // op, so the surviving entries keep the indices they would have had if
    // the gated-off entry were present-but-empty rather than compacted away.
    #[test]
    fn when_false_entry_occupies_its_index_but_emits_no_operation() {
        // Declared file sequence: f0 (survives), f1 (when-false → None),
        // f2 (survives). One surviving directory entry d0.
        let files = vec![
            Some(resolved_entry(FileMode::Symlink, "f0", &["f0.target"])),
            None,
            Some(resolved_entry(FileMode::Copy, "f2", &["f2.target"])),
        ];
        let directories = vec![Some(resolved_entry(
            FileMode::SymlinkDir,
            "d0",
            &["d0.target"],
        ))];

        let (operations, resolved) = assemble_plan_operations(files, directories);

        // The gated-off f1 contributes no operation: only f0, f2, d0 plan.
        let sources: Vec<&str> = operations
            .iter()
            .map(|op| match op {
                PlannedOperation::Symlink { source, .. }
                | PlannedOperation::Copy { source, .. }
                | PlannedOperation::Render { source, .. } => source.as_str(),
            })
            .collect();
        assert_eq!(
            sources,
            vec!["/repo/f0", "/repo/f2", "/repo/d0"],
            "a `when`-false entry must emit no planned operation"
        );

        // f1 still consumed index 1, so f2 keeps index 2 and d0 keeps index 3
        // — the indices are not compacted to fill the gap.
        let indices: Vec<u32> = resolved.iter().map(|op| op.entry_index).collect();
        assert_eq!(
            indices,
            vec![0, 2, 3],
            "the gated-off entry occupies index 1, leaving a gap rather than \
             renumbering the survivors"
        );
    }

    /// REQ-002: a `[[file]]` entry whose canonical source is a directory is
    /// a plan-time kind mismatch directing the author to `[[directory]]`. The
    /// error names the source path and both tables so the message is
    /// actionable.
    #[test]
    fn file_entry_with_directory_source_is_a_kind_mismatch() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let dir_source = root.join("confdir");
        fs_err::create_dir(&dir_source).expect("mkdir source");

        let err = validate_source_kind(&dir_source, EntryKind::File)
            .expect_err("a directory source under `[[file]]` must be rejected");

        assert!(
            matches!(
                &err,
                EngineError::SourceKindMismatch {
                    path,
                    found: "directory",
                    declared_table: "[[file]]",
                    expected_table: "[[directory]]",
                } if *path == dir_source
            ),
            "a `[[file]]` directory source must yield a mismatch naming the source, \
             `directory`, `[[file]]`, and `[[directory]]`, got: {err:?}"
        );
    }

    /// REQ-002 (symmetric): a `[[directory]]` entry whose canonical source is
    /// a regular file is a kind mismatch directing the author to `[[file]]`.
    #[test]
    fn directory_entry_with_file_source_is_a_kind_mismatch() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let file_source = root.join("gitconfig");
        fs_err::write(&file_source, "[user]\n").expect("write source");

        let err = validate_source_kind(&file_source, EntryKind::Directory)
            .expect_err("a file source under `[[directory]]` must be rejected");

        assert!(
            matches!(
                &err,
                EngineError::SourceKindMismatch {
                    path,
                    found: "file",
                    declared_table: "[[directory]]",
                    expected_table: "[[file]]",
                } if *path == file_source
            ),
            "a `[[directory]]` file source must yield a mismatch naming the source, \
             `file`, `[[directory]]`, and `[[file]]`, got: {err:?}"
        );
    }

    /// REQ-002: a source that does not exist on disk is a "source not found"
    /// error, not a kind mismatch. `paths::canonicalize` resolves a missing
    /// path lexically, so the existence check is this explicit probe.
    #[test]
    fn absent_source_is_source_not_found() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let ghost = root.join("ghost");

        let err = validate_source_kind(&ghost, EntryKind::File)
            .expect_err("an absent source must be rejected");

        assert!(
            matches!(&err, EngineError::SourceNotFound { path } if *path == ghost),
            "an absent source must yield SourceNotFound naming the source, got: {err:?}"
        );
    }

    /// REQ-002: a `[[file]]` source that is a file and a `[[directory]]`
    /// source that is a directory both validate cleanly — the matched-kind
    /// path raises no error.
    #[test]
    fn matching_kinds_validate_cleanly() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let file_source = root.join("zshrc");
        fs_err::write(&file_source, "export EDITOR=vim\n").expect("write file source");
        let dir_source = root.join("mpv");
        fs_err::create_dir(&dir_source).expect("mkdir dir source");

        validate_source_kind(&file_source, EntryKind::File)
            .expect("a file source under `[[file]]` validates");
        validate_source_kind(&dir_source, EntryKind::Directory)
            .expect("a directory source under `[[directory]]` validates");
    }

    /// A tempdir state directory plus a minimal empty-operation plan that
    /// resolves its journal / backups / lock under that directory.
    struct Scene {
        _temp: TempDir,
        resolved: ResolvedPlan,
    }

    impl Scene {
        fn new() -> Self {
            let temp = TempDir::new().expect("tempdir");
            let state_dir = Utf8Path::from_path(temp.path())
                .expect("utf8 temp path")
                .to_owned();
            // The journal directory must exist before the plan flush; the
            // production path creates the state tree during resolution.
            fs_err::create_dir_all(state_dir.join("journal")).expect("mkdir journal");

            let resolved = ResolvedPlan {
                repo_root: state_dir.join("repo"),
                profile: String::new(),
                plan: Plan::new(Vec::new()),
                operations: Vec::new(),
                hooks: Vec::new(),
                state_dir,
                host_os: HostOs::current(),
                timestamp: TS.to_owned(),
                resolver: Resolver::new(Builtins::current()),
            };
            Self {
                _temp: temp,
                resolved,
            }
        }

        fn lock_path(&self) -> Utf8PathBuf {
            self.resolved.lock_path()
        }

        fn journal_file_exists(&self, suffix: &str) -> bool {
            self.resolved
                .journal_dir()
                .join(format!("{TS}{suffix}"))
                .exists()
        }

        /// Plant an orphan `<orphan_ts>.plan` and `<orphan_ts>.progress` in
        /// the journal — a prior crashed apply with no COMMIT / `ROLLED_BACK`
        /// sibling, the shape `recover_orphans` would otherwise reverse.
        /// Returns the two paths so a test can assert their bytes are left
        /// untouched.
        fn plant_orphan(&self, orphan_ts: &str) -> (Utf8PathBuf, Utf8PathBuf, Vec<u8>) {
            let journal = self.resolved.journal_dir();
            let plan = journal.join(format!("{orphan_ts}{}", crate::journal::PLAN_SUFFIX));
            let progress = journal.join(format!("{orphan_ts}{}", crate::journal::PROGRESS_SUFFIX));
            let plan_bytes = b"orphan-plan-bytes".to_vec();
            fs_err::write(&plan, &plan_bytes).expect("write orphan plan");
            fs_err::write(&progress, b"orphan-progress").expect("write orphan progress");
            (plan, progress, plan_bytes)
        }
    }

    // CHK-065: under the NonBlocking policy against a lock held by a
    // test-controlled guard, the apply returns the typed contention error
    // and writes no `<ts>.plan` or `<ts>.COMMIT`.
    #[tokio::test]
    async fn non_blocking_apply_on_contended_lock_errors_and_writes_no_journal() {
        let scene = Scene::new();
        let held = acquire_lock(
            &scene.lock_path(),
            LockKind::Exclusive,
            Duration::from_secs(5),
        )
        .expect("hold the exclusive lock for the contended apply");

        let result = execute(
            &scene.resolved,
            &ApplyRequest::default(),
            LockPolicy::NonBlocking,
        )
        .await;

        assert!(
            matches!(
                &result,
                Err(EngineError::Lock(LockError::Contended {
                    kind: LockKind::Exclusive,
                    ..
                }))
            ),
            "a NonBlocking apply against a held lock must return the typed contention error, got {result:?}"
        );
        assert!(
            !scene.journal_file_exists(crate::journal::PLAN_SUFFIX),
            "contended NonBlocking apply must not write a plan file"
        );
        assert!(
            !scene.journal_file_exists(crate::journal::COMMIT_SUFFIX),
            "contended NonBlocking apply must not write a COMMIT file"
        );

        drop(held);
    }

    // CHK-066: under the Held policy with the caller's own exclusive guard,
    // the apply completes (it does not time out against its own lock) and a
    // `<ts>.COMMIT` record is present.
    #[tokio::test]
    async fn held_policy_applies_with_callers_guard_and_commits() {
        let scene = Scene::new();
        let guard = acquire_lock(
            &scene.lock_path(),
            LockKind::Exclusive,
            Duration::from_secs(5),
        )
        .expect("caller acquires the exclusive lock up front");

        let result = execute(
            &scene.resolved,
            &ApplyRequest::default(),
            LockPolicy::Held(guard),
        )
        .await
        .expect("apply under Held policy must not error against its own lock");

        assert!(
            matches!(result, ApplyResult::Applied { .. }),
            "the Held-policy apply committed, got {result:?}"
        );
        assert!(
            scene.journal_file_exists(crate::journal::COMMIT_SUFFIX),
            "a committed apply leaves a `<ts>.COMMIT` record"
        );
        assert!(
            !scene.journal_file_exists(crate::journal::PLAN_SUFFIX),
            "the plan file is removed after COMMIT"
        );
    }

    // CHK-068: under the NonBlocking policy against a lock held by a
    // test-controlled guard AND with a pending orphan `<ts>.plan` in the
    // journal, the apply returns the typed contention error, leaves the
    // orphan plan and its progress untouched (recovery never runs because
    // the lock is resolved first), and writes no new plan / COMMIT / backup.
    // This is the regression that the acquire-then-recover reorder fixes.
    #[tokio::test]
    async fn non_blocking_contention_leaves_pending_orphan_untouched() {
        const ORPHAN_TS: &str = "20260529T090000Z";
        let scene = Scene::new();
        let (orphan_plan, orphan_progress, orphan_bytes) = scene.plant_orphan(ORPHAN_TS);

        let held = acquire_lock(
            &scene.lock_path(),
            LockKind::Exclusive,
            Duration::from_secs(5),
        )
        .expect("hold the exclusive lock for the contended apply");

        let result = execute(
            &scene.resolved,
            &ApplyRequest::default(),
            LockPolicy::NonBlocking,
        )
        .await;

        assert!(
            matches!(
                &result,
                Err(EngineError::Lock(LockError::Contended {
                    kind: LockKind::Exclusive,
                    ..
                }))
            ),
            "a NonBlocking apply against a held lock must return the typed contention error, got {result:?}"
        );

        // The orphan is left exactly as planted — not reversed, not deleted.
        assert!(
            orphan_plan.exists() && orphan_progress.exists(),
            "the pending orphan plan and progress must survive a contended attempt"
        );
        assert_eq!(
            fs_err::read(&orphan_plan).expect("read orphan plan"),
            orphan_bytes,
            "the orphan plan bytes must be untouched (recovery never ran)"
        );

        // No new journal records and no backup were written by the contended
        // attempt.
        assert!(
            !scene.journal_file_exists(crate::journal::PLAN_SUFFIX),
            "contended NonBlocking apply must not write a new plan file"
        );
        assert!(
            !scene.journal_file_exists(crate::journal::COMMIT_SUFFIX),
            "contended NonBlocking apply must not write a COMMIT file"
        );
        assert!(
            !scene.resolved.backups_dir().exists(),
            "contended NonBlocking apply must not write any backup"
        );

        drop(held);
    }

    /// A `symlink-tree` entry expands to one managed key per *live* source
    /// leaf, mirrored under the declared target the same way the executor
    /// materializes them — so a source leaf that no longer exists contributes
    /// no key and its recorded target leaf will classify ORPHANED (CHK-014).
    #[test]
    fn insert_managed_targets_expands_symlink_tree_per_live_leaf() {
        use crate::status::manage_key;

        let td = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf8 temp path");
        // The module's source directory `d/` holds `a.conf` and `sub/b.conf`.
        let module = root.join("mod");
        let source = module.join("d");
        fs_err::create_dir_all(source.join("sub")).expect("mkdir sub");
        fs_err::write(source.join("a.conf"), b"a").expect("write a");
        fs_err::write(source.join("sub").join("b.conf"), b"b").expect("write b");
        // The target lives under the tempdir (real, canonicalizable parent).
        let target = root.join("dest");
        fs_err::create_dir_all(&target).expect("mkdir target");

        let entry = ManagedEntry {
            kind: EntryKind::Directory,
            mode: FileMode::SymlinkTree,
            source: Utf8PathBuf::from("d"),
            targets: vec![target.clone()],
            when: None,
        };

        let mut got = BTreeSet::new();
        insert_managed_targets(&entry, &module, &root, &mut got);

        let expected: BTreeSet<Utf8PathBuf> = [
            manage_key(&target.join("a.conf")),
            manage_key(&target.join("sub").join("b.conf")),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            got, expected,
            "a symlink-tree entry must contribute exactly one key per live leaf"
        );

        // Delete a source leaf: it drops out of the managed set, so its
        // recorded target leaf would now classify ORPHANED.
        fs_err::remove_file(source.join("sub").join("b.conf")).expect("delete leaf");
        let mut after = BTreeSet::new();
        insert_managed_targets(&entry, &module, &root, &mut after);
        assert_eq!(
            after,
            [manage_key(&target.join("a.conf"))].into_iter().collect(),
            "a deleted source leaf must no longer be a managed target"
        );
    }

    /// A non-`symlink-tree` entry contributes its declared target(s)
    /// directly, with no source walk — the prior behaviour for `[[file]]`
    /// symlink/copy/template and atomic `[[directory]]` symlink entries.
    #[test]
    fn insert_managed_targets_inserts_declared_targets_for_non_tree_modes() {
        use crate::status::manage_key;

        let td = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf8 temp path");
        let module = root.join("mod");
        fs_err::create_dir_all(&module).expect("mkdir module");
        let t1 = root.join("a");
        let t2 = root.join("b");

        let entry = ManagedEntry {
            kind: EntryKind::File,
            mode: FileMode::Symlink,
            source: Utf8PathBuf::from("zshrc"),
            targets: vec![t1.clone(), t2.clone()],
            when: None,
        };

        let mut got = BTreeSet::new();
        insert_managed_targets(&entry, &module, &root, &mut got);

        let expected: BTreeSet<Utf8PathBuf> =
            [manage_key(&t1), manage_key(&t2)].into_iter().collect();
        assert_eq!(got, expected);
    }
}
