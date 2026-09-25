//! Shared helpers for the commands that edit a single managed target under
//! one held exclusive lock and re-journal by re-applying.
//!
//! `remove` and `promote` follow the same shape. Each takes one exclusive
//! advisory lock for the whole command, then locates the journaled
//! [`ExpectedTarget`](patina_core::ExpectedTarget) for an input path in the
//! latest commit. A command that refuses or is declined returns through
//! [`refused`], which warns about a pending interrupted apply and writes
//! nothing. Otherwise the command reverts any interrupted apply with
//! [`recover_held`] before its first write, does its own filesystem work, and
//! re-journals by driving the engine re-apply under [`LockPolicy::Held`]. The
//! fresh `<ts>.COMMIT` records the new managed state.
//!
//! The lock acquisition and the re-apply live here. Neither command repeats
//! the lock path, the engine-error mapping, or the re-plan / re-execute
//! sequence.

use crate::cmd::apply::report_recovery;
use crate::cmd::apply::warn_pending_apply;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::ApplyRequest;
use patina_core::EngineError;
use patina_core::LockGuard;
use patina_core::LockKind;
use patina_core::LockPolicy;
use patina_core::PendingApply;
use patina_core::Reap;
use patina_core::RecoveryReport;
use patina_core::acquire_lock;
use patina_core::current_timestamp;
use patina_core::exclusive_timeout;
use patina_core::execute_plan;
use patina_core::orphan_plans;
use patina_core::plan_apply;
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
/// The returned guard is held by the caller for the whole command and reused
/// by [`rejournal`] via [`LockPolicy::Held`], so the re-apply does not block on
/// the command's own lock.
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

/// Re-journal the current managed set by re-applying under the already-held
/// lock `guard`.
///
/// The plan is computed against the manifests as they stand, so an edit the
/// caller just made is included. It is planned under [`Reap::Nothing`], so the
/// re-apply removes no target, including the one `remove` just replaced and
/// unmanaged. Execution runs under [`LockPolicy::Held`] and writes a fresh
/// `<ts>.COMMIT` recording the new expected state.
///
/// # Errors
///
/// Returns an error when the re-plan or the re-apply fails.
pub(crate) fn rejournal(guard: LockGuard) -> Result<()> {
    let request = ApplyRequest {
        reap: Reap::Nothing,
        ..ApplyRequest::default()
    };
    let timestamp = current_timestamp();
    let resolved = plan_apply(&request, &timestamp).context("failed to re-plan")?;
    execute_plan(&resolved, &request, LockPolicy::Held(guard)).context("re-apply failed")?;
    Ok(())
}
