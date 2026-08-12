//! End-to-end `patina apply` orchestration.
//!
//! The file-mode executors ([`crate::apply::materialize`]), the hook
//! runner ([`crate::apply::hooks`]), the plan journal
//! ([`crate::journal`]), and the backup tree ([`crate::backups`]) each
//! own one slice of an apply. This module is the orchestrator that wires
//! them together into the two-phase shape described here:
//!
//! 1. [`plan`] resolves the repository, enumerates modules, parses every module
//!    manifest, resolves the active profile and variable context, canonicalizes
//!    each `[[file]]` entry's source and targets, and produces a
//!    [`ResolvedPlan`]. Planning performs **no** filesystem mutation, so the
//!    CLI can render a diff and (in a non-TTY, or with `--json` and no `--yes`)
//!    exit without touching the user's `$HOME`.
//! 2. [`execute`] takes the [`ResolvedPlan`] and mutates. It recovers any
//!    orphan plan, takes the exclusive lock, and flushes the journal. It then
//!    runs `pre_apply` hooks, materializes every operation (backing up each
//!    pre-existing target first), and runs `post_apply` hooks. Finally it
//!    either commits or rolls the file operations back.
//!
//! The CLI (`patina-cli`) owns the diff rendering, the TTY prompt, the
//! `--pager` plumbing, and the JSON envelope; this module owns the
//! engine semantics so those presentation concerns never reach into the
//! subsystem internals.

use crate::apply::CompletionRecord;
use crate::apply::ForceDeploy;
use crate::apply::HookOutcome;
use crate::apply::LeafWrite;
use crate::apply::Materialization;
use crate::apply::ResolvedHook;
use crate::apply::hooks;
use crate::apply::materialize;
use crate::apply::materialize_tree;
use crate::backups::backup_before_overwrite;
use crate::backups::gc_retain;
use crate::config::EntryKind;
use crate::config::FileMode;
use crate::config::HookEntry;
use crate::config::HookEvent;
use crate::config::ManagedEntry;
use crate::config::RemoteName;
use crate::config::RemoteSpec;
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
use crate::remote::cache as remote_cache;
use crate::remote::lockfile::Lockfile;
use crate::remote::lockfile::lockfile_path;
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
use std::collections::BTreeMap;
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

/// How [`execute`] obtains the exclusive advisory lock guarding the apply.
///
/// The default ([`LockPolicy::Blocking`]) acquires exclusive with
/// [`exclusive_timeout`], mapping a timeout to exit code 4. The other two
/// strategies let callers outside the CLI's `apply` path drive an apply
/// differently:
///
/// - [`LockPolicy::NonBlocking`]: make a single non-blocking attempt and, on
///   contention, return [`crate::lock::LockError::Contended`] before any
///   filesystem mutation. The watcher uses this to skip a reapply while a CLI
///   run holds the lock.
/// - [`LockPolicy::Held`]: reuse a guard the caller already acquired, acquiring
///   nothing further. The `remove` / `promote` commands use this to re-journal
///   while already holding the exclusive lock, without deadlocking against
///   their own held lock.
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

/// The plan-time classification of one declared target the source fans out
/// to.
///
/// For a single-target mode (`symlink`, `copy`, `template`, `symlink-dir`)
/// the `aggregate` is simply that target's own disposition and `leaves` is
/// empty. For a tree mode (`copy-tree`, `symlink-tree`) the `aggregate` is
/// the per-op aggregate. It is `Unchanged` iff every materialized leaf is
/// `Unchanged`, `Create` iff the target directory is absent, and `Update`
/// otherwise. `leaves` then carries the per-leaf disposition that the
/// execute write-skip and the per-leaf diff / `--json` reporting consume.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TargetDisposition {
    /// The whole-target disposition recorded on the durable
    /// [`PlannedOperation`] (the per-op aggregate for tree modes).
    pub aggregate: Disposition,
    /// Per-leaf dispositions for a tree mode, keyed by the leaf's path
    /// relative to the declared target directory; empty for single-target
    /// modes.
    pub leaves: Vec<LeafDisposition>,
}

/// One materialized leaf of a tree-mode target with its plan-time
/// disposition.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LeafDisposition {
    /// The leaf's path relative to the declared target directory, in the
    /// same `walk_files` order the executor materializes leaves.
    pub relative: Utf8PathBuf,
    /// How this leaf relates to the live filesystem.
    pub disposition: Disposition,
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
    /// Plan-time classification of each declared target, parallel to and
    /// aligned with [`targets`](Self::targets). The
    /// execute write-skip and the per-entry diff / `--json`
    /// reporting read these so the live filesystem read
    /// happens once, at plan time.
    pub dispositions: Vec<TargetDisposition>,
    /// Index of the managed entry that produced this operation. Plan time
    /// assigns it over the full declared sequence, as a single monotonic
    /// `u32` space. That sequence is every `[[file]]` entry across all
    /// modules first, then every `[[directory]]` entry. [`execute`] records
    /// this index on each [`ExpectedTarget`] rather than re-deriving one from
    /// operation position. A `[[file]]` and a `[[directory]]` entry can
    /// therefore never collide on an index, and per-entry atomic rollback
    /// groups targets by their declared entry.
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
    /// The names of every `[[remote]]` the root manifest declares, in
    /// declaration order. Carried so the stale-pin and cache sweeps work from
    /// the same parse this plan was built from.
    pub remote_names: Vec<RemoteName>,
    /// The `(name, rev)` of every declared remote's pin, or `None` when
    /// planning never read the lockfile because no active entry selected a
    /// remote. The cache sweep retains these checkouts regardless of journal
    /// reachability, and treats `None` as "leave every declared remote alone".
    pub remote_pins: Option<Vec<(RemoteName, String)>>,
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
        /// Whether this invocation was a full no-op: every target classified
        /// `Unchanged` and nothing to reap, so the apply wrote nothing to disk
        /// and the prior commit remains authoritative. The CLI uses
        /// this to print the deterministic "up to date" line instead of
        /// "Applied." A normal committed apply sets it `false`.
        up_to_date: bool,
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
/// [`ResolvedPlan`].
///
/// Mutates no user target. An entry naming a remote is the one exception to
/// touching the filesystem at all: that remote's pinned checkout is fetched and
/// materialized under the state directory as the entry resolves, so a cold
/// cache fails planning rather than half-way through a diff. Everything it
/// writes stays inside `<state>/remotes/`; no managed target is touched.
///
/// # Errors
///
/// Returns an [`EngineError`] when repository discovery, module
/// enumeration, manifest parsing, state-directory resolution, profile
/// resolution, variable ingestion, remote-checkout materialization, or path
/// canonicalization fails.
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
        remotes,
    } = build_planning_context(&request.cli_overrides)?;
    let mut registry = RemoteRegistry::new(&remotes, &repo_root, &state_dir, CachePolicy::Fetch);

    // Resolve every managed entry into its canonical source/targets, kept
    // in two ordered buckets, `[[file]]` entries and `[[directory]]`
    // entries, each in declaration order across all modules as the modules
    // are iterated. Emitting from these buckets files-then-directories
    // gives the single deterministic order while a managed
    // entry's canonicalization stays where it always was (per-module, under
    // that module's tilde/home context).
    //
    // Each bucket slot is an `Option<ResolvedEntry>`: a `when`-false entry
    // contributes `None`, so it still occupies its position in the declared
    // sequence (and thus its `entry_index`) but emits no operation
    // and no diff line. The `when` gate runs at the top of the
    // per-entry body, before `resolve_entry` canonicalizes the source, so a
    // gated-off entry whose source is absent or wrong-kind on this OS is
    // never canonicalized or validated (ordering).
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
                module,
                &mut registry,
                &home,
                &engine,
                &resolver,
            )?);
        }
        for entry in &config.directories {
            directory_entries.push(gate_and_resolve_entry(
                entry,
                module,
                &mut registry,
                &home,
                &engine,
                &resolver,
            )?);
        }

        hooks.extend(config.hooks.iter().cloned());
    }

    let expanded = expand_claims(&file_entries, &directory_entries)?;
    let claims: Vec<crate::apply::TargetClaim<'_>> =
        expanded.iter().map(ClaimTargets::claim).collect();
    crate::apply::collisions::validate_targets(&claims)?;

    let (operations, resolved_ops) = assemble_plan_operations(file_entries, directory_entries);
    let remote_names: Vec<RemoteName> = remotes.iter().map(|spec| spec.name.clone()).collect();
    let remote_pins = registry.pins();

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
        remote_names,
        remote_pins,
    })
}

/// The repository, profile, variable resolver, and `when` engine shared by
/// the two passes that must agree on which entries are active and how their
/// `when` predicates resolve: [`plan`] (which builds the apply plan) and
/// [`current_managed_targets`] (which recomputes the managed-target set for
/// `patina status` and the apply-time orphan reap).
///
/// Everything up to the per-module entry loop, but not including it, is
/// identical between the two passes (the repo-shared / per-profile
/// layer pushes, the active-profile resolution, the shared `MiniJinja`
/// engine). Sharing it here gives the `when` gate the same variable context
/// in planning and in status. An entry that plans on this host is therefore
/// the same entry status counts as managed, and the same entry the reap
/// leaves alone.
struct PlanningContext {
    repo_root: Utf8PathBuf,
    state_dir: Utf8PathBuf,
    home: Utf8PathBuf,
    profile: crate::profile::Resolution,
    resolver: Resolver,
    engine: Engine,
    modules: Vec<crate::discovery::ModuleHandle>,
    remotes: Vec<RemoteSpec>,
}

/// Whether resolving an entry's source root may fill a cold cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CachePolicy {
    /// Fetch the pinned rev when it is missing. The planning path: `apply` must
    /// converge this machine to the committed lock.
    Fetch,
    /// Never touch the network. The `patina status` path, which is read-only
    /// and must not stall on an unreachable remote; an unmaterialized
    /// checkout is reported as no source root at all.
    ReadOnly,
}

/// Where one entry's `source` resolves from.
///
/// For an entry with no `remote` key this is simply its module's directory.
/// For an entry naming a remote it is the
/// immutable checkout of the rev `patina.lock` pins, so the module directory
/// contributes only the manifest that declared the entry. That is also why a
/// `patina.toml` inside a checkout is inert: module discovery walks the
/// repository, never the cache.
struct EntryOrigin {
    /// The directory the entry's `source` is joined onto, or `None` for a
    /// remote whose checkout is not materialized on this machine (only
    /// reachable under [`CachePolicy::ReadOnly`]).
    source_root: Option<Utf8PathBuf>,
    /// The name of the remote the entry selected, or `None` when its source is
    /// the module's own tree. `Some` drives the containment check that keeps a
    /// remote entry's source from resolving outside its checkout; a local
    /// module's own tree is trusted.
    remote: Option<String>,
}

