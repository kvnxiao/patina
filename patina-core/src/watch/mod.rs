//! The filesystem watcher subsystem (SPEC-0003).
//!
//! The watcher reapplies on source changes and surfaces files modified
//! outside Patina. The subsystem is built from several pieces:
//!
//! - the drift-cache format — the watcher's notification ledger at
//!   `<state>/patina/drift.cache` — via the [`drift_cache`] submodule;
//! - the structured-log sink — the daily-rotating `<state>/patina/logs/` stack
//!   the watcher writes its metrics into (REQ-009) — via the [`logging`]
//!   submodule;
//! - the pure mapping from a committed journal record to the watcher's FS
//!   subscription set (REQ-005) — via the [`subscriptions`] submodule;
//! - the 500ms debounce wrapper and the OS-thread→async bridge (REQ-006 /
//!   DEC-011) — via the [`debounce`] submodule;
//! - the foreground watcher loop itself ([`run_foreground`], REQ-004 /
//!   REQ-006).
//!
//! The re-apply handler body (T-009), the drift handler (T-010), and the
//! per-OS service install land in later tasks; the foreground loop here logs
//! receipt of each debounced batch and leaves the mutating work to those tasks.

pub mod debounce;
pub mod drift_cache;
pub mod logging;
pub mod subscriptions;

use crate::journal::read_latest_commit;
use crate::state_dir;
use camino::Utf8Path;
use std::future::Future;
use thiserror::Error;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::SubscriberExt;

/// The TOML table the watcher would read a debounce override from if the knob
/// existed. In v1.0 it does not: a `debounce_ms` key here is rejected with a
/// typed warning (REQ-006 / DEC-002).
const WATCHER_TABLE: &str = "watcher";

/// The rejected (forward-compatible) debounce-override key (REQ-006 / DEC-002).
const DEBOUNCE_MS_KEY: &str = "debounce_ms";

