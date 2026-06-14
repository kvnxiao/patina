//! The filesystem watcher subsystem.
//!
//! The watcher reapplies on source changes and surfaces files modified
//! outside Patina. The subsystem is built from several pieces:
//!
//! - the drift-cache format — the watcher's notification ledger at
//!   `<state>/patina/drift.cache` — via the [`drift_cache`] submodule;
//! - the structured-log sink — the daily-rotating `<state>/patina/logs/` stack
//!   the watcher writes its metrics into — via the [`logging`] submodule;
//! - the pure mapping from a committed journal record to the watcher's FS
//!   subscription set — via the [`subscriptions`] submodule;
//! - the 500ms debounce wrapper and the OS-thread→async bridge — via the
//!   [`debounce`] submodule;
//! - the `NonBlocking` re-apply handler driven on a source edit — via the
//!   [`reapply`] submodule;
//! - the drift-detection handler driven on a content-target edit — via the
//!   [`drift`] submodule;
//! - the foreground watcher loop itself ([`run_foreground`]), which classifies
//!   each debounced batch and dispatches it;
//! - the per-OS background-service abstraction and lifecycle backends — via the
//!   [`service`] submodule.
//!
//! The foreground watcher is the end-to-end engine the per-OS service
//! ([`service`]) supervises.

pub mod debounce;
pub mod drift;
pub mod drift_cache;
pub mod logging;
pub mod reapply;
pub mod service;
pub mod subscriptions;

use crate::journal::COMMIT_SUFFIX;
use crate::journal::read_latest_commit;
use crate::state_dir;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::future::Future;
use thiserror::Error;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::SubscriberExt;

/// The TOML table the watcher would read a debounce override from if the knob
/// existed. In v1.0 it does not: a `debounce_ms` key here is rejected with a
/// typed warning.
const WATCHER_TABLE: &str = "watcher";

/// The rejected (forward-compatible) debounce-override key.
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
/// `[watcher] debounce_ms` key and return a typed warning when it is present.
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

