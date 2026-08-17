//! Shared helpers for the commands that edit a single managed target under
//! one held exclusive lock and re-journal by re-applying.
//!
//! `remove` and `promote` follow the same shape. Each takes one exclusive
//! advisory lock for the whole command, then locates the journaled
//! [`ExpectedTarget`](patina_core::ExpectedTarget) for an input path in the
//! latest commit. Each then does its own filesystem work and re-journals by
//! driving the engine re-apply under [`LockPolicy::Held`]. The fresh
//! `<ts>.COMMIT` records the new managed state.
//!
//! The lock acquisition and the re-apply live here. Neither command repeats
//! the lock path, the engine-error mapping, or the re-plan / re-execute
//! sequence.

use anyhow::Context;
use anyhow::Result;
use camino::Utf8PathBuf;
use patina_core::ApplyRequest;
use patina_core::EngineError;
use patina_core::LockGuard;
use patina_core::LockKind;
use patina_core::LockPolicy;
use patina_core::acquire_lock;
use patina_core::current_timestamp;
use patina_core::exclusive_timeout;
use patina_core::execute_plan;
use patina_core::plan_apply;
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

/// Re-journal the current managed set by re-applying under the already-held
/// lock `guard`.
///
/// The plan is computed against the manifests as they stand, so an edit the
/// caller just made is included. Execution runs under [`LockPolicy::Held`] and
/// writes a fresh `<ts>.COMMIT` recording the new expected state.
///
/// # Errors
///
/// Returns an error when the re-plan or the re-apply fails.
pub(crate) async fn rejournal(guard: LockGuard) -> Result<()> {
    let timestamp = current_timestamp();
    let resolved = plan_apply(&ApplyRequest::default(), &timestamp).context("failed to re-plan")?;
    execute_plan(&resolved, &ApplyRequest::default(), LockPolicy::Held(guard))
        .await
        .context("re-apply failed")?;
    Ok(())
}
