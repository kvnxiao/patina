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
use crate::config::RemoteName;
use crate::config::RemoteSpec;
use crate::error::EngineError;
use camino::Utf8PathBuf;
use std::time::Duration;

/// One declared remote: what the root manifest says, and what it is pinned to.
#[derive(Debug, Clone)]
pub struct RemoteView {
    /// The root manifest's `[[remote]]` table.
    pub spec: RemoteSpec,
    /// The current pin, or `None` when the remote has never been pinned.
    pub pin: Option<LockEntry>,
}

impl RemoteView {
    /// The remote's name, which entries select it by and which keys its pin,
    /// its cache directory, and every `patina remote` verb.
    #[must_use = "the name keys the pin, the cache directory, and every verb"]
    pub fn name(&self) -> &RemoteName {
        &self.spec.name
    }
}

/// Everything the `patina remote` commands operate over.
#[derive(Debug)]
pub struct RemoteInventory {
    /// Canonical repository root; the lockfile sits directly inside it.
    pub repo_root: Utf8PathBuf,
    /// Per-machine state directory, which holds the cache and notice files.
    pub state_dir: Utf8PathBuf,
    /// The root manifest's `[patina] remote_min_age`, when it declares one.
    pub global_min_age: Option<Duration>,
    /// The committed pins.
    pub lockfile: Lockfile,
    /// Every declared remote, in root-manifest declaration order.
    pub remotes: Vec<RemoteView>,
}

/// Enumerate the resolved repository's declared remotes and their pins.
///
/// Reads the root manifest alone: a remote exists because the root declares it,
/// not because some module currently uses it. That is what lets `patina remote
/// update` keep the committed lock complete for machines whose active entry set
/// differs from this one's.
///
/// # Errors
///
/// Returns an [`EngineError`] when repository or state-directory resolution,
/// root-manifest parsing, or the lockfile read fails.
pub fn inventory() -> Result<RemoteInventory, EngineError> {
    let repo_root = crate::discovery::resolve_repository_root()?;
    let state_dir = crate::state_dir::resolve()?;
    let root_config = crate::config::parse_root_config(&repo_root.join("patina.toml"))?;
    let lockfile = Lockfile::load(&lockfile_path(&repo_root))?;

    let remotes = root_config
        .remotes
        .into_iter()
        .map(|spec| RemoteView {
            pin: lockfile.get(&spec.name).cloned(),
            spec,
        })
        .collect();

    Ok(RemoteInventory {
        repo_root,
        state_dir,
        global_min_age: root_config.remotes_min_age,
        lockfile,
        remotes,
    })
}

impl RemoteInventory {
    /// The view for the remote `name` addresses, if the root manifest declares
    /// it.
    #[must_use = "the view carries the spec and pin a command operates on"]
    pub fn find(&self, name: &str) -> Option<&RemoteView> {
        self.remotes.iter().find(|view| view.name().matches(name))
    }
}

/// One remote's upstream tip against its pin, from `ls-remote` alone.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// The remote's name.
    pub name: String,
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
        name: view.name().to_string(),
        pinned_rev: view.pin.as_ref().map(|pin| pin.rev.clone()),
        upstream_rev,
    })
}

/// A candidate pin bump and the gate's answer about it.
#[derive(Debug, Clone)]
pub struct Proposal {
    /// The remote's name.
    pub name: String,
    /// The rev the tracked ref points at upstream.
    pub candidate_rev: String,
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
            name: view.name().to_string(),
            candidate_rev,
            current_rev,
            outcome: GateOutcome::AlreadyPinned,
        });
    }

    let git_dir = cache::bare_repo(&inventory.state_dir, view.name());
    git::fetch_history(&git_dir, &view.spec.url, view.spec.git_ref.as_deref())?;
    let candidate_epoch = git::committer_time(&git_dir, &candidate_rev)?;

    // After a full-history fetch of the tracked ref, a pinned rev that is not
    // even present in the repository is provably not reachable from the tip,
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
        name: view.name().to_string(),
        candidate_rev,
        current_rev,
        outcome,
    })
}

/// Record `proposal`'s candidate as the remote's pin and write the lockfile.
///
/// `updated_at` is stamped from `now_rfc3339` and rides the same commit as the
/// `rev` change, so the lockfile never needs a commit of its own.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the lockfile cannot be written.
pub fn accept(
    lockfile: &mut Lockfile,
    repo_root: &camino::Utf8Path,
    view: &RemoteView,
    proposal: &Proposal,
    now_rfc3339: &str,
) -> Result<(), RemoteError> {
    lockfile.insert(
        view.name().clone(),
        LockEntry {
            url: view.spec.url.clone(),
            git_ref: view.spec.git_ref.clone(),
            rev: proposal.candidate_rev.clone(),
            updated_at: now_rfc3339.to_owned(),
        },
    );
    lockfile.save(&lockfile_path(repo_root))
}