/// The root manifest's `[[remote]]` declarations, resolved to checkouts on
/// demand.
///
/// Pins are global and checkouts are local: the lockfile is a
/// machine-independent statement about what every machine applies, while
/// materializing one is a per-machine consequence of an entry actually
/// selecting that remote here. So the lockfile is read on the first selection
/// of any remote, a checkout is materialized on the first selection of that
/// remote, and both are memoized. A remote no active entry names on this host
/// costs no `patina.lock` read and no fetch, which is what keeps a
/// `when`-false entry from pulling the network into a plan.
struct RemoteRegistry<'a> {
    /// Every declaration, in root-manifest order.
    declared: &'a [RemoteSpec],
    /// Canonical repository root; `patina.lock` sits directly inside it.
    repo_root: &'a Utf8Path,
    /// Per-machine state directory, whose `remotes/` subtree holds checkouts.
    state_dir: &'a Utf8Path,
    /// Whether resolving may fill a cold cache.
    policy: CachePolicy,
    /// The committed pins, read on first use.
    lockfile: Option<Lockfile>,
    /// Resolved checkout per remote identity, so a remote several entries
    /// select is materialized once however each of them spells its name.
    checkouts: BTreeMap<String, Option<Utf8PathBuf>>,
}

impl<'a> RemoteRegistry<'a> {
    /// A registry over the root manifest's declarations.
    fn new(
        declared: &'a [RemoteSpec],
        repo_root: &'a Utf8Path,
        state_dir: &'a Utf8Path,
        policy: CachePolicy,
    ) -> Self {
        Self {
            declared,
            repo_root,
            state_dir,
            policy,
            lockfile: None,
            checkouts: BTreeMap::new(),
        }
    }

    /// Resolve where `entry`'s source lives, materializing a checkout if this
    /// is the first entry to select that remote.
    ///
    /// # Errors
    ///
    /// Returns an [`EngineError`] when the entry names a remote the root
    /// manifest does not declare. Under [`CachePolicy::Fetch`] it also errors
    /// when that remote has no pin, or when its pinned rev is neither cached
    /// nor fetchable. The no-pin message points at
    /// `patina remote update <name>`. All three are raised at plan time, so
    /// nothing is partially applied.
    fn origin(
        &mut self,
        entry: &ManagedEntry,
        module_dir: &Utf8Path,
    ) -> Result<EntryOrigin, EngineError> {
        let Some(name) = entry.remote.as_deref() else {
            return Ok(EntryOrigin {
                source_root: Some(module_dir.to_path_buf()),
                remote: None,
            });
        };
        Ok(EntryOrigin {
            source_root: self.checkout(name)?,
            remote: Some(name.to_owned()),
        })
    }

    /// The checkout directory for the remote called `name`, memoized.
    ///
    /// Memoized under the folded identity rather than the spelling this entry
    /// wrote, so two entries selecting one remote through two spellings
    /// resolve it once.
    fn checkout(&mut self, name: &str) -> Result<Option<Utf8PathBuf>, EngineError> {
        let key = crate::config::remote::name_key(name);
        if let Some(resolved) = self.checkouts.get(&key) {
            return Ok(resolved.clone());
        }
        let resolved = self.materialize(name)?;
        self.checkouts.insert(key, resolved.clone());
        Ok(resolved)
    }

    /// Fetch and check out `name`'s pinned rev (or, read-only, report whether
    /// it is already here). The returned directory is canonical, so the
    /// containment check downstream needs no further filesystem resolution.
    ///
    /// Under [`CachePolicy::ReadOnly`] every failure shape degrades to "no
    /// source root" instead of erroring. Status reports on what a prior apply
    /// left behind. Losing the whole drift report over an undeclared remote,
    /// a malformed lockfile, or a missing pin would hide the entries that are
    /// perfectly reportable. The apply plan raises all three.
    fn materialize(&mut self, name: &str) -> Result<Option<Utf8PathBuf>, EngineError> {
        // Reborrowed from `self` so the lockfile can be loaded into `self`
        // below while `spec` stays alive.
        let declared = self.declared;
        let Some(spec) = declared.iter().find(|spec| spec.name.matches(name)) else {
            if self.policy == CachePolicy::ReadOnly {
                tracing::warn!(
                    remote = name,
                    "an entry names a remote no [[remote]] table declares; drift for it is \
                     unknown"
                );
                return Ok(None);
            }
            return Err(crate::remote::RemoteError::undeclared_remote(name).into());
        };

        if self.policy == CachePolicy::ReadOnly {
            let pin = match self.lockfile() {
                Ok(lockfile) => lockfile.get(&spec.name).cloned(),
                Err(error) => {
                    tracing::warn!(
                        remote = name,
                        %error,
                        "patina.lock could not be read; drift for remote-backed entries is \
                         unknown"
                    );
                    None
                }
            };
            let Some(pin) = pin else {
                return Ok(None);
            };
            let dir = remote_cache::checkout_dir(self.state_dir, &spec.name, &pin.rev);
            if !dir.is_dir() {
                return Ok(None);
            }
            return Ok(Some(canonicalize(&dir)?));
        }

        let Some(pin) = self.lockfile()?.get(&spec.name).cloned() else {
            return Err(crate::remote::RemoteError::missing_lock_entry(spec.name.as_str()).into());
        };

        let checkout = remote_cache::ensure_checkout(
            self.state_dir,
            &spec.name,
            &spec.url,
            spec.git_ref.as_deref(),
            &pin.rev,
        )
        .map_err(|err| err.into_cold_cache(spec.name.as_str(), &pin.rev))?;
        Ok(Some(canonicalize(&checkout)?))
    }

    /// The `(name, rev)` of every declared remote's pin, or `None` when this
    /// run never read the lockfile.
    ///
    /// The distinction is what keeps the cache sweep honest. "No remote is
    /// pinned" and "which remotes are pinned was never established" both look
    /// like an empty list. Deleting a checkout on the second would take out
    /// the one the next run materializes.
    fn pins(&self) -> Option<Vec<(RemoteName, String)>> {
        Some(declared_pins(
            self.declared.iter().map(|spec| &spec.name),
            self.lockfile.as_ref()?,
        ))
    }

    /// The committed pins, reading `patina.lock` on first use.
    ///
    /// A run where no active entry selects a remote never calls this, so a
    /// stray or malformed `patina.lock` never fails a plain local apply or
    /// status.
    fn lockfile(&mut self) -> Result<&Lockfile, EngineError> {
        let repo_root = self.repo_root;
        let slot = &mut self.lockfile;
        let lockfile = match slot {
            Some(lockfile) => lockfile,
            None => slot.insert(Lockfile::load(&lockfile_path(repo_root))?),
        };
        Ok(lockfile)
    }
}

/// Re-read the pins for a plan that never loaded the lockfile, or `None` when
/// the file cannot be read.
///
/// A remote declared but selected by no active entry still has a pin the next
/// run will materialize, so the sweep must learn it from somewhere. Failing to
/// read is not an error here: it only means no declared remote's cache is
/// swept this time.
fn reread_pins(resolved: &ResolvedPlan) -> Option<Vec<(RemoteName, String)>> {
    if resolved.remote_names.is_empty() {
        return Some(Vec::new());
    }
    let lockfile = Lockfile::load(&lockfile_path(&resolved.repo_root))
        .inspect_err(|error| {
            tracing::warn!(
                %error,
                "leaving the declared remotes' checkouts alone: patina.lock could not be read, \
                 so which revs are pinned is unknown"
            );
        })
        .ok()?;
    Some(declared_pins(&resolved.remote_names, &lockfile))
}

/// The `(name, rev)` each remote in `declared` is pinned to, skipping the ones
/// with no pin yet.
fn declared_pins<'a>(
    declared: impl IntoIterator<Item = &'a RemoteName>,
    lockfile: &Lockfile,
) -> Vec<(RemoteName, String)> {
    declared
        .into_iter()
        .filter_map(|name| {
            lockfile
                .get(name)
                .map(|pin| (name.clone(), pin.rev.clone()))
        })
        .collect()
}

/// Build the [`PlanningContext`] both [`plan`] and [`current_managed_targets`]
/// consume.
///
/// Resolves the repository and state directory, the active profile, and the
/// resolver's repo-shared (`[variables]`) and active-profile
/// (`[profiles.<name>.variables]`) layers. The per-module layer is
/// *not* pushed here; each pass pushes it during its own module loop, in
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

    // The shared `MiniJinja` engine that evaluates every `when` predicate:
    // `[[file]]` / `[[directory]]` / `[[hook]]` and
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
    // pass populates. Resolution precedence is fixed by the
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
        remotes: root_config.remotes,
    })
}

/// Recompute the set of canonical target paths the *current* repository plan
/// manages, keyed by [`crate::status::manage_key`] for cross-time comparison
/// against the recorded commit.
///
/// This is the `when`-aware, `symlink-tree`-aware managed set both
/// `patina status` (to classify a dropped target ORPHANED) and the apply-time
/// orphan reap consume. It mirrors [`plan`]'s entry walk with two
/// differences that make it safe to run for status, where the plan would
/// refuse:
///
/// - **`when` gating.** An entry whose `when` is false on this host contributes
///   no managed target. A `[[file]]` whose `when` has been edited to false
///   therefore has its prior target fall out of the set, and classify ORPHANED.
///   The gate uses the same [`Engine::eval_when`] and layered resolver as
///   planning, so the two passes agree on which entries are active.
/// - **Tree-mode leaf expansion.** A `symlink-tree` or `copy-tree`
///   `[[directory]]` entry expands into one managed key per *live* source leaf,
///   walked in the same `walk_files` order the executor used. A deleted source
///   leaf is therefore absent from the set, and its recorded target leaf
///   classifies ORPHANED. Both modes materialize one object per leaf and
///   journal each leaf as its own target, so both must expand here; every other
///   mode contributes its declared target(s) directly.
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
pub fn current_managed_targets() -> Result<crate::status::ManagedTargets, EngineError> {
    let PlanningContext {
        repo_root,
        state_dir,
        home,
        mut resolver,
        engine,
        modules,
        remotes,
        ..
    } = build_planning_context(&[])?;

    // Read-only: status must not fetch. An unpinned or uncached remote yields
    // no source root, which costs only tree-mode leaf expansion.
    let mut registry = RemoteRegistry::new(&remotes, &repo_root, &state_dir, CachePolicy::ReadOnly);
    let mut managed = crate::status::ManagedTargets::default();
    for module in &modules {
        let manifest = module.path.join(MANIFEST_FILENAME);
        let config = parse_module_config(&manifest)?;

        if let Some(table) = config.variables.as_ref() {
            resolver = resolver.with_per_module(table_to_layer(table))?;
        }

        for entry in config.files.iter().chain(&config.directories) {
            // `when`-false entries manage nothing this run: their
            // prior targets fall out of the set and classify ORPHANED.
            if let Some(expr) = entry.when.as_deref()
                && !engine.eval_when(expr, &resolver)?
            {
                continue;
            }
            let origin = registry.origin(entry, &module.path)?;
            insert_managed_targets(entry, &origin, &home, &mut managed);
        }
    }
    Ok(managed)
}