/// Errors returned by the foreground watcher.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WatchError {
    /// Resolving the per-machine state directory failed.
    #[error("failed to resolve the per-machine state directory: {source}")]
    StateDir {
        /// The underlying state-directory resolution error.
        #[source]
        source: state_dir::StateDirError,
    },

    /// Building the watcher's structured-log file appender failed.
    #[error(transparent)]
    Logging(#[from] logging::LoggingError),

    /// Reading the most recent committed journal record failed.
    #[error("failed to read the latest committed apply: {source}")]
    Journal {
        /// The underlying journal-read error.
        #[source]
        source: crate::journal::JournalError,
    },

    /// Building or arming the debouncer failed.
    #[error(transparent)]
    Debounce(#[from] debounce::DebounceError),
}

/// Inspect a root `patina.toml` for the forward-compatible-but-rejected
/// `[watcher] debounce_ms` key and return a typed warning when it is present
/// (REQ-006 / DEC-002).
///
/// The 500ms debounce is hardcoded in v1.0 ([`debounce::DEBOUNCE`]); a
/// `debounce_ms` override is parsed, warned about, and otherwise ignored, so a
/// repository that sets it keeps working and a future version can add the knob
/// without breaking older repositories. A malformed manifest, a missing
/// `[watcher]` table, or a `[watcher]` table without `debounce_ms` all yield
/// `None` — this helper diagnoses only the one forward-compatible key and
/// leaves real parse errors to the apply path.
///
/// # Examples
///
/// ```
/// use patina_core::watch::watcher_config_warning;
///
/// assert!(watcher_config_warning("[patina]\nroot = true\n").is_none());
/// assert!(watcher_config_warning("[watcher]\ndebounce_ms = 250\n").is_some());
/// ```
#[must_use = "the warning must be surfaced to the user or it is silently dropped"]
pub fn watcher_config_warning(manifest_text: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(manifest_text).ok()?;
    let table = value.get(WATCHER_TABLE)?.as_table()?;
    if table.contains_key(DEBOUNCE_MS_KEY) {
        Some(format!(
            "ignoring `[{WATCHER_TABLE}] {DEBOUNCE_MS_KEY}` in patina.toml: the \
             watcher debounce window is fixed at {}ms in this version and is not \
             configurable",
            debounce::DEBOUNCE.as_millis()
        ))
    } else {
        None
    }
}

/// Run the foreground watcher loop inline until `shutdown` resolves
/// (REQ-004 / REQ-006 / DEC-011).
///
/// The watcher: resolves the per-machine state directory, initializes the
/// structured-log stack (a rotating file layer plus a stderr layer), reads the
/// most recent committed apply and computes its FS subscription set
/// ([`subscriptions::compute_subscriptions`]), arms the 500ms debouncer over
/// that set ([`debounce::spawn`]), and runs a single `tokio::select!` loop that
/// awaits either the next debounced event batch or `shutdown`. On `shutdown` it
/// logs a `shutdown` event, drops the debouncer (releasing every FS
/// subscription), and returns `Ok(())`.
///
/// This task wires the loop and shutdown only. Each received batch is logged
/// (`watch_event`) but not yet acted on: the re-apply handler lands in T-009
/// and the drift handler in T-010. The foreground process does **not** acquire
/// the exclusive advisory lock (REQ-004); the watcher takes per-re-apply locks
/// internally in T-009.
///
/// When no apply has ever committed, the subscription set is just the
/// journal-rescan directory, so the watcher idles until an apply writes a
/// journal there.
///
/// # Arguments
///
/// * `shutdown` - a future that resolves when the watcher should stop. The CLI
///   passes a `ctrl_c` + SIGTERM signal future (DEC-011); tests pass a
///   controllable future.
///
/// # Errors
///
/// Returns [`WatchError::StateDir`] when the state directory cannot be
/// resolved, [`WatchError::Logging`] when the log appender cannot be built,
/// [`WatchError::Journal`] when the latest commit cannot be read, and
/// [`WatchError::Debounce`] when the debouncer cannot be built or armed.
pub async fn run_foreground<F>(shutdown: F) -> Result<(), WatchError>
where
    F: Future<Output = ()>,
{
    let state = state_dir::resolve().map_err(|source| WatchError::StateDir { source })?;
    run_foreground_in(&state, shutdown).await
}

/// The testable core of [`run_foreground`]: run the loop against an explicit
/// state directory rather than the process-resolved one.
///
/// Split out so tests can drive the loop against an isolated tempdir state tree
/// without touching the developer's real state directory or relying on a global
/// `tracing` subscriber being installable more than once per process.
async fn run_foreground_in<F>(state: &Utf8Path, shutdown: F) -> Result<(), WatchError>
where
    F: Future<Output = ()>,
{
    let appender = logging::build_file_appender(state)?;
    // A structured file layer over the rotating log appender plus a
    // human-readable stderr layer (REQ-004: the foreground watcher logs to
    // stderr at info level). Both disable ANSI coloring so the stderr output is
    // deterministic and substring-matchable by the test harness, and the whole
    // subscriber is gated by the `RUST_LOG` env filter (default `info`).
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(appender.writer);
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(std::io::stderr);
    let subscriber = tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .with(env_filter());

    let journal_dir = state.join("journal");

    // Subscriber-routing decision (resolves the T-008 round-1 correctness vs
    // architecture tension; binding on T-009/T-010):
    //
    // The watcher's subscriber is attached to the run future via
    // `WithSubscriber::with_subscriber`, NOT installed thread-locally with
    // `tracing::subscriber::set_default`. `WithSubscriber` re-enters the
    // dispatcher on *every* poll of the wrapped future, so every event the run
    // emits — `watch_subscriptions` (from `compute_subscriptions`),
    // `watch_started`, `watch_event`, `shutdown`, and the future `re_apply` /
    // `drift` events — routes through our layers regardless of which runtime
    // thread polls the future. The whole run body (journal read, subscription
    // compute, debouncer arm, select-loop) lives inside the future so its
    // events all route through the watcher's layers, not just the loop's.
    //
    // A thread-local `set_default` guard would be subtly fragile here. Today
    // the run is awaited inline in the `#[tokio::main]` (`rt-multi-thread`)
    // `block_on` root future, which is driven on the calling thread and is
    // never work-stolen (only `tokio::spawn`ed tasks migrate across workers),
    // so a thread-local default would *happen* to stay valid. But T-009/T-010
    // add `tokio::spawn`ed re-apply / drift handlers; a thread-local default
    // does not propagate into spawned tasks at all, so their post-await
    // emissions would silently fall through to the global no-op subscriber and
    // be dropped. `with_subscriber` removes that hazard structurally: spawned
    // children can carry the same dispatcher forward with
    // `.with_current_subscriber()`. There is no global install, so a second
    // watcher run in the same test process never double-installs, preserving
    // the per-run tempdir isolation tests rely on.
    async {
        let record =
            read_latest_commit(&journal_dir).map_err(|source| WatchError::Journal { source })?;
        let subscriptions = record.as_ref().map_or_else(
            || vec![journal_dir.clone()],
            |record| subscriptions::compute_subscriptions(record, state),
        );

        let mut debouncer = debounce::spawn(&subscriptions)?;
        tracing::info!(
            target: "patina_core",
            subscriptions = subscriptions.len(),
            "watch_started"
        );

        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => {
                    tracing::info!(target: "patina_core", "shutdown");
                    break;
                }
                batch = debouncer.events.recv() => {
                    match batch {
                        Some(batch) => {
                            // T-008 wires the loop; the re-apply (T-009) and
                            // drift (T-010) handlers land later. Log receipt so
                            // the loop is observable and the later tasks have a
                            // dispatch point. When those tasks spawn handlers,
                            // they must carry this subscriber forward (e.g.
                            // `handler.with_current_subscriber()`) so their
                            // post-await emissions are not dropped.
                            tracing::info!(
                                target: "patina_core",
                                paths = batch.paths.len(),
                                "watch_event"
                            );
                        }
                        None => break,
                    }
                }
            }
        }

        // Dropping the debouncer releases every FS subscription (REQ-004).
        drop(debouncer);
        Ok(())
    }
    .with_subscriber(subscriber)
    .await
}

