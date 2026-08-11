//! Producer-side orchestration: enumerate the repository's remotes, check them
//! against upstream, and propose pin bumps through the update gate.
//!
//! The split here is deliberate. This module does the network work and computes
//! a [`Proposal`] per remote; it never prompts and never prints. Deciding what
//! to do with a [`GateOutcome::NeedsConfirmation`] belongs to the CLI, which
//! owns the TTY. [`accept`] is the only function that writes a pin.
//!
//! See `docs/REMOTE_SOURCES.md` "Commands" and "The update gate".

use super::RemoteError;
use super::cache;
use super::gate;
use super::gate::GateInputs;
use super::gate::GateOutcome;
use super::git;
use super::lockfile::LockEntry;
use super::lockfile::Lockfile;
use super::lockfile::lockfile_path;
use crate::config::RemoteSpec;
use crate::error::EngineError;
use camino::Utf8PathBuf;
use std::time::Duration;

/// One remote-backed module: what it declares, and what it is pinned to.
#[derive(Debug, Clone)]
pub struct RemoteView {
    /// The module directory name, which doubles as the remote's name.
    pub module: String,
    /// The module's `[remote]` table.
    pub spec: RemoteSpec,
    /// The current pin, or `None` when the remote has never been pinned.
    pub pin: Option<LockEntry>,
}

/// Everything the `patina remote` commands operate over.
#[derive(Debug)]
pub struct RemoteInventory {
    /// Canonical repository root; the lockfile sits directly inside it.
    pub repo_root: Utf8PathBuf,
    /// Per-machine state directory, which holds the cache and notice files.
    pub state_dir: Utf8PathBuf,
    /// The root manifest's `[remotes] min_age`, when it declares one.
    pub global_min_age: Option<Duration>,
    /// The committed pins.
    pub lockfile: Lockfile,
    /// Every remote-backed module, in module-name order (the order
    /// `discover_modules` returns).
    pub remotes: Vec<RemoteView>,
}

/// Enumerate the resolved repository's remote-backed modules and their pins.
///
/// # Errors
///
/// Returns an [`EngineError`] when repository or state-directory resolution,
/// module enumeration, manifest parsing, or the lockfile read fails.
pub fn inventory() -> Result<RemoteInventory, EngineError> {
    let repo_root = crate::discovery::resolve_repository_root()?;
    let state_dir = crate::state_dir::resolve()?;
    let root_config = crate::config::parse_root_config(&repo_root.join("patina.toml"))?;
    let lockfile = Lockfile::load(&lockfile_path(&repo_root))?;

    let mut remotes = Vec::new();
    for module in crate::discovery::discover_modules(&repo_root)? {
        let config = crate::config::parse_module_config(&module.path.join("patina.toml"))?;
        if let Some(spec) = config.remote {
            let pin = lockfile.get(&module.name).cloned();
            remotes.push(RemoteView {
                module: module.name,
                spec,
                pin,
            });
        }
    }

    Ok(RemoteInventory {
        repo_root,
        state_dir,
        global_min_age: root_config.remotes_min_age,
        lockfile,
        remotes,
    })
}

impl RemoteInventory {
    /// The view for `module`, if the repository declares it as a remote.
    #[must_use = "the view carries the spec and pin a command operates on"]
    pub fn find(&self, module: &str) -> Option<&RemoteView> {
        self.remotes.iter().find(|view| view.module == module)
    }
}

/// One remote's upstream tip against its pin, from `ls-remote` alone.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// The module name.
    pub module: String,
    /// The pinned rev, or `None` when unpinned.
    pub pinned_rev: Option<String>,
    /// The rev the tracked ref points at upstream.
    pub upstream_rev: String,
}

impl CheckResult {
    /// Whether the upstream tip has moved off the pin (or the remote is
    /// unpinned, which is also something to act on).
    #[must_use = "the answer decides whether this remote appears in the notice"]
    pub fn has_update(&self) -> bool {
        self.pinned_rev.as_deref() != Some(self.upstream_rev.as_str())
    }
}