/// Run the foreground watcher loop inline until `shutdown` resolves.
///
/// The watcher: resolves the per-machine state directory, initializes the
/// structured-log stack (a rotating file layer plus a stderr layer), reads the
/// most recent committed apply and computes its FS subscription set
/// ([`subscriptions::compute_watch_set`]), arms the 500ms debouncer over
/// that set ([`debounce::spawn`]), and runs a single `tokio::select!` loop that
/// awaits either the next debounced event batch or `shutdown`. On `shutdown` it
/// logs a `shutdown` event, drops the debouncer (releasing every FS
/// subscription), and returns `Ok(())`.
///
/// Each received batch is classified and dispatched: a
/// journal-directory event re-reads the latest commit and re-arms the debouncer
/// over the recomputed subscription set ([`reapply`] is *not* invoked); a
/// source-path event drives a `NonBlocking` re-apply
/// ([`reapply::run_reapply`]); a content-target-only event is left for the
/// drift handler. The foreground process does **not** acquire the
/// exclusive advisory lock; the engine takes the per-re-apply lock
/// under `NonBlocking` inside [`reapply::run_reapply`].
///
/// When no apply has ever committed, the subscription set is just the
/// journal-rescan directory, so the watcher idles until an apply writes a
/// journal there.
///
/// # Arguments
///
/// * `shutdown` - a future that resolves when the watcher should stop. The CLI
///   passes a `ctrl_c` + SIGTERM signal future; tests pass a controllable
///   future.
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
    // human-readable stderr layer (the foreground watcher logs to
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

    // Subscriber-routing decision:
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
    // so a thread-local default would *happen* to stay valid. But the
    // `tokio::spawn`ed re-apply / drift handlers add spawned tasks; a
    // thread-local default does not propagate into spawned tasks at all, so
    // their post-await emissions would silently fall through to the global
    // no-op subscriber and
    // be dropped. `with_subscriber` removes that hazard structurally: spawned
    // children can carry the same dispatcher forward with
    // `.with_current_subscriber()`. There is no global install, so a second
    // watcher run in the same test process never double-installs, preserving
    // the per-run tempdir isolation tests rely on.
    async {
        let record =
            read_latest_commit(&journal_dir).map_err(|source| WatchError::Journal { source })?;
        let mut watch_set = record.as_ref().map_or_else(
            || subscriptions::WatchSet {
                watched: vec![journal_dir.clone()],
                sources: Vec::new(),
                content_targets: Vec::new(),
            },
            |record| subscriptions::compute_watch_set(record, state),
        );
        // The journal timestamp the drift cache binds its expectations to: the
        // committed apply's `at` metadata, empty when no apply has committed
        // yet (no content targets to drift against in that case either).
        let mut journal_ts = record
            .as_ref()
            .map_or_else(String::new, |record| record.last_apply.at.clone());

        let mut debouncer = debounce::spawn(&watch_set.watched)?;
        tracing::info!(
            target: "patina_core",
            subscriptions = watch_set.watched.len(),
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
                            tracing::info!(
                                target: "patina_core",
                                paths = batch.paths.len(),
                                "watch_event"
                            );
                            // Classify the coalesced batch, journal
                            // first so the watcher's own writes never re-trigger
                            // a re-apply:
                            //
                            // - A batch touching `<state>/patina/journal/` is a
                            //   new `.plan`/`.COMMIT` from some apply (the
                            //   watcher's own re-apply, or a parallel CLI run):
                            //   re-read the latest commit and recompute the watch
                            //   set, re-arming the debouncer (rescan). It
                            //   does NOT re-apply, so a re-apply's own journal
                            //   write cannot drive an unbounded loop.
                            // - Otherwise, a batch touching a repository **source**
                            //   path is a source edit: drive a `NonBlocking`
                            //   re-apply.
                            // - Otherwise the batch touched only content-target
                            //   paths — a re-apply's own target rewrite or an
                            //   external edit. That is drift detection's concern,
                            //   NOT a re-apply: a content-target event
                            //   re-hashes the live bytes and notifies on
                            //   divergence, and must not re-apply (re-applying
                            //   would rewrite the target and re-trigger itself).
                            if journal_event(&batch) {
                                if let Some((rebuilt, rebuilt_set, rebuilt_ts)) =
                                    rescan(&journal_dir, state, &batch)
                                {
                                    debouncer = rebuilt;
                                    watch_set = rebuilt_set;
                                    journal_ts = rebuilt_ts;
                                }
                            } else if source_event(&batch, &watch_set.sources) {
                                let _outcome = reapply::run_reapply().await;
                            } else {
                                // A content-target-only batch: re-hash the
                                // touched targets and notify on drift.
                                let now_unix = jiff::Timestamp::now().as_second();
                                let _outcomes = drift::handle_target_events(
                                    &watch_set.content_targets,
                                    &batch.paths,
                                    state,
                                    &journal_ts,
                                    now_unix,
                                    &drift::NotifySink,
                                );
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        // Dropping the debouncer releases every FS subscription.
        drop(debouncer);
        Ok(())
    }
    .with_subscriber(subscriber)
    .await
}

/// The `RUST_LOG` env filter, defaulting to `info` when unset, so the
/// foreground watcher logs at info level by default and a harness can
/// scope it with `RUST_LOG=patina_core=info`.
fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

/// Whether a debounced batch is a journal-directory event — a new
/// `.plan`/`.COMMIT` written under `<state>/patina/journal/` by some apply
/// (the watcher's own re-apply, or a parallel CLI invocation). Such a batch
/// triggers a subscription rescan rather than a re-apply, so the
/// watcher's own journal writes cannot drive an unbounded re-apply loop.
///
/// A batch counts as a journal event when any touched path is a journal
/// sentinel (a file with a `.plan` / `.COMMIT` / `.progress` /
/// `.ROLLED_BACK` suffix) whose parent directory is named `journal`. The
/// classifier matches on the path *leaf* rather than a `starts_with` against
/// the resolved journal directory because `notify` reports canonicalized event
/// paths (on macOS `/tmp` resolves to `/private/tmp`), which would never share
/// a prefix with the un-canonicalized state directory the watcher resolved.
fn journal_event(batch: &debounce::EventBatch) -> bool {
    batch.paths.iter().any(|p| is_journal_sentinel(p))
}

/// Whether `path` is a journal sentinel: a `.plan` / `.COMMIT` / `.progress` /
/// `.ROLLED_BACK` file directly inside a directory named `journal`.
fn is_journal_sentinel(path: &Utf8Path) -> bool {
    let in_journal_dir = path
        .parent()
        .and_then(Utf8Path::file_name)
        .is_some_and(|name| name == "journal");
    let is_sentinel = path.file_name().is_some_and(|name| {
        name.ends_with(crate::journal::PLAN_SUFFIX)
            || name.ends_with(crate::journal::COMMIT_SUFFIX)
            || name.ends_with(crate::journal::PROGRESS_SUFFIX)
            || name.ends_with(crate::journal::ROLLED_BACK_SUFFIX)
    });
    in_journal_dir && is_sentinel
}

/// Whether a debounced batch touched a repository **source** path — the only
/// kind of event that drives a re-apply. Content-target events route
/// to drift detection, not re-apply, so a re-apply's own target rewrite
/// does not re-trigger itself.
fn source_event(batch: &debounce::EventBatch, sources: &[Utf8PathBuf]) -> bool {
    batch.paths.iter().any(|path| sources.contains(path))
}

/// Re-read the latest committed apply and recompute the watcher's subscription
/// set, re-arming a fresh debouncer over it.
///
/// Logs a `journal_rescan` event naming the `.COMMIT` file(s) the triggering
/// batch touched and the recomputed subscription count. On success it returns
/// the rebuilt [`debounce::Debouncer`] paired with the recomputed
/// [`subscriptions::WatchSet`] and the new journal timestamp (the committed
/// apply's `at`, empty when none committed) for the caller to install. On a
/// journal read failure or a debouncer rebuild failure it logs a warning and
/// returns `None`, leaving the existing debouncer, watch set, and timestamp in
/// place — a transient rescan failure must not crash the watcher.
fn rescan(
    journal_dir: &Utf8Path,
    state: &Utf8Path,
    batch: &debounce::EventBatch,
) -> Option<(debounce::Debouncer, subscriptions::WatchSet, String)> {
    // Name the COMMIT file(s) the batch touched so a log reader can join the
    // rescan to the apply that triggered it. The `.COMMIT` suffix is
    // the journal's commit sentinel; a `.plan` arrives first but the COMMIT is
    // the durable record the rescan reads.
    let commits: Vec<&str> = batch
        .paths
        .iter()
        .filter(|path| path.file_name().is_some_and(|n| n.ends_with(COMMIT_SUFFIX)))
        .filter_map(|path| path.file_name())
        .collect();

    let record = match read_latest_commit(journal_dir) {
        Ok(record) => record,
        Err(error) => {
            tracing::warn!(
                target: "patina_core",
                error = %error,
                "journal_rescan_failed"
            );
            return None;
        }
    };
    let watch_set = record.as_ref().map_or_else(
        || subscriptions::WatchSet {
            watched: vec![journal_dir.to_path_buf()],
            sources: Vec::new(),
            content_targets: Vec::new(),
        },
        |record| subscriptions::compute_watch_set(record, state),
    );
    let journal_ts = record
        .as_ref()
        .map_or_else(String::new, |record| record.last_apply.at.clone());

    let rebuilt = match debounce::spawn(&watch_set.watched) {
        Ok(debouncer) => debouncer,
        Err(error) => {
            tracing::warn!(
                target: "patina_core",
                error = %error,
                "journal_rescan_failed"
            );
            return None;
        }
    };

    tracing::info!(
        target: "patina_core",
        commits = %commits.join("\t"),
        subscriptions = watch_set.watched.len(),
        "journal_rescan"
    );
    Some((rebuilt, watch_set, journal_ts))
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

    fn batch(paths: &[&str]) -> debounce::EventBatch {
        debounce::EventBatch {
            paths: paths.iter().map(Utf8PathBuf::from).collect(),
        }
    }

    #[test]
    fn journal_sentinels_in_a_journal_dir_classify_as_journal_events() {
        // A `.plan` / `.COMMIT` / `.progress` / `.ROLLED_BACK` file inside a
        // `journal` directory is a journal event regardless of the absolute
        // prefix (the macOS `/tmp` vs `/private/tmp` canonicalization is why we
        // match the leaf, not a `starts_with`).
        assert!(journal_event(&batch(&[
            "/private/tmp/x/patina/journal/20260531T000000Z.COMMIT"
        ])));
        assert!(journal_event(&batch(&[
            "/state/patina/journal/20260531T000000Z.plan"
        ])));
        assert!(journal_event(&batch(&[
            "/state/patina/journal/20260531T000000Z.progress"
        ])));
    }

    #[test]
    fn non_journal_paths_do_not_classify_as_journal_events() {
        // A source edit and a content-target rewrite are not journal events,
        // so they never route to the rescan path. A `.plan`-suffixed file that
        // is NOT inside a `journal` directory is also not a journal event (the
        // parent-dir-name guard rejects it).
        assert!(!journal_event(&batch(&["/repo/git/gitconfig"])));
        assert!(!journal_event(&batch(&["/home/u/.gitconfig"])));
        assert!(!journal_event(&batch(&["/repo/weird/notes.plan"])));
    }

    #[test]
    fn only_source_paths_classify_as_source_events() {
        // The re-apply trigger fires only for a repository source path. A
        // content-target path is watched (for drift) but is NOT a source, so a
        // batch naming only the target does not re-apply — this is the loop
        // guard that stops a re-apply's own target rewrite from re-triggering.
        let sources = vec![Utf8PathBuf::from("/repo/git/gitconfig")];
        assert!(source_event(&batch(&["/repo/git/gitconfig"]), &sources));
        assert!(!source_event(&batch(&["/home/u/.gitconfig"]), &sources));
        // A batch coalescing a source edit with the target rewrite still counts
        // as a source event (the source path is present).
        assert!(source_event(
            &batch(&["/home/u/.gitconfig", "/repo/git/gitconfig"]),
            &sources
        ));
    }

    #[tokio::test]
    async fn run_foreground_in_returns_ok_when_shutdown_fires_with_no_commit() {
        // No apply has committed, so the subscription set is just the journal
        // dir. An immediately-ready shutdown future drives the loop straight to
        // the shutdown arm and a clean Ok return.
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