/// Insert the managed `manage_key`(s) for one surviving (`when`-true) entry.
///
/// A tree-mode entry (`symlink-tree` or `copy-tree`) expands to one key per
/// live source leaf, mirrored under each declared target the same way the
/// executor materializes them (`target.join(rel)`); a missing local source
/// contributes no leaves. Every other mode contributes its declared targets
/// directly.
///
/// A tree-mode entry whose remote checkout is not on this machine is different
/// from one whose source is missing. Its leaves are unknowable rather than
/// gone. Patina therefore records its declared targets as indeterminate roots,
/// and reports its remote. The alternative would let every recorded leaf
/// classify ORPHANED, and be reaped, over a checkout that merely is not here.
fn insert_managed_targets(
    entry: &ManagedEntry,
    origin: &EntryOrigin,
    home: &Utf8Path,
    managed: &mut crate::status::ManagedTargets,
) {
    use crate::status::manage_key;

    // Tree modes (`symlink-tree` and `copy-tree`) materialize one object per
    // live source leaf and the journal records each leaf as its own target, so
    // the managed set must expand to those same leaves. Recording only the
    // declared directory would make every committed leaf look orphaned on the
    // next apply, and the reap pass would delete it (`copy-tree`'s journal
    // hashing would then fail on the just-reaped file).
    if matches!(entry.mode, FileMode::SymlinkTree | FileMode::CopyTree) {
        let Some(source_root) = origin.source_root.as_deref() else {
            if let Some(remote) = origin.remote.as_ref() {
                managed.unmaterialized_remotes.insert(remote.clone());
                for target in &entry.targets {
                    managed
                        .indeterminate_roots
                        .insert(manage_key(&expand_tilde(target, home)));
                }
            }
            return;
        };
        let source = source_root.join(&entry.source);
        // The executor walks the live source for leaves; a source that no
        // longer exists yields none, so every recorded leaf is then orphaned.
        let Ok(leaves) = crate::apply::walk_files(&source) else {
            return;
        };
        for target in &entry.targets {
            let expanded = expand_tilde(target, home);
            for rel in &leaves {
                managed.targets.insert(manage_key(&expanded.join(rel)));
            }
        }
        return;
    }

    for target in &entry.targets {
        let expanded = expand_tilde(target, home);
        managed.targets.insert(manage_key(&expanded));
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
    /// Plan-time classification of each declared target, parallel to
    /// [`targets`](Self::targets). Computed during
    /// resolution while the template engine and resolver are in scope (a
    /// template target is classified against its freshly rendered output).
    dispositions: Vec<TargetDisposition>,
    /// Name of the module that declared this entry, and the entry's declared
    /// module-relative source. Neither drives materialization; both exist so a
    /// target-collision error can point the author at a specific manifest line.
    module: String,
    /// The entry's declared source, as written in the manifest.
    declared_source: Utf8PathBuf,
}

/// One resolved entry paired with the targets it actually claims on the
/// filesystem: borrowed straight from the entry when they are its declared
/// targets, owned only when the planner expanded a tree-mode entry into
/// leaves.
struct ClaimTargets<'a> {
    entry: &'a ResolvedEntry,
    /// The declared directory target `targets` are the leaves of, for a
    /// tree-mode entry; `None` when they are the declared targets themselves.
    tree_target: Option<&'a Utf8Path>,
    targets: std::borrow::Cow<'a, [Utf8PathBuf]>,
}

impl ClaimTargets<'_> {
    /// Project this into the claim the target-collision check consumes.
    fn claim(&self) -> crate::apply::TargetClaim<'_> {
        crate::apply::TargetClaim {
            module: &self.entry.module,
            source: &self.entry.declared_source,
            mode: self.entry.mode,
            tree_target: self.tree_target,
            targets: &self.targets,
        }
    }
}

/// Project the active entries into what each one actually claims, expanding a
/// tree-mode entry into the leaves it will materialize.
///
/// A `symlink-tree` or `copy` `[[directory]]` does not own its whole target
/// directory: it materializes one object per source leaf, records each leaf as
/// its own journal target, and the reap pass removes only the leaves it
/// recorded. The collision check has to see the same footprint, or it would
/// refuse manifests the rest of the engine handles cleanly. Deploying one
/// upstream file into a directory the repository also fills is the ordinary
/// case once remotes exist.
///
/// The consequence is that the verdict depends on what the source tree holds
/// right now, so a file appearing upstream under a tree source can newly fail a
/// plan. That failure lands before any write, which is what a tree growing an
/// unexpected file is supposed to do.
///
/// The leaves are walked here rather than taken from the classified
/// dispositions. Classification deliberately records none for a tree target
/// that does not exist yet (the whole-op Create shortcut), and a fresh target
/// is exactly when a collision must still be caught.
///
/// Claims come out in declaration order, every `[[file]]` entry and then every
/// `[[directory]]` entry, so the reported pair is a function of the manifest.
///
/// # Errors
///
/// Returns an [`EngineError`] when a tree source cannot be walked. Its
/// existence and kind were validated during resolution, so this is a live IO
/// failure rather than a manifest error.
fn expand_claims<'a>(
    file_entries: &'a [Option<ResolvedEntry>],
    directory_entries: &'a [Option<ResolvedEntry>],
) -> Result<Vec<ClaimTargets<'a>>, EngineError> {
    let mut expanded = Vec::new();
    // Only surviving (`when`-true) entries claim anything, so `flatten` drops
    // the gated-off slots.
    for entry in file_entries.iter().chain(directory_entries).flatten() {
        if !matches!(entry.mode, FileMode::SymlinkTree | FileMode::CopyTree) {
            expanded.push(ClaimTargets {
                entry,
                tree_target: None,
                targets: std::borrow::Cow::Borrowed(&entry.targets[..]),
            });
            continue;
        }
        let leaves = crate::apply::walk_files(&entry.source)?;
        for target in &entry.targets {
            expanded.push(ClaimTargets {
                entry,
                tree_target: Some(target.as_path()),
                targets: std::borrow::Cow::Owned(
                    leaves.iter().map(|leaf| target.join(leaf)).collect(),
                ),
            });
        }
    }
    Ok(expanded)
}

/// Impose the single deterministic order on the resolved entries and
/// assign each managed entry its index over the full declared sequence.
///
/// Every `[[file]]` entry (in declaration order across all modules) is
/// emitted before every `[[directory]]` entry, and each managed entry is
/// assigned a single monotonic `u32` `entry_index` (files first, then
/// directories). The index advances for **every** declared entry,
/// including a `when`-false one (passed as `None`): a gated-off entry
/// occupies its index but emits no [`PlannedOperation`] and no
/// [`ResolvedOperation`]. That index is carried on each
/// [`ResolvedOperation`] so [`execute`] records the planned index rather
/// than re-deriving one from operation position. That guarantees no
/// `[[file]]` and `[[directory]]` entry collide on an index and that
/// targets sharing an entry form one atomic rollback unit. The
/// returned [`PlannedOperation`] vec is the per-target
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
            // The durable `PlannedOperation` is per declared target and
            // carries the target's whole-op (aggregate, for tree modes)
            // disposition; the in-memory `ResolvedOperation` carries the
            // full per-target/per-leaf classification.
            for (target, target_disposition) in resolved.targets.iter().zip(&resolved.dispositions)
            {
                operations.push(planned_operation(
                    resolved.mode,
                    &resolved.source,
                    target,
                    target_disposition.aggregate,
                ));
            }
            resolved_ops.push(ResolvedOperation {
                mode: resolved.mode,
                source: resolved.source,
                targets: resolved.targets,
                dispositions: resolved.dispositions,
                entry_index,
            });
        }
        entry_index = entry_index.saturating_add(1);
    }
    (operations, resolved_ops)
}

/// Evaluate one managed entry's `when` predicate, then, only if it holds,
/// canonicalize the entry's source and resolve its targets by declared
/// location.
///
/// This enforces the per-entry order: step (1) the `when` gate runs
/// first, so a `when`-false entry returns `Ok(None)` and is **never**
/// canonicalized; step (2) canonicalization happens only for a surviving
/// (`when`-true or no-`when`) entry. Returning `None` lets the caller keep
/// the entry's slot in the declared sequence (and thus its `entry_index`)
/// while emitting no operation or diff line. For a multi-target
/// entry the gate is above the target loop, so `when` gates all targets
/// together.
///
/// Step (3) of the order is the plan-time source existence-and-kind
/// validation. It runs inside [`resolve_entry`], right after the source is
/// canonicalized. A `when`-false entry (which returns `Ok(None)` here
/// before `resolve_entry` is ever called) is never canonicalized or
/// validated.
fn gate_and_resolve_entry(
    entry: &ManagedEntry,
    module: &crate::discovery::ModuleHandle,
    registry: &mut RemoteRegistry<'_>,
    home: &Utf8Path,
    engine: &Engine,
    resolver: &Resolver,
) -> Result<Option<ResolvedEntry>, EngineError> {
    if let Some(expr) = entry.when.as_deref()
        && !engine.eval_when(expr, resolver)?
    {
        return Ok(None);
    }
    // The gate is above this, so an entry switched off on this host never
    // reaches the registry and never triggers a fetch.
    let origin = registry.origin(entry, &module.path)?;
    Ok(Some(resolve_entry(
        entry,
        &origin,
        &module.name,
        home,
        engine,
        resolver,
    )?))
}

/// Classify each declared target of one resolved entry against live
/// filesystem state at plan time.
///
/// Single-target modes classify the target directly. Tree modes
/// (`copy-tree`, `symlink-tree`) enumerate the source leaves with
/// [`walk_files`](crate::apply::walk_files) and classify each leaf, then
/// fold the per-leaf results into the per-op aggregate:
/// `Create` if the target directory is absent, otherwise `Unchanged` iff
/// every leaf is `Unchanged`, otherwise `Update`. The per-leaf dispositions
/// ride along on each [`TargetDisposition`] for the execute write-skip
/// and the per-leaf reporting.
///
/// A template target is classified against its freshly rendered output,
/// rendered once here through `engine` / `resolver`.
///
/// # Errors
///
/// Returns an [`EngineError`] when a copy/copy-tree leaf's source cannot be
/// read to hash it, a tree source cannot be walked, or a template source
/// cannot be read or rendered.
fn classify_entry(
    mode: FileMode,
    source: &Utf8Path,
    targets: &[Utf8PathBuf],
    engine: &Engine,
    resolver: &Resolver,
) -> Result<Vec<TargetDisposition>, EngineError> {
    // Render a template source once; reuse the bytes to classify every
    // target (the executor likewise renders once per entry).
    let rendered = if matches!(mode, FileMode::TemplateRender) {
        let body = fs_err::read_to_string(source)
            .map_err(|err| EngineError::Journal(crate::journal::JournalError::Filesystem(err)))?;
        Some(engine.render(&body, resolver)?)
    } else {
        None
    };

    let mut dispositions = Vec::with_capacity(targets.len());
    for target in targets {
        dispositions.push(classify_target(mode, source, target, rendered.as_deref())?);
    }
    Ok(dispositions)
}