/// Read one remote's upstream tip. Downloads no objects.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the remote is unreachable or does not have
/// the tracked ref.
pub fn check_upstream(view: &RemoteView) -> Result<CheckResult, RemoteError> {
    let upstream_rev = git::ls_remote(&view.spec.url, view.spec.git_ref.as_deref())?;
    Ok(CheckResult {
        module: view.module.clone(),
        pinned_rev: view.pin.as_ref().map(|pin| pin.rev.clone()),
        upstream_rev,
    })
}

/// A candidate pin bump and the gate's answer about it.
#[derive(Debug, Clone)]
pub struct Proposal {
    /// The module name.
    pub module: String,
    /// The rev the tracked ref points at upstream.
    pub candidate_rev: String,
    /// The candidate's committer time, Unix seconds. Zero when the gate
    /// answered [`GateOutcome::AlreadyPinned`] and no object was fetched.
    pub candidate_epoch: i64,
    /// The rev currently pinned, or `None` when unpinned.
    pub current_rev: Option<String>,
    /// What the gate decided.
    pub outcome: GateOutcome,
}

/// Fetch upstream for one remote and run the update gate. Writes no pin and
/// touches no target.
///
/// An up-to-date remote short-circuits after `ls-remote`, so the common case
/// downloads nothing.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the remote is unreachable, the tracked ref is
/// missing, or the candidate's committer time cannot be read.
pub fn propose(
    inventory: &RemoteInventory,
    view: &RemoteView,
    now_epoch: i64,
    bypass_age: bool,
) -> Result<Proposal, RemoteError> {
    let candidate_rev = git::ls_remote(&view.spec.url, view.spec.git_ref.as_deref())?;
    let current_rev = view.pin.as_ref().map(|pin| pin.rev.clone());

    if current_rev.as_deref() == Some(candidate_rev.as_str()) {
        return Ok(Proposal {
            module: view.module.clone(),
            candidate_rev,
            candidate_epoch: 0,
            current_rev,
            outcome: GateOutcome::AlreadyPinned,
        });
    }

    // Real history, not a depth-1 fetch: the ancestry check below is only
    // answerable when the commits between the pin and the candidate are present.
    let git_dir = cache::bare_repo(&inventory.state_dir, &view.module);
    git::fetch_history(&git_dir, &view.spec.url, view.spec.git_ref.as_deref())?;
    let candidate_epoch = git::committer_time(&git_dir, &candidate_rev)?;

    // After a full-history fetch of the tracked ref, a pinned rev that is not
    // even present in the repository is provably not reachable from the tip —
    // which is exactly the rewrite the ancestry check exists to catch.
    let descends_from_pin = match &view.pin {
        None => None,
        Some(pin) => Some(
            git::has_commit(&git_dir, &pin.rev)?
                && git::is_ancestor(&git_dir, &pin.rev, &candidate_rev)?,
        ),
    };

    let outcome = gate::evaluate(GateInputs {
        candidate_epoch,
        now_epoch,
        descends_from_pin,
        pinned_updated_at: view.pin.as_ref().and_then(LockEntry::updated_at_epoch),
        min_age: gate::effective_min_age(&view.spec, inventory.global_min_age),
        first_pin: view.pin.is_none(),
        bypass_age,
    });

    Ok(Proposal {
        module: view.module.clone(),
        candidate_rev,
        candidate_epoch,
        current_rev,
        outcome,
    })
}

/// Record `proposal`'s candidate as `module`'s pin and write the lockfile.
///
/// `updated_at` is stamped from `now_rfc3339` and rides the same commit as the
/// `rev` change, so the lockfile never needs a commit of its own.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the lockfile cannot be written.
pub fn accept(
    inventory: &mut RemoteInventory,
    view: &RemoteView,
    proposal: &Proposal,
    now_rfc3339: &str,
) -> Result<(), RemoteError> {
    inventory.lockfile.insert(
        view.module.clone(),
        LockEntry {
            url: view.spec.url.clone(),
            git_ref: view.spec.git_ref.clone(),
            rev: proposal.candidate_rev.clone(),
            updated_at: now_rfc3339.to_owned(),
        },
    );
    inventory
        .lockfile
        .save(&lockfile_path(&inventory.repo_root))
}
