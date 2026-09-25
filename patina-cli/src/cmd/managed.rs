//! Shared helpers for the commands that edit a single managed target under
//! one held exclusive lock and record the edit in a new commit.
//!
//! `remove` and `promote` follow the same shape. Each takes one exclusive
//! advisory lock for the whole command, then locates the journaled
//! [`ExpectedTarget`] for an input path in the latest commit with
//! [`Recorded::find`]. A command that refuses or is declined before recovering
//! returns through [`refused`], which warns about a pending interrupted apply
//! and writes nothing. Otherwise the command reverts any interrupted apply with
//! [`recover_held`] before its first write, does its own filesystem work, and
//! writes a new `<ts>.COMMIT` derived from the latest record through
//! [`Recorded`]. The command does not write another target or run a hook.
//! Because the new commit records every target as `Unchanged` and does not
//! list a reaped target, rolling back the commit does not change a file.
//! `promote` can still refuse after [`recover_held`] has written, when the
//! recovery changed its target.

use crate::cmd::apply::report_recovery;
use crate::cmd::apply::warn_pending_apply;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::EngineError;
use patina_core::ExpectedTarget;
use patina_core::LockGuard;
use patina_core::LockKind;
use patina_core::PendingApply;
use patina_core::RecoveryReport;
use patina_core::acquire_lock;
use patina_core::commit_record_only;
use patina_core::discard_record_only_commit;
use patina_core::exclusive_timeout;
use patina_core::journal::OsSyncer;
use patina_core::manage_key;
use patina_core::orphan_plans;
use patina_core::read_latest_commit;
use patina_core::recover_orphans;
use patina_core::resolve_state_dir;

/// The `.tmpl` source suffix marking an implicit template-rendered target.
///
/// `remove` re-renders such a source to reconstruct the last-applied content.
/// `promote` refuses the target outright.
pub(crate) const TEMPLATE_SUFFIX: &str = ".tmpl";

/// Resolve the per-machine state directory and acquire the engine's
/// exclusive advisory lock at `<state>/lock`.
///
/// The caller holds the returned guard for the whole command.
///
/// # Errors
///
/// Returns an error when the state directory cannot be resolved, or the lock
/// cannot be acquired within [`exclusive_timeout`]. A resolution failure is
/// exit 1; a lock timeout maps to exit 4 through the engine-error chain.
pub(crate) fn acquire_state_and_lock() -> Result<(Utf8PathBuf, LockGuard)> {
    let state = resolve_state_dir().map_err(EngineError::from)?;
    let lock_path = state.join("lock");
    let guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .map_err(EngineError::from)
        .context("failed to acquire the exclusive lock")?;
    Ok((state, guard))
}

/// Revert every interrupted apply under the held exclusive lock and report it.
///
/// Call this after consent and before the command's first write.
///
/// # Errors
///
/// Returns an error when an orphan plan cannot be read or reverted.
pub(crate) fn recover_held(
    state: &Utf8Path,
    reporter: &mut impl Reporter,
) -> Result<RecoveryReport> {
    let report = recover_orphans(state)
        .map_err(EngineError::from)
        .context("failed to recover an interrupted apply")?;
    report_recovery(&report, reporter);
    Ok(report)
}

/// Return `code` for a command that refused or was declined, warning first
/// when an interrupted apply is pending.
///
/// The caller holds the exclusive lock, so a plan without a sentinel is an
/// orphan.
///
/// # Errors
///
/// Returns an error when the journal directory cannot be read.
pub(crate) fn refused(state: &Utf8Path, reporter: &mut impl Reporter, code: i32) -> Result<i32> {
    let orphans = orphan_plans(state.join("journal"))
        .map_err(EngineError::from)
        .context("failed to read the journal")?;
    if !orphans.is_empty() {
        warn_pending_apply(PendingApply::Interrupted, reporter);
    }
    Ok(code)
}

/// The latest commit's targets and the target among them that a command edits.
#[derive(Debug)]
pub(crate) struct Recorded {
    /// The recorded expectation of the edited target.
    pub(crate) expected: ExpectedTarget,
    targets: Vec<ExpectedTarget>,
    index: usize,
}

impl Recorded {
    /// Read the latest commit under `state` and find the target whose
    /// [`manage_key`] is `target_key`. Return `None` when there is no latest
    /// commit or it does not record that target.
    ///
    /// # Errors
    ///
    /// Returns an error when the journal cannot be read or its newest record
    /// is from a newer binary.
    pub(crate) fn find(state: &Utf8Path, target_key: &Utf8Path) -> Result<Option<Self>> {
        let Some(record) = read_latest_commit(state.join("journal")).map_err(EngineError::from)?
        else {
            return Ok(None);
        };
        let found = record
            .targets
            .iter()
            .enumerate()
            .find(|(_, expected)| manage_key(Utf8Path::new(expected.target())) == target_key)
            .map(|(index, expected)| (index, expected.clone()));
        Ok(found.map(|(index, expected)| Self {
            expected,
            targets: record.targets,
            index,
        }))
    }

    /// Commit the latest record without the edited target, and return the
    /// commit's `<ts>` for [`discard_commit`].
    ///
    /// # Errors
    ///
    /// Returns an error when the commit cannot be written.
    pub(crate) fn commit_without(self, state: &Utf8Path) -> Result<String> {
        let index = self.index;
        let targets = self
            .targets
            .into_iter()
            .enumerate()
            .filter(|(position, _)| *position != index)
            .map(|(_, expected)| expected)
            .collect();
        commit_targets(state, targets)
    }

    /// Commit the latest record with the edited content target's hash
    /// replaced by `hash`.
    ///
    /// # Errors
    ///
    /// Returns an error when the edited target is not a content target or the
    /// commit cannot be written.
    pub(crate) fn commit_with_hash(self, state: &Utf8Path, hash: [u8; 32]) -> Result<()> {
        let mut targets = self.targets;
        let Some(ExpectedTarget::Content { hash: recorded, .. }) = targets.get_mut(self.index)
        else {
            bail!("the promoted target is not a content target");
        };
        *recorded = hash;
        commit_targets(state, targets).map(|_timestamp| ())
    }
}

fn commit_targets(state: &Utf8Path, targets: Vec<ExpectedTarget>) -> Result<String> {
    let timestamp = commit_record_only(state, targets, &OsSyncer)
        .map_err(EngineError::from)
        .context("failed to write the commit record")?;
    Ok(timestamp)
}

/// Delete the commit that [`Recorded::commit_without`] wrote at `timestamp`,
/// so the record before it is the latest again.
///
/// # Errors
///
/// Returns an error when the commit cannot be deleted.
pub(crate) fn discard_commit(state: &Utf8Path, timestamp: &str) -> Result<()> {
    discard_record_only_commit(state, timestamp, &OsSyncer)
        .map_err(EngineError::from)
        .context("failed to discard the commit record")
}