/// Classify one declared target: a single-target leaf, or a whole tree
/// expanded per leaf.
fn classify_target(
    mode: FileMode,
    source: &Utf8Path,
    target: &Utf8Path,
    rendered: Option<&str>,
) -> Result<TargetDisposition, EngineError> {
    use crate::apply::classify::classify_leaf;

    if !matches!(mode, FileMode::CopyTree | FileMode::SymlinkTree) {
        // Single-target mode: one classification, no leaves.
        let disposition = classify_leaf(mode, source, target, rendered.map(str::as_bytes))?;
        return Ok(TargetDisposition {
            aggregate: disposition,
            leaves: Vec::new(),
        });
    }

    // Tree mode. A target directory that does not yet exist is a
    // whole-op Create; its leaves would all be Create, so there is no need
    // to walk-and-classify them for the aggregate. Recording no per-leaf
    // entries here is fine: the execute path materializes the whole tree on
    // a Create.
    if fs_err::symlink_metadata(target).is_err() {
        return Ok(TargetDisposition {
            aggregate: Disposition::Create,
            leaves: Vec::new(),
        });
    }

    // The executor mirrors the live source tree to the target one leaf at a
    // time; classify each leaf at its mirrored target path. A missing source
    // yields no leaves (the entry would have failed source validation first).
    let relative_files = crate::apply::walk_files(source)?;
    let mut leaves = Vec::with_capacity(relative_files.len());
    let mut all_unchanged = true;
    for relative in relative_files {
        let leaf_source = source.join(&relative);
        let leaf_target = target.join(&relative);
        let disposition = classify_leaf(mode, &leaf_source, &leaf_target, None)?;
        if disposition != Disposition::Unchanged {
            all_unchanged = false;
        }
        leaves.push(LeafDisposition {
            relative,
            disposition,
        });
    }

    // The target exists, so the op is not a Create; it is Unchanged iff every
    // materialized leaf is Unchanged, otherwise Update.
    let aggregate = if all_unchanged {
        Disposition::Unchanged
    } else {
        Disposition::Update
    };
    Ok(TargetDisposition { aggregate, leaves })
}

/// Canonicalize one managed entry's source and resolve its targets under
/// `module_path` and `home`. Then validate the canonical source's existence
/// and kind against the entry's declared table. That is step 3 of the
/// order. The source is canonicalized through the filesystem; each
/// target is resolved by *declared location* via [`resolve_location`] so a
/// symlink already occupying the target is never followed back to the source.
/// The file/directory order and the entry-index space are imposed by the
/// caller; this resolves paths, performs the plan-time source check, and
/// classifies each resolved target against live state using
/// `engine` / `resolver` to render a template target's comparison bytes.
///
/// # Errors
///
/// Returns [`EngineError::SourceNotFound`] when the canonical source does
/// not exist on disk, and [`EngineError::SourceKindMismatch`] when a
/// `[[file]]` entry's source is a directory or a `[[directory]]` entry's
/// source is a file. Both are raised here, in the plan phase, before any
/// mutation. Path canonicalization failures surface as [`EngineError::Path`].
/// A classification read or template render failure surfaces as
/// [`EngineError::Classify`], [`EngineError::Journal`], or
/// [`EngineError::Template`].
fn resolve_entry(
    entry: &ManagedEntry,
    origin: &EntryOrigin,
    module: &str,
    home: &Utf8Path,
    engine: &Engine,
    resolver: &Resolver,
) -> Result<ResolvedEntry, EngineError> {
    let root = match (origin.source_root.as_deref(), origin.remote.as_ref()) {
        (Some(root), _) => root,
        // A `Fetch`-policy origin always carries a source root for a remote
        // entry; a slip here must not weaken the containment check below by
        // anchoring it on the working directory.
        (None, Some(remote)) => {
            return Err(
                crate::remote::RemoteError::checkout_not_materialized(remote.as_str()).into(),
            );
        }
        // A local origin always carries its module directory; the fallback
        // keeps this total so a logic slip surfaces as `SourceNotFound` rather
        // than as a panic.
        (None, None) => Utf8Path::new("."),
    };
    let source = canonicalize(&root.join(&entry.source))?;
    // A remote checkout is third-party: refuse a source that resolves outside
    // it (a `..` in the declared source, or a symlink the checkout shipped),
    // which would otherwise let a hostile remote read arbitrary host files. A
    // local source is the user's own tree and keeps its existing latitude.
    // `root` came out of the registry canonical, so the prefix test needs no
    // further resolution.
    if let Some(remote) = origin.remote.as_ref()
        && !source.starts_with(root)
    {
        return Err(crate::remote::RemoteError::source_escapes_checkout(
            remote.as_str(),
            &entry.source,
        )
        .into());
    }
    let metadata = source_metadata(&source)?;
    // Canonicalizing the declared source vets that one path, but a directory
    // source is deployed leaf by leaf and the executors dereference any
    // symlink they meet, so every leaf must be vetted too. Patina's own
    // materialization writes with `core.symlinks=false` and never produces
    // one; finding one means the cache was made or altered by something else.
    if let Some(remote) = origin.remote.as_ref()
        && metadata.is_dir()
    {
        reject_symlink_leaves(remote.as_str(), &source)?;
    }
    validate_source_kind(&source, entry.kind, &metadata)?;
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
    // Classify each resolved target against live filesystem state at plan
    // time. A template target is compared against its freshly
    // rendered output, rendered here while the engine and resolver are in
    // scope (the double render at execute time is accepted per the
    // assumptions).
    let dispositions = classify_entry(entry.mode, &source, &targets, engine, resolver)?;
    Ok(ResolvedEntry {
        mode: entry.mode,
        source,
        targets,
        dispositions,
        module: module.to_owned(),
        declared_source: entry.source.clone(),
    })
}

/// Refuse a remote directory source holding a symbolic link anywhere beneath
/// it.
///
/// The leaves come from the same [`crate::apply::walk_files`] enumeration the
/// executors deploy. That enumeration yields a symlink as a leaf rather than
/// descending it, whether it points at a file, a directory, or nothing at
/// all. The two
/// passes can therefore never disagree about what the tree contains. The plan
/// fails before any mutation.
///
/// # Errors
///
/// Returns the [`RemoteError`](crate::remote::RemoteError) naming the first
/// link found, or the walk's own error when the tree cannot be read.
fn reject_symlink_leaves(remote: &str, source: &Utf8Path) -> Result<(), EngineError> {
    for leaf in crate::apply::walk_files(source)? {
        let path = source.join(&leaf);
        let metadata = fs_err::symlink_metadata(path.as_std_path()).map_err(|io_source| {
            EngineError::Path(crate::paths::PathError::Filesystem {
                path: path.clone(),
                source: io_source,
            })
        })?;
        if metadata.file_type().is_symlink() {
            return Err(crate::remote::RemoteError::symlink_in_checkout(remote, &path).into());
        }
    }
    Ok(())
}

/// The metadata of a canonical source path, with a missing source reported as
/// [`EngineError::SourceNotFound`].
///
/// `paths::canonicalize` falls back to a *lexical* resolution for a
/// non-existent path, so a missing source does not fail at canonicalization;
/// the existence check is therefore this explicit probe. `metadata` follows
/// symlinks, so the kind carried is the kind the source ultimately resolves
/// to: the same kind the executor will materialize.
fn source_metadata(source: &Utf8Path) -> Result<std::fs::Metadata, EngineError> {
    match fs_err::metadata(source) {
        Ok(metadata) => Ok(metadata),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(EngineError::SourceNotFound {
                path: source.to_path_buf(),
            })
        }
        Err(err) => Err(EngineError::Path(crate::paths::PathError::Filesystem {
            path: source.to_path_buf(),
            source: err,
        })),
    }
}

/// Validate a canonical source's already-fetched metadata against the kind
/// declared by its table-array, so the check adds no IO of its own.
///
/// # Errors
///
/// Returns [`EngineError::SourceKindMismatch`] when a `[[file]]` source is a
/// directory or a `[[directory]]` source is a file.
fn validate_source_kind(
    source: &Utf8Path,
    kind: EntryKind,
    metadata: &std::fs::Metadata,
) -> Result<(), EngineError> {
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

/// Build the durable [`PlannedOperation`] for one resolved
/// `(mode, source, target)`, carrying the target's plan-time disposition
/// (the per-op aggregate for tree modes).
fn planned_operation(
    mode: FileMode,
    source: &Utf8Path,
    target: &Utf8Path,
    disposition: Disposition,
) -> PlannedOperation {
    match mode {
        // A `[[directory]]` `symlink` (the atomic whole-directory
        // `SymlinkDir`) maps to the same durable symlink op shape as a
        // `[[file]]` `symlink`. `SymlinkTree` shares that symlink op shape
        // so the plan is well-formed; the executor handles its per-leaf
        // expansion.
        FileMode::Symlink | FileMode::SymlinkDir | FileMode::SymlinkTree => {
            PlannedOperation::symlink(source.as_str(), target.as_str(), disposition)
        }
        FileMode::Copy | FileMode::CopyTree => {
            PlannedOperation::copy(source.as_str(), target.as_str(), disposition)
        }
        FileMode::TemplateRender => {
            PlannedOperation::render(source.as_str(), target.as_str(), disposition)
        }
    }
}

/// Materialize one declared target according to its plan-time disposition,
/// upholding the write-and-backup skip.
///
/// - **Aggregate `Unchanged`**: the target (single-target or whole tree)
///   matches desired state, so it is neither backed up nor written. No
///   [`CompletionRecord`] is produced; the commit records it from the resolved
///   plan instead.
/// - **Single-target `Create` / `Update`**: back up the pre-existing target (a
///   no-op for an absent `Create` target, since [`backup_before_overwrite`]
///   only stashes something that exists), then materialize it.
/// - **Tree `Create` / `Update`**: back up the target directory as a unit (a
///   whole-directory backup, which captures every leaf's prior bytes), then
///   (re)write only the leaves whose per-leaf disposition is not `Unchanged`. A
///   `Create` aggregate carries no per-leaf entries, so every leaf is written
///   ([`LeafWrite::All`]); an `Update` aggregate writes only the drifted leaves
///   ([`LeafWrite::Only`]), leaving clean leaves' inode/mtime untouched.
///
/// # Errors
///
/// Returns an [`EngineError`] when a backup or an executor write fails.
#[expect(
    clippy::too_many_arguments,
    reason = "the per-target write-skip needs the op's mode/source, the \
              target and its plan-time disposition, the backup tree + \
              timestamp, and the template engine/resolver; threading a \
              struct here would only move the same fields behind a name."
)]
fn materialize_target(
    mode: FileMode,
    source: &Utf8Path,
    target: &Utf8Path,
    disposition: &TargetDisposition,
    backups_dir: &Utf8Path,
    timestamp: &str,
    engine: &Engine,
    resolver: &Resolver,
) -> Result<Vec<CompletionRecord>, EngineError> {
    // An Unchanged target is left exactly as it is: no backup, no write.
    // Its commit-record entry is sourced from the resolved plan.
    if disposition.aggregate == Disposition::Unchanged {
        return Ok(Vec::new());
    }

    let is_tree = matches!(mode, FileMode::CopyTree | FileMode::SymlinkTree);
    if !is_tree {
        // Single-target Create/Update: back up the pre-existing target (a
        // no-op for an absent Create target) and materialize it as today.
        backup_before_overwrite(backups_dir, timestamp, target)?;
        return Ok(materialize(
            mode,
            source,
            std::slice::from_ref(&target.to_path_buf()),
            engine,
            resolver,
        )?);
    }

    // Tree Create/Update: back up the whole target directory as a
    // unit so every leaf's prior bytes are captured, then write only the
    // drifted leaves. A Create aggregate has no per-leaf entries (the target
    // dir is absent), so write every leaf.
    backup_before_overwrite(backups_dir, timestamp, target)?;
    if disposition.aggregate == Disposition::Create {
        return Ok(materialize_tree(mode, source, target, LeafWrite::All)?);
    }
    // Update aggregate: write only the leaves whose per-leaf disposition is
    // not Unchanged, so clean leaves keep their inode/mtime.
    let drifted: BTreeSet<Utf8PathBuf> = disposition
        .leaves
        .iter()
        .filter(|leaf| leaf.disposition != Disposition::Unchanged)
        .map(|leaf| leaf.relative.clone())
        .collect();
    Ok(materialize_tree(
        mode,
        source,
        target,
        LeafWrite::Only(&drifted),
    )?)
}

