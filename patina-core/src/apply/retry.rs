//! Windows `ERROR_SHARING_VIOLATION` retry-with-backoff wrapper.
//!
//! On Windows, antivirus scans, cloud-sync uploads, and indexers
//! transiently hold a file open with no sharing, so a write that would
//! otherwise succeed fails with `ERROR_SHARING_VIOLATION` (Win32 error code
//! 32). [`with_sharing_violation_retry`] wraps a single write operation and,
//! on Windows only, retries that specific failure with a fixed exponential
//! backoff (`50, 100, 200, 400, 800, 1600` ms — six retries, ~3.15s total).
//! Any other error is re-raised immediately, and the violation is re-raised
//! unchanged after the sixth failed retry so the normal apply
//! failure/rollback path handles it.
//!
//! On macOS and Linux there is no `FILE_SHARE_NONE` equivalent for ordinary
//! writes, so the wrapper is a pure pass-through: it runs the operation
//! exactly once and never emits a retry event.
//!
//! Each retry emits a `fs_write_retry` debug-level `tracing` event with
//! `attempt`, `delay_ms`, and `error` fields, so the retry behaviour is
//! observable under `RUST_LOG=patina_core=debug`.

/// `ERROR_SHARING_VIOLATION` — the Win32 error code (32) a write hits when
/// another process holds the target open with no sharing. Matched via
/// [`std::io::Error::raw_os_error`] exactly as `lock::is_contended` matches
/// `fs2`'s contended-lock error by its raw OS code.
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;

/// Fixed exponential backoff between retries, in milliseconds. Six
/// entries means six retries after the initial attempt; the cumulative wait
/// is `50+100+200+400+800+1600 = 3150` ms (~3.15s) before the violation is
/// re-raised as a real failure.
#[cfg(windows)]
const BACKOFF_SCHEDULE_MS: &[u64] = &[50, 100, 200, 400, 800, 1600];

/// Run a single filesystem write operation, retrying on Windows when it
/// fails with `ERROR_SHARING_VIOLATION`.
///
/// On Windows, a write that fails with `ERROR_SHARING_VIOLATION` is retried
/// after each delay in the fixed backoff schedule (`50, 100, 200, 400, 800,
/// 1600` ms). The first success returns immediately; any non-violation error
/// is returned immediately without retrying; and if every retry still hits
/// the violation, the last violation error is returned unchanged so the
/// apply pipeline can roll back or abort. Each retry emits a debug-level
/// `fs_write_retry` `tracing` event with `attempt`, `delay_ms`, and `error`.
///
/// On every other platform this runs `op` exactly once and returns its
/// result verbatim — no retry, no sleep, no `tracing` event.
///
/// # Errors
///
/// Returns the [`std::io::Error`] produced by `op`: the first non-violation
/// error on any platform, or — on Windows after the retry budget is
/// exhausted — the final `ERROR_SHARING_VIOLATION` error.
pub(crate) fn with_sharing_violation_retry<T>(
    mut op: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    #[cfg(windows)]
    {
        for (index, &delay_ms) in BACKOFF_SCHEDULE_MS.iter().enumerate() {
            match op() {
                Ok(value) => return Ok(value),
                Err(err) => {
                    if err.raw_os_error() != Some(ERROR_SHARING_VIOLATION) {
                        // Not a sharing violation: surface it to the apply
                        // pipeline without consuming the retry budget.
                        return Err(err);
                    }
                    let attempt = index + 1;
                    tracing::debug!(
                        target: "patina_core",
                        attempt,
                        delay_ms,
                        error = %err,
                        "fs_write_retry"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
            }
        }
        // Six-retry budget spent: make the final attempt and surface its
        // result verbatim — the last `ERROR_SHARING_VIOLATION`, any other
        // error, or a success if the contention just cleared.
        op()
    }

    #[cfg(not(windows))]
    {
        // No `FILE_SHARE_NONE` equivalent for ordinary writes off Windows:
        // run once and surface the result as-is (pass-through).
        op()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Off Windows the wrapper is a pass-through: `op` runs exactly once and
    /// its `Ok` value is returned verbatim.
    #[cfg(not(windows))]
    #[test]
    fn non_windows_runs_op_exactly_once_on_success() {
        let calls = Cell::new(0);
        let result = with_sharing_violation_retry(|| {
            calls.set(calls.get() + 1);
            Ok::<_, std::io::Error>(7)
        });
        assert_eq!(result.expect("ok"), 7);
        assert_eq!(calls.get(), 1, "pass-through must not retry");
    }

    /// Off Windows an error surfaces on the first attempt with no retry,
    /// even when it is a synthetic "sharing violation"-shaped error: the
    /// retry path is Windows-only, so the code matters nowhere else.
    #[cfg(not(windows))]
    #[test]
    fn non_windows_surfaces_first_error_without_retry() {
        let calls = Cell::new(0);
        let result = with_sharing_violation_retry(|| {
            calls.set(calls.get() + 1);
            Err::<u8, _>(std::io::Error::from_raw_os_error(32))
        });
        assert!(result.is_err(), "error must surface");
        assert_eq!(calls.get(), 1, "pass-through must not retry");
    }

    /// On Windows a non-violation error is re-raised immediately without
    /// consuming any of the retry budget.
    #[cfg(windows)]
    #[test]
    fn windows_non_violation_error_is_not_retried() {
        let calls = Cell::new(0);
        let result = with_sharing_violation_retry(|| {
            calls.set(calls.get() + 1);
            Err::<u8, _>(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert!(result.is_err(), "error must surface");
        assert_eq!(calls.get(), 1, "non-violation error must not retry");
    }

    /// On Windows a violation that clears mid-schedule succeeds, having
    /// retried only as many times as it took.
    #[cfg(windows)]
    #[test]
    fn windows_violation_then_success_retries_until_clear() {
        let calls = Cell::new(0);
        let result = with_sharing_violation_retry(|| {
            let n = calls.get() + 1;
            calls.set(n);
            if n < 3 {
                Err(std::io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION))
            } else {
                Ok(())
            }
        });
        assert!(result.is_ok(), "should succeed once the violation clears");
        assert_eq!(calls.get(), 3, "two retries then success");
    }

    /// On Windows a persistent violation exhausts the six-retry budget
    /// (seven total attempts: the initial one plus six retries) and the
    /// final violation is re-raised unchanged.
    #[cfg(windows)]
    #[test]
    fn windows_persistent_violation_exhausts_budget_then_reraises() {
        let calls = Cell::new(0);
        let result = with_sharing_violation_retry(|| {
            calls.set(calls.get() + 1);
            Err::<u8, _>(std::io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION))
        });
        let err = result.expect_err("budget exhausted");
        assert_eq!(err.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
        assert_eq!(
            calls.get(),
            BACKOFF_SCHEDULE_MS.len() + 1,
            "initial attempt plus six retries"
        );
    }
}
