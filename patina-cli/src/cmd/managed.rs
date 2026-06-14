//! Shared scaffolding for the commands that edit a single managed target
//! under one held exclusive lock and re-journal by re-applying.
//!
//! `remove` and `promote` both: take ONE exclusive
//! advisory lock for the whole command, locate the journaled
//! [`ExpectedTarget`](patina_core::ExpectedTarget) for an input path in the
//! latest commit, do
//! command-specific filesystem work, and then re-journal by driving the
//! engine re-apply under [`LockPolicy::Held`] so the fresh `<ts>.COMMIT`
//! reflects the new managed state. This module factors those two shared
//! pieces — the lock acquisition and the re-apply — so neither command
//! duplicates the lock path, the engine-error mapping, or the re-plan /
//! re-execute sequence.

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
/// Shared by the two commands `managed.rs` scaffolds: `remove` re-renders
/// such sources to reconstruct last-applied content, and `promote` refuses to
/// promote a template-rendered target.
pub(crate) const TEMPLATE_SUFFIX: &str = ".tmpl";

/// Resolve the per-machine state directory and acquire the engine's
/// exclusive advisory lock at `<state>/lock`.
///
/// The returned guard is held by the caller for the whole command and reused
/// by [`rejournal`] via [`LockPolicy::Held`], so the re-apply does not
/// self-contend against the command's own lock.
///
/// # Errors
///
/// Returns an error (exit 1, or exit 4 on a lock-acquisition timeout via the
/// engine-error chain) when the state directory cannot be resolved or the
/// lock cannot be acquired within [`exclusive_timeout`].
pub(crate) fn acquire_state_and_lock() -> Result<(Utf8PathBuf, LockGuard)> {
    let state = resolve_state_dir().map_err(EngineError::from)?;
    let lock_path = state.join("lock");
    let guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .map_err(EngineError::from)
        .context("failed to acquire the exclusive lock")?;
    Ok((state, guard))
}

/// Re-journal the current managed set by re-applying under the already-held
/// lock `guard`. Plans against the present manifests (so any edit the caller
/// made is reflected) and executes with [`LockPolicy::Held`], writing a fresh
/// `<ts>.COMMIT` that records the new expected state.
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
