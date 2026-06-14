//! Advisory file lock coordinating mutations and read-only commands.
//!
//! The engine serializes concurrent invocations through a single
//! advisory lock file at `<state>/patina/lock`. Two acquisition modes
//! mirror the two classes of subcommand:
//!
//! - **Exclusive** ([`LockKind::Exclusive`]) — held by the mutating subcommands
//!   (`apply`, `rollback`) for the full apply duration. A second exclusive
//!   acquirer, or any shared acquirer, blocks until the holder releases. The
//!   mutating subcommands cap their wait at [`EXCLUSIVE_TIMEOUT`]; on expiry
//!   [`acquire`] returns [`LockError::Timeout`], which the CLI maps to process
//!   exit code 4.
//! - **Shared** ([`LockKind::Shared`]) — held by the read-only subcommand
//!   (`status`). Multiple shared holders coexist, but a shared holder blocks an
//!   exclusive acquirer and vice versa. `status` caps its wait at
//!   [`SHARED_TIMEOUT`]; on expiry it warns and proceeds without the lock (the
//!   read-only escape hatch). The lock module surfaces the typed
//!   [`LockError::Timeout`]; the warn-and-proceed policy lives at the `status`
//!   call site.
//!
//! ## Acquisition is a bounded poll, not an unbounded block
//!
//! [`acquire`] loops on `fs2`'s non-blocking `try_lock_*` calls with a
//! short sleep between attempts rather than calling the unbounded
//! `lock_exclusive` / `lock_shared`. This is what makes the timeout cap
//! enforceable — and parameterisable down to milliseconds so the
//! integration suite can exercise the timeout path without waiting a real
//! minute.
//!
//! ## Release is the OS's job
//!
//! The returned [`LockGuard`] owns the open lock-file handle. Dropping it
//! drops the handle, and the operating system releases the advisory lock
//! as a side effect of closing the descriptor — including when the
//! process dies abnormally (`SIGKILL` on POSIX, `TerminateProcess` on
//! Windows). There is no "unlock" syscall the engine must remember to
//! call, so a crashed holder never wedges the lock for the next process.
//!
//! # Examples
//!
//! ```no_run
//! use patina_core::lock::{acquire, LockKind, EXCLUSIVE_TIMEOUT};
//!
//! let state = patina_core::state_dir::resolve()?;
//! // Held for the duration of `guard`'s scope; released on drop.
//! let _guard = acquire(&state.join("lock"), LockKind::Exclusive, EXCLUSIVE_TIMEOUT)?;
//! // … mutate the filesystem under the exclusive lock …
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use camino::Utf8Path;
use camino::Utf8PathBuf;
use fs2::FileExt;
use std::time::Duration;
use std::time::Instant;
use thiserror::Error;

/// The exclusive-lock wait cap for the mutating subcommands (`apply`,
/// `rollback`). On expiry [`acquire`] returns [`LockError::Timeout`],
/// which the CLI maps to exit code 4.
pub const EXCLUSIVE_TIMEOUT: Duration = Duration::from_mins(1);

/// The shared-lock wait cap for the read-only subcommand (`status`). On
/// expiry the caller warns and proceeds without the lock.
pub const SHARED_TIMEOUT: Duration = Duration::from_secs(5);

/// Environment variable overriding the exclusive-lock wait cap, in
/// milliseconds. Exists solely so the integration suite can provoke a
/// [`LockError::Timeout`] (and the CLI's exit-code-4 mapping) without
/// waiting the full production [`EXCLUSIVE_TIMEOUT`]. Unset in normal
/// operation; a missing or unparseable value falls back to the constant.
pub const EXCLUSIVE_TIMEOUT_ENV: &str = "PATINA_LOCK_TIMEOUT_MS";