/// The `RUST_LOG` env filter, defaulting to `info` when unset, so the
/// foreground watcher logs at info level by default (REQ-004) and a harness can
/// scope it with `RUST_LOG=patina_core=info`.
fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn plain_root_manifest_yields_no_watcher_warning() {
        assert!(watcher_config_warning("[patina]\nroot = true\n").is_none());
    }

    #[test]
    fn watcher_table_without_debounce_ms_yields_no_warning() {
        // A `[watcher]` table that does not set the rejected key is not warned
        // about — only `debounce_ms` is diagnosed.
        assert!(watcher_config_warning("[watcher]\nother = 1\n").is_none());
    }

    #[test]
    fn debounce_ms_key_yields_a_warning_naming_the_fixed_window() {
        let warning = watcher_config_warning("[watcher]\ndebounce_ms = 250\n")
            .expect("debounce_ms must produce a warning");
        assert!(
            warning.contains(DEBOUNCE_MS_KEY),
            "warning should name the rejected key, got: {warning}"
        );
        assert!(
            warning.contains(&debounce::DEBOUNCE.as_millis().to_string()),
            "warning should name the fixed {}ms window, got: {warning}",
            debounce::DEBOUNCE.as_millis()
        );
    }

    #[test]
    fn malformed_manifest_yields_no_warning() {
        // A genuinely malformed manifest is not this helper's concern; it
        // diagnoses only the one forward-compatible key and defers real parse
        // errors to the apply path.
        assert!(watcher_config_warning("this is not = = toml").is_none());
    }

    #[tokio::test]
    async fn run_foreground_in_returns_ok_when_shutdown_fires_with_no_commit() {
        // No apply has committed, so the subscription set is just the journal
        // dir. An immediately-ready shutdown future drives the loop straight to
        // the shutdown arm and a clean Ok return (REQ-004).
        let tmp = tempfile::tempdir().expect("tempdir");
        let state =
            Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("temp path is utf-8");
        fs_err::create_dir_all(state.join("journal").as_std_path()).expect("mkdir journal");

        run_foreground_in(&state, async {})
            .await
            .expect("foreground watcher exits Ok on shutdown");
    }

    #[tokio::test]
    async fn run_foreground_in_runs_the_loop_until_shutdown() {
        // Drive the loop for a beat before signalling shutdown, exercising the
        // select-loop (not just the immediate-shutdown shortcut). A oneshot is
        // the shutdown future; firing it resolves the future and ends the loop.
        let tmp = tempfile::tempdir().expect("tempdir");
        let state =
            Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("temp path is utf-8");
        fs_err::create_dir_all(state.join("journal").as_std_path()).expect("mkdir journal");

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            run_foreground_in(&state, async {
                // Resolve when the oneshot fires (or its sender drops); either
                // way the shutdown future completes and ends the loop.
                let _received = rx.await;
            })
            .await
        });

        // Let the loop arm its debouncer and reach the select, then shut down.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tx.send(()).expect("send shutdown");

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("the watcher task joins within the timeout")
            .expect("the watcher task does not panic");
        result.expect("foreground watcher exits Ok after a running loop");
    }
}