/// Execute a [`ResolvedPlan`] against the filesystem.
///
/// Takes the exclusive lock, recovers any orphan plan under that held lock,
/// and flushes the journal. Then runs `pre_apply` hooks, materializes every
/// operation (backing up each pre-existing target first), and runs
/// `post_apply` hooks. Finally commits, or rolls the file operations back
/// when a `must_succeed` `post_apply` hook fails.
///
/// # Errors
///
/// Returns an [`EngineError`] when recovery, locking, journal flushing,
/// an executor, hook execution, backup, or retention GC fails. A hook
/// that *fails* under `must_succeed` is not an error: it is reported via
/// the returned [`ApplyResult`] so the CLI can map it to the right exit
/// code.
#[expect(
    clippy::too_many_lines,
    reason = "execute is the single linear apply orchestrator: lock, recover, \
              no-op short-circuit, hooks, flush, materialize, commit/rollback, \
              and GC, in the fixed order the crash-safety contract depends on. \
              Splitting a phase into a helper would hide that ordering behind a \
              call without removing any step; the no-op gate is one such \
              step and pushed it four lines past the lint's ceiling."
)]
pub async fn execute(
    resolved: &ResolvedPlan,
    request: &ApplyRequest,
    policy: LockPolicy,
) -> Result<ApplyResult, EngineError> {
    let journal_dir = resolved.journal_dir();
    let backups_dir = resolved.backups_dir();
    let template_engine = Engine::new();

    // Whether this run reaps targets a prior apply committed that the current
    // plan no longer manages. A full `apply` (`Blocking`)
    // and a watcher re-apply (`NonBlocking`) reconcile the whole plan, so they
    // reap. The `Held` path is a surgical single-target re-journal driven by
    // `patina remove` / `patina promote` under a caller-held lock: those
    // commands intentionally convert one managed target into an owned regular
    // file and drop its entry, so reaping would delete the very file they just
    // promoted. They must not reap.
    let reap = !matches!(policy, LockPolicy::Held(_));

    // Resolve the exclusive lock per policy BEFORE any filesystem
    // mutation, including orphan recovery. On the `NonBlocking`
    // contention path this returns early, before `recover_orphans` and
    // the plan flush below, so a contended attempt mutates nothing
    // (no recovery, no plan, no COMMIT, no backup), upholding the
    // zero-write guarantee. Recovering only under the held lock also
    // prevents a second apply from reversing a live in-flight apply's
    // operations.
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

    // Windows-only symlink-elevation gate. Runs after
    // recovery and BEFORE the first backup / materialize, so a plan that
    // needs Developer Mode cannot mutate the filesystem without consent.
    // This is the engine-side backstop: the CLI normally drives the UAC
    // prompt before calling `execute`, so a `RequireElevation` verdict here
    // means the gate was reached without that orchestration. Refuse to
    // proceed with a typed signal. On a host that is already
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

    // Full no-op short-circuit: return before the plan flush so
    // nothing is written this run (see `is_full_noop` for the condition).
    if is_full_noop(resolved, reap)? {
        return Ok(ApplyResult::Applied {
            warnings: Vec::new(),
            up_to_date: true,
        });
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

    // Flush the plan journal: the durability point before mutation.
    let mut journal = Journal::flush_plan_and_fsync(
        &journal_dir,
        &resolved.timestamp,
        &resolved.plan,
        &OsSyncer,
    )?;

    // Materialize every operation, backing up each pre-existing target
    // first, except a target classified `Unchanged` at plan time, which is
    // neither backed up nor (re)written so its inode/mtime is preserved.
    // Track completion records (paired with the index of the
    // `[[file]]` entry that produced them) so a post_apply hook failure can
    // reverse them and the commit record can group targets into atomic
    // rollback units. Only the targets actually written produce a
    // record; `Unchanged` targets are recorded in the commit from the
    // resolved plan instead (see `build_apply_record`).
    // Test-only crash-injection seam. Compiled only in debug builds, so it is
    // absent from the release binary users install, and dormant unless the
    // `PATINA_TEST_ABORT_AFTER_OP` environment variable is set. When set to
    // `k`, the process exits abruptly after the k-th materialized operation,
    // before the COMMIT sentinel is written, simulating a `kill -9` so an
    // integration test can prove the next run converges to a consistent state.
    // See `patina-cli/tests/apply_crash_recovery.rs`.
    #[cfg(debug_assertions)]
    let abort_after_op: Option<u32> = std::env::var("PATINA_TEST_ABORT_AFTER_OP")
        .ok()
        .and_then(|value| value.parse().ok());

    let mut completed: Vec<(u32, CompletionRecord)> = Vec::new();
    let mut op_index: u32 = 0;
    for op in &resolved.operations {
        // Use the entry index assigned at plan time over the full declared
        // sequence (files then directories) rather than re-deriving one from
        // operation position, so a `[[file]]` and a `[[directory]]` entry can
        // never collide on an index and rollback groups targets by their
        // declared entry.
        let entry_index = op.entry_index;
        // Each declared target carries its plan-time disposition, parallel to
        // `targets`; drive the per-target / per-leaf write-skip off it.
        for (target, disposition) in op.targets.iter().zip(&op.dispositions) {
            let records = materialize_target(
                op.mode,
                &op.source,
                target,
                disposition,
                &backups_dir,
                &resolved.timestamp,
                &template_engine,
                vars,
            )?;
            for record in records {
                journal.record_progress(op_index)?;
                op_index = op_index.saturating_add(1);
                completed.push((entry_index, record));

                // Test-only crash-injection point (see `abort_after_op` above):
                // terminate immediately, before COMMIT, leaving `op_index`
                // operations durably applied and the plan an orphan.
                #[cfg(debug_assertions)]
                if abort_after_op == Some(op_index) {
                    std::process::exit(70);
                }
            }
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
        // longer manages: a removed entry, a `when` flipped to false
        // or a deleted `symlink-tree` source leaf. Each
        // orphan's prior bytes are backed up into this run's backup tree
        // before it is removed; a directory is never removed.
        // Runs after the post_apply hooks succeed, so a hook
        // failure rolls back the materializations without having reaped.
        // Skipped on the `Held` path (`patina remove` / `promote`), which
        // re-journals one surgically-modified target and must not reap.
        if reap {
            reap_orphans(resolved, &backups_dir)?;
        }
        let record = build_apply_record(resolved)?;
        journal.commit(&record, &OsSyncer)?;
        // Retention prunes the oldest backup cycles, then the journal
        // sentinels for exactly those cycles are dropped in lockstep: a
        // commit whose backups are gone can no longer be faithfully reversed
        // (its overwrite-restores are gone), so it must not remain
        // rollback- or status-eligible. An all-fresh
        // apply writes no backup directory and so is never pruned here, and
        // rolling back to it correctly deletes its fresh targets.
        let pruned = gc_retain(&backups_dir, crate::backups::RETENTION_COUNT)?;
        prune_cycles(&journal_dir, &pruned)?;
        // Remote checkouts follow the same retain-what-recovery-needs rule as
        // backups, and for the same reason: a checkout an on-disk journal record
        // still names is what `patina rollback` re-points links back to. Runs
        // after the commit and after the journal prune, so the reachability set
        // it reads is this run's, and disk settles at roughly the current and
        // previous rev per remote. Currently pinned checkouts survive even
        // unreferenced: another process's not-yet-committed plan points at a
        // pinned rev by construction.
        //
        // Planning reads `patina.lock` only when an active entry selects a
        // remote, so a run without one arrives here not knowing what is pinned.
        // It is re-read here rather than assumed empty; a lockfile that cannot
        // be read leaves every declared remote's cache untouched.
        //
        // Pruning is best-effort cleanup after a durable commit, so a failure
        // anywhere here is logged, not propagated: the apply already succeeded
        // and rolling it back over a stale checkout would be worse than leaving
        // the checkout on disk.
        let declared: BTreeSet<&RemoteName> = resolved.remote_names.iter().collect();
        let pins = resolved
            .remote_pins
            .clone()
            .or_else(|| reread_pins(resolved));
        match remote_cache::prune(&resolved.state_dir, &declared, pins.as_deref()) {
            Ok(removed) => {
                for checkout in removed {
                    tracing::debug!(checkout = %checkout, "pruned an unreferenced remote checkout");
                }
            }
            Err(error) => tracing::warn!(
                %error,
                "failed to prune unreferenced remote checkouts; the committed apply is unaffected"
            ),
        }
        // A committed apply that actually flushed a plan and wrote a COMMIT is
        // not a no-op, even when some targets were `Unchanged`: the full-no-op
        // short-circuit above is the only path that sets `up_to_date`.
        Ok(ApplyResult::Applied {
            warnings,
            up_to_date: false,
        })
    }
}

/// Build the [`ApplyRecord`] persisted in this run's COMMIT sentinel from
/// the resolved plan's `last_apply` metadata and per-target/per-leaf
/// dispositions. `patina status` decodes this to classify the live
/// filesystem against the last committed apply.
///
/// Every managed target becomes one [`ExpectedTarget`], **including
/// `Unchanged` targets** that the execute write-skip left untouched and that
/// therefore produced no [`CompletionRecord`]. The record is sourced
/// from the resolved plan rather than from the written objects. That keeps an
/// `Unchanged` target in the commit, so `status` reports it managed (`Clean`)
/// and [`reap_orphans`] never removes it. A symlink records its canonical link
/// target (which is also its source); a copy or render records its canonical
/// source path and a `blake3` hash of the live target bytes, read back so the
/// recorded hash matches exactly what `status` computes; the live
/// bytes hold the desired output whether the target was just written
/// (`Create` / `Update`) or already matched (`Unchanged`). Each target carries
/// its real plan-time [`Disposition`] (per-leaf for a tree), the
/// marker recovery and rollback read to leave an `Unchanged` target in place.
fn build_apply_record(resolved: &ResolvedPlan) -> Result<ApplyRecord, EngineError> {
    let vars = &resolved.resolver;
    let last_apply = LastApply {
        at: timestamp_to_rfc3339(&resolved.timestamp),
        user: vars.get("patina.user").unwrap_or_default(),
        host: vars.get("patina.hostname").unwrap_or_default(),
    };

    let mut targets = Vec::new();
    for op in &resolved.operations {
        let entry = op.entry_index;
        let is_tree = matches!(op.mode, FileMode::CopyTree | FileMode::SymlinkTree);
        for (target, disposition) in op.targets.iter().zip(&op.dispositions) {
            if is_tree {
                record_tree_targets(
                    &mut targets,
                    op.mode,
                    &op.source,
                    target,
                    disposition,
                    entry,
                )?;
            } else {
                targets.push(expected_target(
                    op.mode,
                    &op.source,
                    target,
                    disposition.aggregate,
                    entry,
                )?);
            }
        }
    }
    Ok(ApplyRecord::new(last_apply, targets))
}

/// Append one [`ExpectedTarget`] per materialized leaf of a tree-mode target:
/// the commit records per-leaf so `status` and `rollback` resolve
/// each leaf independently.
///
/// A `Create` aggregate carries no per-leaf dispositions, because the target
/// directory was absent at plan time. The source leaves are enumerated here
/// instead, with the same [`walk_files`](crate::apply::walk_files) walk the
/// executor used, each recorded as `Create`. Otherwise the per-leaf
/// dispositions computed at plan time are recorded verbatim. An `Update` tree
/// therefore records its drifted leaves as `Update` / `Create` and its clean
/// leaves as `Unchanged`. A fully-`Unchanged` tree records every leaf as
/// `Unchanged`.
fn record_tree_targets(
    targets: &mut Vec<ExpectedTarget>,
    mode: FileMode,
    source: &Utf8Path,
    target: &Utf8Path,
    disposition: &TargetDisposition,
    entry: u32,
) -> Result<(), EngineError> {
    if disposition.aggregate == Disposition::Create {
        for relative in crate::apply::walk_files(source)? {
            targets.push(expected_target(
                mode,
                &source.join(&relative),
                &target.join(&relative),
                Disposition::Create,
                entry,
            )?);
        }
        return Ok(());
    }
    for leaf in &disposition.leaves {
        targets.push(expected_target(
            mode,
            &source.join(&leaf.relative),
            &target.join(&leaf.relative),
            leaf.disposition,
            entry,
        )?);
    }
    Ok(())
}

/// Build one [`ExpectedTarget`] for a single materialized object from its
/// `(mode, source, target)` and plan-time `disposition`.
///
/// A symlink-family mode records the canonical link target (= the source); a
/// content mode records the source path and a `blake3` hash of the live target
/// bytes. The live read is correct for every disposition: a `Create`
/// or `Update` target was just written to the desired output, and an
/// `Unchanged` target already matched it.
fn expected_target(
    mode: FileMode,
    source: &Utf8Path,
    target: &Utf8Path,
    disposition: Disposition,
    entry: u32,
) -> Result<ExpectedTarget, EngineError> {
    let target_str = target.as_str().to_owned();
    match mode {
        FileMode::Symlink | FileMode::SymlinkDir | FileMode::SymlinkTree => {
            Ok(ExpectedTarget::Symlink {
                target: target_str,
                link_target: source.as_str().to_owned(),
                entry,
                disposition,
            })
        }
        FileMode::Copy | FileMode::CopyTree | FileMode::TemplateRender => {
            let bytes = fs_err::read(target).map_err(|source| {
                EngineError::Journal(crate::journal::JournalError::Filesystem(source))
            })?;
            Ok(ExpectedTarget::Content {
                target: target_str,
                source: source.as_str().to_owned(),
                hash: content_hash(&bytes),
                entry,
                disposition,
            })
        }
    }
}

/// Reap targets a prior committed apply materialized that the current plan
/// no longer manages.
///
/// Reads the last committed [`ApplyRecord`] and the current managed-target
/// set ([`current_managed_targets`], the same `when`-aware /
/// `symlink-tree`-aware set `patina status` classifies against). A recorded
/// target whose [`manage_key`](crate::status::manage_key) is absent from the
/// current set is an orphan: the entry was removed, its `when` flipped false
/// or, for a `symlink-tree` leaf, its source leaf was deleted.
/// Each orphan still present on disk is backed up into
/// this run's backup tree, under the same never-overwrite-without-backup
/// guarantee every mutating path upholds, and then removed.
///
/// A directory is never removed, even one left empty after its last leaf
/// link is reaped: Patina cannot prove it owns a directory that may also
/// hold files written outside Patina. The check is on the live
/// entry's kind, so an intermediate `symlink-tree` directory survives while
/// its orphaned leaf links are removed.
///
/// Nor is a recorded target that now lies inside a live entry's target: the
/// live entry owns those bytes, and where it materializes a whole-directory
/// symlink the recorded path resolves through the link into its source.
///
/// # Errors
///
/// Returns an [`EngineError`] when the commit read, the managed-set
/// recomputation, a backup, or a removal fails.
fn reap_orphans(resolved: &ResolvedPlan, backups_dir: &Utf8Path) -> Result<(), EngineError> {
    for target in detect_orphans(resolved)? {
        // Record the prior bytes in a backup before removal. The
        // stash uses this run's timestamped backup tree, the
        // same one materialize stashes overwrites into.
        backup_before_overwrite(backups_dir, &resolved.timestamp, &target)?;
        remove_target(&target)?;
    }
    Ok(())
}

/// Whether `resolved` is a full no-op: every planned target classifies
/// `Unchanged`, a prior committed apply exists to stay authoritative, and the
/// reap set is empty. `reap` mirrors [`execute`]'s policy gate: a
/// `Held` run never reaps, so its orphan set is not consulted.
///
/// This is the single source of truth for the condition, shared by
/// [`execute`]'s pre-flush short-circuit and the public [`plan_is_full_noop`]
/// probe the CLI calls to decide whether to skip the diff-and-prompt.
/// The `Unchanged` check is pure (it reads the plan-time dispositions, no IO);
/// the prior-commit and orphan checks read the journal and re-derive the
/// managed set.
///
/// # Errors
///
/// Returns an [`EngineError`] when the commit read or the orphan-set
/// recomputation fails.
fn is_full_noop(resolved: &ResolvedPlan, reap: bool) -> Result<bool, EngineError> {
    // A non-reaping policy is the `Held` surgical re-journal (`patina remove` /
    // `promote`): it deliberately re-records one target, often one whose bytes
    // now match its just-rewritten source and so classify `Unchanged`, and
    // must always commit that fresh record. It is never a no-op, so the
    // short-circuit is disabled for it; only the whole-plan reconcile policies
    // (`Blocking` / `NonBlocking`) can no-op.
    if !reap {
        return Ok(false);
    }
    // Pure, IO-free check next: a single Create/Update target means there is
    // work to do, so skip the journal read entirely.
    let all_unchanged = resolved.operations.iter().all(|op| {
        op.dispositions
            .iter()
            .all(|d| d.aggregate == Disposition::Unchanged)
    });
    if !all_unchanged {
        return Ok(false);
    }
    // A full no-op keeps the prior commit authoritative, so one must
    // exist. A first-ever apply with an empty plan is vacuously all-`Unchanged`
    // but has no baseline; it must fall through and commit an (empty) record to
    // establish one, preserving the pre-existing commit-always contract.
    if crate::journal::read_latest_commit(resolved.journal_dir())?.is_none() {
        return Ok(false);
    }
    // A reap is work to do: an all-`Unchanged` plan that still has an
    // orphan to remove is not a no-op.
    if !detect_orphans(resolved)?.is_empty() {
        return Ok(false);
    }
    Ok(true)
}

/// Whether a `patina apply` over `resolved` would be a full no-op under the
/// CLI's default reaping (`Blocking`) policy: every target `Unchanged`, a
/// prior commit present, and nothing to reap.
///
/// The CLI calls this *before* prompting so a fully-satisfied repo skips the
/// diff-and-prompt confirmation and never reads stdin. It is a
/// read-only probe: [`execute`] re-checks the same condition under the held
/// lock, so this decision only governs the prompt, never whether a write
/// happens. Because the CLI's `apply` path always reaps, this fixes `reap`
/// to `true`.
///
/// # Errors
///
/// Returns an [`EngineError`] when the commit read or the orphan-set
/// recomputation fails.
pub fn plan_is_full_noop(resolved: &ResolvedPlan) -> Result<bool, EngineError> {
    is_full_noop(resolved, true)
}

/// The targets a `patina apply` over `resolved` would reap this run, sorted by
/// path: the orphan targets a prior committed apply materialized that the
/// current plan no longer manages and that are still present on disk as
/// non-directories.
///
/// The CLI calls this to render the reap in the diff / `--json` preview
/// *before* prompting, so an entry removed from config is never silently
/// deleted on confirm. It is a read-only probe over the same orphan set
/// [`execute`] re-derives under the held lock, so it only informs the
/// preview, never whether a removal happens. Sorting makes the preview a
/// stable function of the reap set (orphans have no config declaration order
/// to preserve), upholding the byte-identical-stdout contract.
///
/// # Errors
///
/// Returns an [`EngineError`] when the commit read or the managed-set
/// recomputation fails.
pub fn plan_orphans(resolved: &ResolvedPlan) -> Result<Vec<Utf8PathBuf>, EngineError> {
    let mut orphans = detect_orphans(resolved)?;
    orphans.sort();
    Ok(orphans)
}

/// The set of targets the reap phase would remove this run: the orphan
/// targets a prior committed apply materialized that the current plan no
/// longer manages and that are still present on disk as non-directories.
///
/// Reads the last committed [`ApplyRecord`] and the current managed-target
/// set ([`current_managed_targets`]). Returns each recorded target whose
/// [`manage_key`](crate::status::manage_key) is absent from the current set,
/// is still on disk, is not a directory, and lies under no currently-managed
/// target. Two callers share it. [`reap_orphans`] backs up and removes each
/// returned target. The full-no-op short-circuit in [`execute`] only needs to
/// know whether this set is empty: a non-empty reap set means there is work to
/// do, so the run is not a no-op. Splitting the detection out
/// keeps the "what counts as an orphan" rule in one place rather than copying
/// the walk into the short-circuit.
///
/// # Errors
///
/// Returns an [`EngineError`] when the commit read or the managed-set
/// recomputation fails.
fn detect_orphans(resolved: &ResolvedPlan) -> Result<Vec<Utf8PathBuf>, EngineError> {
    use crate::status::manage_key;

    let journal_dir = resolved.journal_dir();
    let Some(record) = crate::journal::read_latest_commit(&journal_dir)? else {
        // No prior committed apply: nothing was ever materialized to orphan.
        return Ok(Vec::new());
    };

    let managed = current_managed_targets()?;

    let mut orphans = Vec::new();
    for expected in &record.targets {
        let target = Utf8PathBuf::from(expected.target());
        if managed.governs(&manage_key(&target)) {
            // Still managed by the current plan: leave it for materialize /
            // status to handle. Never reaped.
            continue;
        }
        // The current plan dropped this target. Only act on one that is still
        // on disk; an already-gone orphan needs no work.
        let Ok(meta) = fs_err::symlink_metadata(&target) else {
            continue;
        };
        // Never remove a directory: Patina cannot prove it owns a
        // directory that may also hold files written by another tool. This
        // is the guard that keeps a `symlink-tree` intermediate directory in
        // place while its orphaned leaf links are reaped.
        if meta.is_dir() {
            continue;
        }
        // A recorded target that now lies *under* a target the current plan
        // manages belongs to that entry, not to the record: a whole-directory
        // `symlink` entry can claim the directory a prior apply's leaf lived
        // in, and the recorded leaf path then resolves through the new link
        // into that entry's source, a remote checkout or the repository itself.
        // Removing it would delete source bytes, and back them up as if they
        // were the target's.
        //
        // Containment is tested one ancestor at a time rather than as a prefix
        // relation between keys: `manage_key` canonicalizes a path's parent, so
        // the leaf's key already resolves through the link while each
        // ancestor's own key does not. Tested last, so only a path that passed
        // every cheaper test pays for the canonicalizations.
        if target
            .ancestors()
            .skip(1)
            .any(|ancestor| managed.governs(&manage_key(ancestor)))
        {
            continue;
        }
        orphans.push(target);
    }
    Ok(orphans)
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
    //! Unit coverage for the lock-acquisition policy
    //! and the acquire-then-recover orphan-safety
    //! reorder.
    //!
    //! These drive [`execute`] in-process so a `Held` policy can pass a
    //! test-controlled [`LockGuard`] and a `NonBlocking` policy can be
    //! observed returning before any mutation. Neither is expressible
    //! through the CLI binary, which cannot share a guard across processes.
    //! Each test builds a minimal empty-operation [`ResolvedPlan`] over a
    //! tempdir state directory. No repository discovery or process-env
    //! mutation is needed: the workspace forbids `unsafe`, and env mutation
    //! is `unsafe` under edition 2024. An empty plan still flushes a
    //! `<ts>.plan`, commits a `<ts>.COMMIT`, and then deletes the plan, which
    //! is enough surface to assert the journal side effects the scenarios name.
    //!
    //! The default `Blocking` policy preserving
    //! byte-identical stdout across two `patina apply --yes` runs is
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
        let targets: Vec<Utf8PathBuf> = target_tags
            .iter()
            .map(|t| Utf8PathBuf::from(format!("/home/{t}")))
            .collect();
        // The ordering/index tests assert over paths only, not dispositions;
        // give each target a placeholder so `dispositions` stays aligned with
        // `targets` (the real classification runs in `gate_and_resolve_entry`,
        // exercised by the tempdir plan-level tests below).
        let dispositions = targets
            .iter()
            .map(|_| TargetDisposition {
                aggregate: Disposition::Create,
                leaves: Vec::new(),
            })
            .collect();
        ResolvedEntry {
            mode,
            source: Utf8PathBuf::from(format!("/repo/{source_tag}")),
            targets,
            dispositions,
            module: "m".to_owned(),
            declared_source: Utf8PathBuf::from(source_tag),
        }
    }

    // Ordering: with two `[[file]]` entries and one
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

    // Index space: entry indices form a single monotonic
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
        // 2,3. The two ranges are disjoint, so no file/directory entry
        // collides on an index.
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

    // A `when`-false entry (a `None` slot) occupies its
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

        // f1 still consumed index 1, so f2 keeps index 2 and d0 keeps index 3.
        // The indices are not compacted to fill the gap.
        let indices: Vec<u32> = resolved.iter().map(|op| op.entry_index).collect();
        assert_eq!(
            indices,
            vec![0, 2, 3],
            "the gated-off entry occupies index 1, leaving a gap rather than \
             renumbering the survivors"
        );
    }

    /// The production sequence for a source check: probe the metadata, then
    /// validate the declared kind against it.
    fn check_source(source: &Utf8Path, kind: EntryKind) -> Result<(), EngineError> {
        validate_source_kind(source, kind, &source_metadata(source)?)
    }

    /// A `[[file]]` entry whose canonical source is a directory is
    /// a plan-time kind mismatch directing the author to `[[directory]]`. The
    /// error names the source path and both tables so the message is
    /// actionable.
    #[test]
    fn file_entry_with_directory_source_is_a_kind_mismatch() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let dir_source = root.join("confdir");
        fs_err::create_dir(&dir_source).expect("mkdir source");

        let err = check_source(&dir_source, EntryKind::File)
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

    /// Symmetric: a `[[directory]]` entry whose canonical source is
    /// a regular file is a kind mismatch directing the author to `[[file]]`.
    #[test]
    fn directory_entry_with_file_source_is_a_kind_mismatch() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let file_source = root.join("gitconfig");
        fs_err::write(&file_source, "[user]\n").expect("write source");

        let err = check_source(&file_source, EntryKind::Directory)
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

    /// A source that does not exist on disk is a "source not found"
    /// error, not a kind mismatch. `paths::canonicalize` resolves a missing
    /// path lexically, so the existence check is this explicit probe.
    #[test]
    fn absent_source_is_source_not_found() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let ghost = root.join("ghost");

        let err =
            check_source(&ghost, EntryKind::File).expect_err("an absent source must be rejected");

        assert!(
            matches!(&err, EngineError::SourceNotFound { path } if *path == ghost),
            "an absent source must yield SourceNotFound naming the source, got: {err:?}"
        );
    }

    /// A `[[file]]` source that is a file and a `[[directory]]`
    /// source that is a directory both validate cleanly: the matched-kind
    /// path raises no error.
    #[test]
    fn matching_kinds_validate_cleanly() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let file_source = root.join("zshrc");
        fs_err::write(&file_source, "export EDITOR=vim\n").expect("write file source");
        let dir_source = root.join("mpv");
        fs_err::create_dir(&dir_source).expect("mkdir dir source");

        check_source(&file_source, EntryKind::File)
            .expect("a file source under `[[file]]` validates");
        check_source(&dir_source, EntryKind::Directory)
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
                remote_names: Vec::new(),
                remote_pins: Some(Vec::new()),
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
        /// the journal: a prior crashed apply with no COMMIT / `ROLLED_BACK`
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

    // Under the NonBlocking policy against a lock held by a
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

    // Under the Held policy with the caller's own exclusive guard,
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

    // Under the NonBlocking policy against a lock held by a
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

        // The orphan is left exactly as planted, neither reversed nor deleted.
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
    /// materializes them. A source leaf that no longer exists therefore
    /// contributes no key, and its recorded target leaf classifies ORPHANED.
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
            remote: None,
        };

        let origin = EntryOrigin {
            source_root: Some(module.clone()),
            remote: None,
        };
        let mut got = crate::status::ManagedTargets::default();
        insert_managed_targets(&entry, &origin, &root, &mut got);

        let expected: BTreeSet<Utf8PathBuf> = [
            manage_key(&target.join("a.conf")),
            manage_key(&target.join("sub").join("b.conf")),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            got.targets, expected,
            "a symlink-tree entry must contribute exactly one key per live leaf"
        );

        // Delete a source leaf: it drops out of the managed set, so its
        // recorded target leaf would now classify ORPHANED.
        fs_err::remove_file(source.join("sub").join("b.conf")).expect("delete leaf");
        let mut after = crate::status::ManagedTargets::default();
        insert_managed_targets(&entry, &origin, &root, &mut after);
        assert_eq!(
            after.targets,
            [manage_key(&target.join("a.conf"))].into_iter().collect(),
            "a deleted source leaf must no longer be a managed target"
        );
    }

    /// A tree-mode entry whose remote checkout is not on this machine must not
    /// let its recorded leaves classify ORPHANED (and be reaped): its leaves
    /// are unknowable, so its declared target becomes an indeterminate root
    /// that still governs everything beneath it.
    #[test]
    fn insert_managed_targets_marks_an_unmaterialized_remote_tree_indeterminate() {
        use crate::status::manage_key;

        let td = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf8 temp path");
        let target = root.join("dest");
        fs_err::create_dir_all(&target).expect("mkdir target");

        let entry = ManagedEntry {
            kind: EntryKind::Directory,
            mode: FileMode::CopyTree,
            source: Utf8PathBuf::from("skills"),
            targets: vec![target.clone()],
            when: None,
            remote: Some("humanizer".to_owned()),
        };
        let origin = EntryOrigin {
            source_root: None,
            remote: Some("humanizer".to_owned()),
        };

        let mut got = crate::status::ManagedTargets::default();
        insert_managed_targets(&entry, &origin, &root, &mut got);

        assert!(got.targets.is_empty(), "no leaf is enumerable");
        assert!(
            got.governs(&manage_key(&target.join("SKILL.md"))),
            "a recorded leaf under the declared target must still count as managed"
        );
        assert!(
            !got.governs(&manage_key(&root.join("elsewhere"))),
            "a path outside the declared target is not governed"
        );
        assert_eq!(
            got.unmaterialized_remotes.iter().collect::<Vec<_>>(),
            ["humanizer"],
            "the remote must be reported so status can warn"
        );
    }

    /// A non-`symlink-tree` entry contributes its declared target(s)
    /// directly, with no source walk. That covers `[[file]]`
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
            remote: None,
        };

        let origin = EntryOrigin {
            source_root: Some(module.clone()),
            remote: None,
        };
        let mut got = crate::status::ManagedTargets::default();
        insert_managed_targets(&entry, &origin, &root, &mut got);

        let expected: BTreeSet<Utf8PathBuf> =
            [manage_key(&t1), manage_key(&t2)].into_iter().collect();
        assert_eq!(got.targets, expected);
    }

    /// The guard behind the remote trust boundary. A checkout Patina wrote
    /// never contains a symbolic link (`core.symlinks=false`). One found
    /// under a remote tree source therefore means the cache was made or
    /// altered by something else, and deploying through it could read or
    /// plant paths outside the checkout.
    #[test]
    fn a_symlink_anywhere_under_a_remote_tree_source_fails_the_plan() {
        use crate::test_util::symlink_file;

        let td = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf8 temp path");
        let tree = root.join("checkout").join("skills");
        fs_err::create_dir_all(tree.join("sub")).expect("mkdir tree");
        fs_err::write(tree.join("ok.md"), b"fine").expect("write plain leaf");

        reject_symlink_leaves("humanizer", &tree).expect("a link-free tree passes");

        let outside = root.join("secret");
        fs_err::write(&outside, b"key material").expect("write outside file");
        symlink_file(&outside, &tree.join("sub").join("creds"));

        let err = reject_symlink_leaves("humanizer", &tree)
            .expect_err("a nested symlink must fail the plan");
        let message = err.to_string();
        assert!(
            message.contains("creds") && message.contains("humanizer"),
            "the error must name the link and the remote: {message}"
        );
    }

    // --- plan-time disposition classification ------------
    //
    // These drive `classify_entry` / `classify_target`, the unit `plan`
    // calls during entry resolution to populate `ResolvedOperation` and the
    // durable `PlannedOperation`. They build tempdir fixtures matching the
    // scenarios so no repository discovery or
    // process-env mutation is needed (the workspace forbids `unsafe`).

    fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().expect("create tempdir");
        let path =
            Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
        let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
        (td, canonical)
    }

    fn make_symlink(target: &Utf8Path, link: &Utf8Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).expect("create unix symlink");
        #[cfg(windows)]
        {
            if target.is_dir() {
                std::os::windows::fs::symlink_dir(target, link)
                    .expect("create windows dir symlink");
            } else {
                std::os::windows::fs::symlink_file(target, link)
                    .expect("create windows file symlink");
            }
        }
    }

    fn classify_one(mode: FileMode, source: &Utf8Path, target: &Utf8Path) -> Disposition {
        let engine = Engine::new();
        let resolver = Resolver::new(Builtins::for_tests());
        let dispositions = classify_entry(
            mode,
            source,
            std::slice::from_ref(&target.to_path_buf()),
            &engine,
            &resolver,
        )
        .expect("classify entry");
        dispositions
            .first()
            .expect("one disposition per target")
            .aggregate
    }

    // A symlink already pointing at its source, a copy whose bytes
    // match, and a template whose bytes match the rendered output all
    // classify Unchanged.
    #[test]
    fn satisfied_symlink_copy_and_template_all_classify_unchanged() {
        let (_td, dir) = utf8_tempdir();

        // symlink already pointing at the source.
        let sym_source = dir.join("zshrc");
        fs_err::write(&sym_source, b"export X=1").expect("write symlink source");
        let sym_target = dir.join("link-zshrc");
        make_symlink(&sym_source, &sym_target);
        assert_eq!(
            classify_one(FileMode::Symlink, &sym_source, &sym_target),
            Disposition::Unchanged,
        );

        // copy whose target bytes already match the source.
        let copy_source = dir.join("config");
        fs_err::write(&copy_source, b"same bytes").expect("write copy source");
        let copy_target = dir.join("out-config");
        fs_err::write(&copy_target, b"same bytes").expect("write matching copy target");
        assert_eq!(
            classify_one(FileMode::Copy, &copy_source, &copy_target),
            Disposition::Unchanged,
        );

        // template whose target already holds the rendered output.
        let tmpl_source = dir.join("gitconfig.tmpl");
        fs_err::write(&tmpl_source, b"name = {{ who }}").expect("write template source");
        let tmpl_target = dir.join("out-gitconfig");
        let engine = Engine::new();
        let resolver = Resolver::new(Builtins::for_tests())
            .with_repo_shared([("who", "kevin")])
            .expect("layer accepted");
        let rendered = engine
            .render("name = {{ who }}", &resolver)
            .expect("render template");
        fs_err::write(&tmpl_target, rendered.as_bytes()).expect("write matching template target");
        let dispositions = classify_entry(
            FileMode::TemplateRender,
            &tmpl_source,
            std::slice::from_ref(&tmpl_target),
            &engine,
            &resolver,
        )
        .expect("classify template");
        assert_eq!(
            dispositions.first().expect("one disposition").aggregate,
            Disposition::Unchanged,
        );
    }

    // With the copy target's bytes mutated and the symlink target
    // deleted, the copy classifies Update, the symlink classifies Create,
    // and an untouched template stays Unchanged.
    #[test]
    fn mutated_copy_is_update_and_deleted_symlink_is_create() {
        let (_td, dir) = utf8_tempdir();

        // copy target present but bytes differ → Update.
        let copy_source = dir.join("config");
        fs_err::write(&copy_source, b"new contents").expect("write copy source");
        let copy_target = dir.join("out-config");
        fs_err::write(&copy_target, b"stale contents").expect("write drifted copy target");
        assert_eq!(
            classify_one(FileMode::Copy, &copy_source, &copy_target),
            Disposition::Update,
        );

        // symlink target absent → Create.
        let sym_source = dir.join("zshrc");
        fs_err::write(&sym_source, b"export X=1").expect("write symlink source");
        let sym_target = dir.join("absent-link");
        assert_eq!(
            classify_one(FileMode::Symlink, &sym_source, &sym_target),
            Disposition::Create,
        );

        // template target still matches the rendered output → Unchanged.
        let tmpl_source = dir.join("gitconfig.tmpl");
        fs_err::write(&tmpl_source, b"static body").expect("write template source");
        let tmpl_target = dir.join("out-gitconfig");
        fs_err::write(&tmpl_target, b"static body").expect("write matching template target");
        assert_eq!(
            classify_one(FileMode::TemplateRender, &tmpl_source, &tmpl_target),
            Disposition::Unchanged,
        );
    }

    // A copy-tree materialized to three leaves with one drifted out
    // of band classifies that one leaf Update and the other two Unchanged,
    // and the durable tree op's aggregate disposition is Update.
    #[test]
    fn copy_tree_with_one_drifted_leaf_aggregates_to_update() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("themes");
        fs_err::create_dir_all(&source).expect("mkdir source tree");
        fs_err::write(source.join("a.conf"), b"alpha").expect("write a");
        fs_err::write(source.join("b.conf"), b"beta").expect("write b");
        fs_err::write(source.join("c.conf"), b"gamma").expect("write c");

        // Target tree mirrors the source, then one leaf is altered out of band.
        let target = dir.join("out-themes");
        fs_err::create_dir_all(&target).expect("mkdir target tree");
        fs_err::write(target.join("a.conf"), b"alpha").expect("write target a");
        fs_err::write(target.join("b.conf"), b"DRIFTED").expect("write drifted target b");
        fs_err::write(target.join("c.conf"), b"gamma").expect("write target c");

        let engine = Engine::new();
        let resolver = Resolver::new(Builtins::for_tests());
        let dispositions = classify_entry(
            FileMode::CopyTree,
            &source,
            std::slice::from_ref(&target),
            &engine,
            &resolver,
        )
        .expect("classify copy-tree");
        let tree = dispositions.first().expect("one disposition per target");

        // The durable per-op aggregate is Update: not every leaf is Unchanged.
        assert_eq!(tree.aggregate, Disposition::Update);

        // Exactly the drifted leaf (b.conf) is Update; the other two are
        // Unchanged. Compare on leaf-name component so the assertion is
        // independent of the platform path separator.
        let mut by_leaf: Vec<(String, Disposition)> = tree
            .leaves
            .iter()
            .map(|leaf| (leaf.relative.as_str().to_owned(), leaf.disposition))
            .collect();
        by_leaf.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            by_leaf,
            vec![
                ("a.conf".to_owned(), Disposition::Unchanged),
                ("b.conf".to_owned(), Disposition::Update),
                ("c.conf".to_owned(), Disposition::Unchanged),
            ],
        );
    }

    // A copy-tree whose target directory is absent classifies the whole op
    // Create without enumerating leaves.
    #[test]
    fn copy_tree_absent_target_is_create_aggregate() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("themes");
        fs_err::create_dir_all(&source).expect("mkdir source tree");
        fs_err::write(source.join("a.conf"), b"alpha").expect("write a");
        let target = dir.join("absent-themes");

        let engine = Engine::new();
        let resolver = Resolver::new(Builtins::for_tests());
        let dispositions = classify_entry(
            FileMode::CopyTree,
            &source,
            std::slice::from_ref(&target),
            &engine,
            &resolver,
        )
        .expect("classify copy-tree");
        let tree = dispositions.first().expect("one disposition per target");
        assert_eq!(tree.aggregate, Disposition::Create);
        assert!(
            tree.leaves.is_empty(),
            "an absent tree target needs no per-leaf classification"
        );
    }

    // A fully-clean copy-tree (every leaf matches) aggregates to Unchanged.
    #[test]
    fn copy_tree_all_leaves_match_aggregates_to_unchanged() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("themes");
        fs_err::create_dir_all(&source).expect("mkdir source tree");
        fs_err::write(source.join("a.conf"), b"alpha").expect("write a");
        fs_err::write(source.join("b.conf"), b"beta").expect("write b");
        let target = dir.join("out-themes");
        fs_err::create_dir_all(&target).expect("mkdir target tree");
        fs_err::write(target.join("a.conf"), b"alpha").expect("write target a");
        fs_err::write(target.join("b.conf"), b"beta").expect("write target b");

        let engine = Engine::new();
        let resolver = Resolver::new(Builtins::for_tests());
        let dispositions = classify_entry(
            FileMode::CopyTree,
            &source,
            std::slice::from_ref(&target),
            &engine,
            &resolver,
        )
        .expect("classify copy-tree");
        assert_eq!(
            dispositions.first().expect("one disposition").aggregate,
            Disposition::Unchanged,
        );
    }

    // The durable plan carries the classified disposition: a satisfied copy
    // entry assembles a `PlannedOperation::Copy` whose disposition is
    // Unchanged, threaded from `ResolvedEntry` through `assemble_plan_operations`.
    #[test]
    fn assemble_plan_threads_disposition_onto_durable_operation() {
        let resolved = ResolvedEntry {
            mode: FileMode::Copy,
            source: Utf8PathBuf::from("/repo/config"),
            targets: vec![Utf8PathBuf::from("/home/out-config")],
            dispositions: vec![TargetDisposition {
                aggregate: Disposition::Unchanged,
                leaves: Vec::new(),
            }],
            module: "m".to_owned(),
            declared_source: Utf8PathBuf::from("config"),
        };

        let (operations, resolved_ops) = assemble_plan_operations(vec![Some(resolved)], vec![]);

        assert_eq!(operations.len(), 1);
        let durable = operations.first().expect("one durable operation");
        assert_eq!(durable.disposition(), Disposition::Unchanged);
        let resolved_op = resolved_ops.first().expect("one resolved operation");
        let target_disposition = resolved_op
            .dispositions
            .first()
            .expect("one disposition per target");
        assert_eq!(target_disposition.aggregate, Disposition::Unchanged);
    }
}