/// The exclusive-lock wait cap honouring the [`EXCLUSIVE_TIMEOUT_ENV`]
/// test override.
///
/// Returns [`EXCLUSIVE_TIMEOUT`] unless `PATINA_LOCK_TIMEOUT_MS` is set to
/// a parseable non-negative integer count of milliseconds, in which case
/// that duration is used instead. The mutating subcommands acquire the
/// exclusive lock with this value so the timeout cap is parameterisable
/// from the test harness.
#[must_use = "the returned duration is the exclusive-lock acquisition cap"]
pub fn exclusive_timeout() -> Duration {
    match std::env::var(EXCLUSIVE_TIMEOUT_ENV) {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(ms) => Duration::from_millis(ms),
            Err(_) => EXCLUSIVE_TIMEOUT,
        },
        Err(_) => EXCLUSIVE_TIMEOUT,
    }
}

/// Interval between successive non-blocking acquisition attempts.
///
/// Short enough that a freed lock is picked up promptly, long enough that
/// a contended wait does not spin the CPU.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Which advisory-lock mode an acquirer requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    /// Mutating access (`apply`, `rollback`). Excludes every other
    /// holder, shared or exclusive.
    Exclusive,
    /// Read-only access (`status`). Coexists with other shared holders;
    /// excludes exclusive holders.
    Shared,
}

impl LockKind {
    /// The lower-case word used to name this mode in error messages
    /// (`"exclusive"` or `"shared"`).
    ///
    /// # Examples
    ///
    /// ```
    /// use patina_core::lock::LockKind;
    ///
    /// assert_eq!(LockKind::Exclusive.label(), "exclusive");
    /// assert_eq!(LockKind::Shared.label(), "shared");
    /// ```
    #[must_use = "the label is a value to use, not a side effect"]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Exclusive => "exclusive",
            Self::Shared => "shared",
        }
    }
}

