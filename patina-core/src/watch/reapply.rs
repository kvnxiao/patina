//! The watcher's re-apply handler.
//!
//! On a debounced source-edit batch, the watcher re-runs the engine apply under
//! [`LockPolicy::NonBlocking`](crate::LockPolicy): the engine self-acquires the
//! exclusive advisory lock with a single non-blocking attempt and, on
//! contention, returns
//! [`LockError::Contended`] having mutated
//! nothing — not even orphan recovery (the lock is resolved before
//! recovery). The watcher therefore must **not**
//! pre-acquire the lock and then call apply: doing so would self-contend
//! against its own guard. It lets the engine acquire under `NonBlocking` and
//! treats a contention error as "the CLI (or another holder) owns the lock
//! right now" — it logs a `lock_contention_skip` event and skips the cycle. The
//! next FS event re-arms the debounce.
//!
//! A successful re-apply emits an info `re_apply` event carrying the
//! metric fields (`re_apply_id`, `re_apply_duration_ms`,
//! `re_apply_files_changed`), written to the log stack. Its journal
//! `<ts>` is keyed by the hoisted
//! [`current_timestamp`], exactly as `patina apply`
//! keys its own.
//!
//! Re-applying unchanged source is a no-op at the filesystem level (the engine
//! re-materializes byte-identical targets), so a self-triggered re-apply that
//! produces an identical journal record does not loop: the journal-rescan path
//! (the select-loop) re-reads the same record and recomputes the same
//! subscription set.

use crate::ApplyRequest;
use crate::ApplyResult;
use crate::EngineError;
use crate::LockError;
use crate::LockPolicy;
use crate::current_timestamp;
use crate::execute_plan;
use crate::plan_apply;

/// How one re-apply cycle settled. Returned so the select-loop (and tests) can
/// observe the outcome without re-parsing the `tracing` log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapplyOutcome {
    /// The engine acquired the lock and the apply ran to a terminal
    /// [`ApplyResult`]. Carries the count of materialized operations
    /// (`re_apply.files_changed`).
    Applied {
        /// The number of planned operations this re-apply materialized.
        files_changed: usize,
    },
    /// The exclusive lock was held by another holder (a CLI `apply` /
    /// `rollback` / `promote` / `add` / `remove`). The cycle skipped without
    /// mutating anything; the watcher logged a `lock_contention_skip` event.
    Skipped,
    /// Planning or executing failed for a reason other than lock contention.
    /// The watcher logs the error and stays running; the cycle produced no
    /// re-apply.
    Failed,
}

/// Drive one watcher re-apply cycle under [`LockPolicy::NonBlocking`].
///
/// Plans the apply from the process-resolved repository and state directory
/// (the watcher inherits the same `PATINA_REPO` / state-dir resolution the CLI
/// uses), keys the journal `<ts>` with [`current_timestamp`], then executes
/// under `NonBlocking`:
///
/// - On success it emits an info `re_apply` event with `re_apply_id`,
///   `re_apply_duration_ms`, and `re_apply_files_changed`, and returns
///   [`ReapplyOutcome::Applied`].
/// - On [`LockError::Contended`] it emits a debug `lock_contention_skip` event
///   (`skip_reason = "lock_held"`) and returns [`ReapplyOutcome::Skipped`],
///   having mutated nothing.
/// - On any other engine error it emits a warn `re_apply_failed` event and
///   returns [`ReapplyOutcome::Failed`]; a failed re-apply never crashes the
///   watcher.
///
/// `re_apply_id` is the journal timestamp, which is unique per cycle and keys
/// the journal and backups this re-apply wrote, so a log reader can join the
/// metric event to the on-disk artifacts.
pub async fn run_reapply() -> ReapplyOutcome {
    let id = current_timestamp();
    let request = ApplyRequest::default();

    let resolved = match plan_apply(&request, id.clone()) {
        Ok(resolved) => resolved,
        Err(error) => return fail(&id, &error),
    };
    let files_changed = resolved.operations.len();

    let started = std::time::Instant::now();
    match execute_plan(&resolved, &request, LockPolicy::NonBlocking).await {
        Ok(result) => {
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            tracing::info!(
                target: "patina_core",
                re_apply_id = id.as_str(),
                re_apply_duration_ms = duration_ms,
                re_apply_files_changed = files_changed,
                re_apply_result = result_label(&result),
                "re_apply"
            );
            ReapplyOutcome::Applied { files_changed }
        }
        Err(EngineError::Lock(LockError::Contended { .. })) => {
            // The CLI (or another holder) owns the exclusive lock. The engine
            // returned before any mutation: no
            // plan, COMMIT, backup, or orphan recovery. Skip this cycle; the
            // next FS event re-arms the debounce.
            tracing::debug!(
                target: "patina_core",
                skip_reason = "lock_held",
                "lock_contention_skip"
            );
            ReapplyOutcome::Skipped
        }
        Err(error) => fail(&id, &error),
    }
}

/// Log a non-contention re-apply failure and return [`ReapplyOutcome::Failed`].
/// A failed re-apply is logged and survived, never fatal to the watcher.
fn fail(id: &str, error: &EngineError) -> ReapplyOutcome {
    tracing::warn!(
        target: "patina_core",
        re_apply_id = id,
        error = %error,
        "re_apply_failed"
    );
    ReapplyOutcome::Failed
}

/// The stable `re_apply.result` label for a terminal [`ApplyResult`], so the
/// structured log distinguishes a committed re-apply from a hook-driven
/// rollback or abort without re-deriving it from other fields.
fn result_label(result: &ApplyResult) -> &'static str {
    match result {
        ApplyResult::Applied { .. } => "applied",
        ApplyResult::RolledBack { .. } => "rolled_back",
        ApplyResult::Aborted { .. } => "aborted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_label_names_each_terminal_outcome() {
        // The label is part of the re_apply event surface; each terminal
        // ApplyResult must map to its own stable word so a log reader can tell
        // a committed re-apply from a rollback or abort. Asserting all three
        // arms (not one) gates the mapping against a variant being dropped or
        // collapsed.
        assert_eq!(
            result_label(&ApplyResult::Applied {
                warnings: Vec::new(),
                up_to_date: false,
            }),
            "applied"
        );
        assert_eq!(
            result_label(&ApplyResult::RolledBack {
                failed_hook: "h".into()
            }),
            "rolled_back"
        );
        assert_eq!(
            result_label(&ApplyResult::Aborted {
                failed_hook: "h".into()
            }),
            "aborted"
        );
    }
}