/// Errors returned by [`acquire`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LockError {
    /// The lock could not be acquired within the caller's timeout
    /// because another process held a conflicting lock for the whole
    /// window. The mutating subcommands map this to exit code 4;
    /// `status` catches it to warn and proceed without the lock.
    #[error("timed out acquiring {} lock on `{path}` after {waited:?}", kind.label())]
    Timeout {
        /// Which acquisition mode timed out.
        kind: LockKind,
        /// The lock-file path that could not be acquired.
        path: Utf8PathBuf,
        /// How long the acquirer waited before giving up.
        waited: Duration,
    },

    /// A single non-blocking acquisition attempt found the lock held by
    /// another holder. Returned only by [`try_acquire`]; distinct from
    /// [`LockError::Timeout`] so a caller that deliberately does not wait
    /// (the watcher's `NonBlocking` apply policy) can match contention
    /// without conflating it with the mutating subcommands' wait-cap
    /// expiry. The blocking [`acquire`] never produces this variant.
    #[error("{} lock on `{path}` is held by another holder", kind.label())]
    Contended {
        /// Which acquisition mode found the lock contended.
        kind: LockKind,
        /// The lock-file path that is held.
        path: Utf8PathBuf,
    },

    /// Opening the lock file or issuing the advisory-lock syscall failed
    /// for a reason other than contention (a permissions problem, a
    /// missing parent directory, an unsupported filesystem).
    #[error("failed to acquire lock on `{path}`: {source}")]
    Io {
        /// The lock-file path.
        path: Utf8PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// A held advisory lock. The OS releases the lock when this guard is
/// dropped (the underlying file handle closes), so the lock is also
/// released automatically if the holding process dies.
///
/// The guard is deliberately opaque: callers hold it for the duration of
/// the critical section and let it drop at scope end. There is no manual
/// `release` — relying on `Drop` is what guarantees release on both the
/// happy path and on a panic-unwind.
#[derive(Debug)]
#[must_use = "the lock is released as soon as this guard is dropped; bind it for the critical section"]
pub struct LockGuard {
    /// The open lock-file handle. Held solely to keep the OS lock alive;
    /// closing it (on drop) releases the advisory lock.
    _file: std::fs::File,
    /// The lock-file path, retained for diagnostics.
    path: Utf8PathBuf,
    /// The mode this guard holds.
    kind: LockKind,
}

impl LockGuard {
    /// The lock-file path this guard holds.
    #[must_use = "returns the lock path for diagnostics"]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// The acquisition mode this guard holds.
    #[must_use = "returns the held lock mode"]
    pub fn kind(&self) -> LockKind {
        self.kind
    }
}

/// Acquire the advisory lock at `path` in the requested `kind`, waiting
/// up to `timeout` for a conflicting holder to release.
///
/// The lock file is created if it does not exist (its byte contents are
/// irrelevant — only the advisory lock on the handle matters). The call
/// polls a non-blocking acquisition every `POLL_INTERVAL` until it
/// succeeds or `timeout` elapses.
///
/// On success the returned [`LockGuard`] holds the lock until it is
/// dropped. The OS releases the lock when the handle closes, including on
/// abnormal process termination.
///
/// # Errors
///
/// Returns [`LockError::Timeout`] when a conflicting holder retained the
/// lock for the whole `timeout` window, and [`LockError::Io`] when the
/// lock file cannot be opened or the lock syscall fails for a
/// non-contention reason.
pub fn acquire(path: &Utf8Path, kind: LockKind, timeout: Duration) -> Result<LockGuard, LockError> {
    let file = open_lock_file(path)?;

    let deadline = Instant::now() + timeout;
    loop {
        match try_lock(&file, kind) {
            Ok(()) => {
                return Ok(LockGuard {
                    _file: file,
                    path: path.to_owned(),
                    kind,
                });
            }
            Err(e) if is_contended(&e) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(LockError::Timeout {
                        kind,
                        path: path.to_owned(),
                        waited: timeout,
                    });
                }
                // Don't overshoot the deadline on the final wait.
                let remaining = deadline.saturating_duration_since(now);
                std::thread::sleep(POLL_INTERVAL.min(remaining));
            }
            Err(source) => {
                return Err(LockError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
}

/// Acquire the advisory lock at `path` in the requested `kind` with a
/// single non-blocking attempt, never waiting for a conflicting holder.
///
/// This is the zero-wait counterpart to [`acquire`]: it makes exactly one
/// `try_lock_*` attempt and, if the lock is already held, returns
/// [`LockError::Contended`] immediately rather than polling. It exists for
/// the apply path's `NonBlocking` policy (the watcher), which
/// must skip on contention instead of blocking a background reapply.
///
/// On success the returned [`LockGuard`] holds the lock until it is
/// dropped, exactly as for [`acquire`].
///
/// # Errors
///
/// Returns [`LockError::Contended`] when another holder currently holds a
/// conflicting lock, and [`LockError::Io`] when the lock file cannot be
/// opened or the lock syscall fails for a non-contention reason.
pub fn try_acquire(path: &Utf8Path, kind: LockKind) -> Result<LockGuard, LockError> {
    let file = open_lock_file(path)?;
    match try_lock(&file, kind) {
        Ok(()) => Ok(LockGuard {
            _file: file,
            path: path.to_owned(),
            kind,
        }),
        Err(e) if is_contended(&e) => Err(LockError::Contended {
            kind,
            path: path.to_owned(),
        }),
        Err(source) => Err(LockError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Open (creating if absent) the advisory-lock file at `path` for
/// acquisition. The byte contents are irrelevant — only the advisory lock
/// on the returned handle matters.
fn open_lock_file(path: &Utf8Path) -> Result<std::fs::File, LockError> {
    Ok(fs_err::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| LockError::Io {
            path: path.to_owned(),
            source,
        })?
        .into())
}

/// Issue one non-blocking advisory-lock attempt in the requested mode.
fn try_lock(file: &std::fs::File, kind: LockKind) -> Result<(), std::io::Error> {
    match kind {
        LockKind::Exclusive => FileExt::try_lock_exclusive(file),
        LockKind::Shared => FileExt::try_lock_shared(file),
    }
}

/// Distinguish "another holder has the lock" (retry) from a genuine I/O
/// failure (give up). `fs2` reports contention via
/// [`fs2::lock_contended_error`], whose `kind`/`raw_os_error` we match
/// against the returned error.
fn is_contended(err: &std::io::Error) -> bool {
    let contended = fs2::lock_contended_error();
    err.kind() == contended.kind() && err.raw_os_error() == contended.raw_os_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn lock_path(dir: &TempDir) -> Utf8PathBuf {
        Utf8Path::from_path(dir.path())
            .expect("utf8 temp path")
            .join("lock")
    }

    #[test]
    fn exclusive_then_shared_blocks_until_drop_then_succeeds() {
        let dir = TempDir::new().expect("tempdir");
        let path = lock_path(&dir);

        let exclusive =
            acquire(&path, LockKind::Exclusive, Duration::from_secs(5)).expect("first exclusive");

        // A shared acquirer cannot get in while the exclusive lock is held.
        let blocked = acquire(&path, LockKind::Shared, Duration::from_millis(50));
        assert!(
            matches!(
                blocked,
                Err(LockError::Timeout {
                    kind: LockKind::Shared,
                    ..
                })
            ),
            "shared acquire should time out against a held exclusive lock, got {blocked:?}"
        );

        // After the exclusive guard drops, the shared lock is available.
        drop(exclusive);
        let shared =
            acquire(&path, LockKind::Shared, Duration::from_secs(5)).expect("shared after release");
        assert_eq!(shared.kind(), LockKind::Shared);
        assert_eq!(shared.path(), path);
    }

    #[test]
    fn two_shared_holders_coexist() {
        let dir = TempDir::new().expect("tempdir");
        let path = lock_path(&dir);

        let first = acquire(&path, LockKind::Shared, Duration::from_secs(5)).expect("first shared");
        let second = acquire(&path, LockKind::Shared, Duration::from_millis(200))
            .expect("second shared coexists");

        assert_eq!(first.kind(), LockKind::Shared);
        assert_eq!(second.kind(), LockKind::Shared);
    }

    #[test]
    fn shared_blocks_exclusive() {
        let dir = TempDir::new().expect("tempdir");
        let path = lock_path(&dir);

        let _shared =
            acquire(&path, LockKind::Shared, Duration::from_secs(5)).expect("shared holder");

        let blocked = acquire(&path, LockKind::Exclusive, Duration::from_millis(50));
        assert!(
            matches!(
                blocked,
                Err(LockError::Timeout {
                    kind: LockKind::Exclusive,
                    ..
                })
            ),
            "exclusive acquire should time out against a held shared lock, got {blocked:?}"
        );
    }

    #[test]
    fn timeout_error_names_the_path_and_waited_duration() {
        let dir = TempDir::new().expect("tempdir");
        let path = lock_path(&dir);
        let _held = acquire(&path, LockKind::Exclusive, Duration::from_secs(5)).expect("hold");

        let cap = Duration::from_millis(40);
        let err = acquire(&path, LockKind::Exclusive, cap).expect_err("must time out");
        assert!(
            matches!(
                &err,
                LockError::Timeout {
                    kind: LockKind::Exclusive,
                    path: errored,
                    waited,
                } if *errored == path && *waited == cap
            ),
            "expected an exclusive Timeout naming {path} with waited {cap:?}, got {err:?}"
        );
    }

    #[test]
    fn lock_file_is_created_on_first_acquire() {
        let dir = TempDir::new().expect("tempdir");
        let path = lock_path(&dir);
        assert!(!path.exists(), "lock file should not exist before acquire");

        let _guard =
            acquire(&path, LockKind::Exclusive, Duration::from_secs(5)).expect("create + lock");
        assert!(path.exists(), "lock file should be created by acquire");
    }
}
