//! The watcher's 500ms debounce wrapper (SPEC-0003 REQ-006 / DEC-002 /
//! DEC-011).
//!
//! A typical editor save produces a burst of 3-5 filesystem events (write to a
//! tempfile, rename into place, metadata touch, stat). Re-applying once per raw
//! event would be wasteful and racy, so the watcher coalesces a burst arriving
//! within a fixed window into a single re-apply trigger. The window is the
//! hardcoded [`DEBOUNCE`] constant (REQ-006: no configuration knob in v1.0; a
//! `[watcher] debounce_ms` key in the root manifest is rejected with a typed
//! warning, see [`super::watcher_config_warning`]).
//!
//! `notify` / `notify-debouncer-full` deliver coalesced event batches on their
//! own OS-managed thread via a synchronous callback. The re-apply path lives on
//! the async runtime (DEC-011), so this module bridges the two: [`spawn`]
//! builds the debouncer with a callback that forwards each batch into a
//! [`tokio::sync::mpsc`] channel, and the foreground watcher's `tokio::select!`
//! loop awaits the receiver. The returned [`Debouncer`] owns the live
//! subscriptions; dropping it tears them down (the watcher holds it for its
//! process lifetime and drops it on shutdown, satisfying REQ-004's
//! release-subscriptions-on-exit contract).
//!
//! This module wires the debounce and the bridge only. Interpreting a batch —
//! deciding whether it is a source edit (re-apply, T-009), a content-target
//! edit (drift check, T-010), or a journal-directory event (rescan, T-009) —
//! is the select-loop's job, not this module's.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use notify::RecursiveMode;
use notify_debouncer_full::DebounceEventResult;
use notify_debouncer_full::Debouncer as InnerDebouncer;
use notify_debouncer_full::RecommendedCache;
use notify_debouncer_full::new_debouncer;
use notify_debouncer_full::notify::RecommendedWatcher;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

/// The hardcoded debounce window (REQ-006 / DEC-002). Burst FS events arriving
/// within this window coalesce into a single trigger. Not configurable in
/// v1.0: a `[watcher] debounce_ms` key in the root manifest produces a typed
/// warning and is otherwise ignored (forward-compatible).
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// One coalesced filesystem-event batch the debouncer delivered. Carries the
/// de-duplicated set of paths the batch touched, in first-occurrence order, so
/// the select-loop can classify them (source edit, content-target edit, or
/// journal-directory event) without re-deriving the path set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventBatch {
    /// The distinct paths this batch touched, first-occurrence order.
    pub paths: Vec<Utf8PathBuf>,
}

/// Errors returned when building or arming the watcher's debouncer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DebounceError {
    /// Constructing the underlying `notify` watcher/debouncer failed.
    #[error("failed to initialize the filesystem watcher: {source}")]
    Build {
        /// The underlying `notify` error.
        #[source]
        source: notify::Error,
    },

    /// Registering a subscription path with the watcher failed.
    #[error("failed to watch path `{path}`: {source}")]
    Watch {
        /// The path the watcher failed to register.
        path: Utf8PathBuf,
        /// The underlying `notify` error.
        #[source]
        source: notify::Error,
    },
}

/// A live, armed debouncer plus the async receiver its coalesced batches arrive
/// on.
///
/// `debouncer` owns the OS-level filesystem subscriptions; holding it keeps the
/// watch alive and dropping it releases every subscription (REQ-004). `events`
/// yields one [`EventBatch`] per coalesced burst; the foreground watcher's
/// `tokio::select!` loop awaits it.
#[must_use = "the Debouncer owns the live FS subscriptions; dropping it releases them"]
pub struct Debouncer {
    /// The live underlying debouncer. Held to keep subscriptions armed; never
    /// read directly after construction.
    _debouncer: InnerDebouncer<RecommendedWatcher, RecommendedCache>,
    /// Receiver of coalesced event batches, bridged from the `notify` OS
    /// thread.
    pub events: UnboundedReceiver<EventBatch>,
}

/// Build the 500ms debouncer, subscribe it to every path in `subscriptions`,
/// and bridge its coalesced batches into a [`tokio::sync::mpsc`] channel.
///
/// Each path is watched non-recursively ([`RecursiveMode::NonRecursive`]): the
/// watcher subscribes to exactly the journal-recorded paths and the journal
/// directory (REQ-005), never the repository tree recursively. The debouncer's
/// callback runs on `notify`'s own OS thread; it maps each coalesced batch to
/// an [`EventBatch`] and forwards it through the returned receiver, so the
/// async select-loop never blocks the OS thread.
///
/// # Arguments
///
/// * `subscriptions` - the path set from
///   [`compute_subscriptions`](super::subscriptions::compute_subscriptions).
///
/// # Errors
///
/// Returns [`DebounceError::Build`] when the underlying `notify` watcher cannot
/// be constructed and [`DebounceError::Watch`] when a subscription path cannot
/// be registered.
pub fn spawn(subscriptions: &[Utf8PathBuf]) -> Result<Debouncer, DebounceError> {
    let (tx, rx): (UnboundedSender<EventBatch>, UnboundedReceiver<EventBatch>) =
        tokio::sync::mpsc::unbounded_channel();

    // The callback runs on `notify`'s OS thread. `UnboundedSender::send` is
    // non-blocking and callable from any thread, so it is the natural bridge
    // into the async loop (DEC-011). A send error means the receiver was
    // dropped (the watcher is shutting down); there is nothing to forward to,
    // so drop the batch.
    let mut debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
        if let Ok(events) = result {
            let mut paths: Vec<Utf8PathBuf> = Vec::new();
            for event in events {
                for path in &event.event.paths {
                    if let Ok(utf8) = Utf8PathBuf::try_from(path.clone())
                        && !paths.contains(&utf8)
                    {
                        paths.push(utf8);
                    }
                }
            }
            if !paths.is_empty() && tx.send(EventBatch { paths }).is_err() {
                // The receiver was dropped (the watcher is shutting down);
                // there is nothing to forward to, so the batch is discarded.
            }
        }
    })
    .map_err(|source| DebounceError::Build { source })?;

    for path in subscriptions {
        watch_path(&mut debouncer, path)?;
    }

    Ok(Debouncer {
        _debouncer: debouncer,
        events: rx,
    })
}

/// Register one subscription path with the debouncer, non-recursively.
///
/// A subscription path may not yet exist on disk at watch time (e.g. a content
/// target a future apply will create); `notify` errors on a missing path, so a
/// path that cannot be registered is surfaced as [`DebounceError::Watch`]
/// rather than silently skipped — the caller (T-009's rescan) re-derives the
/// set after each apply, when the recorded paths do exist.
fn watch_path(
    debouncer: &mut InnerDebouncer<RecommendedWatcher, RecommendedCache>,
    path: &Utf8Path,
) -> Result<(), DebounceError> {
    debouncer
        .watch(path.as_std_path(), RecursiveMode::NonRecursive)
        .map_err(|source| DebounceError::Watch {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_window_brackets_the_coalescing_scenarios() {
        // CHK-010's scenario fires five touches within a 100ms burst and
        // requires them to coalesce into a single trigger; that only holds if
        // the debounce window is comfortably wider than the burst spread, so
        // the window must be at least the 100ms burst. The integration tests
        // (`watch_foreground_cli.rs`) wait on the loop with a 5s timeout, so a
        // window approaching that budget would make the watcher miss its
        // deadline; cap it well under that. These are independent bounds
        // derived from the scenario timings, not a re-spelling of the literal:
        // editing `DEBOUNCE` to 50ms or to 5s would fail this without the test
        // and the constant being changed in lockstep.
        let burst_spread = Duration::from_millis(100);
        let test_wait_budget = Duration::from_secs(5);
        assert!(
            DEBOUNCE >= burst_spread,
            "DEBOUNCE ({DEBOUNCE:?}) must be at least the {burst_spread:?} \
             burst spread so CHK-010's five touches coalesce"
        );
        assert!(
            DEBOUNCE < test_wait_budget,
            "DEBOUNCE ({DEBOUNCE:?}) must stay well under the {test_wait_budget:?} \
             integration-test wait budget so the watcher reacts in time"
        );
    }

    #[tokio::test]
    async fn spawn_watches_an_existing_dir_and_forwards_a_coalesced_batch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("temp path is utf-8");

        let mut debouncer =
            spawn(std::slice::from_ref(&dir)).expect("spawn debouncer over existing dir");

        // Write into the watched directory; the debouncer coalesces the burst
        // and delivers one batch within roughly the debounce window.
        let file = dir.join("touched");
        fs_err::write(file.as_std_path(), b"hello").expect("write watched file");

        let batch = tokio::time::timeout(Duration::from_secs(5), debouncer.events.recv())
            .await
            .expect("a batch arrives within the timeout")
            .expect("the channel is open");

        // `notify` may report a canonicalized path (on macOS `/tmp` resolves to
        // `/private/tmp`), so assert on the touched file's name rather than the
        // tempdir prefix: a coalesced batch naming `touched` proves the write
        // under the watched directory was debounced and bridged into the async
        // channel.
        assert!(
            batch.paths.iter().any(|p| p.file_name() == Some("touched")),
            "the coalesced batch should name the touched file, got {:?}",
            batch.paths
        );
    }

    #[test]
    fn watching_a_missing_path_is_a_typed_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf())
            .expect("temp path is utf-8")
            .join("does-not-exist");

        let err = spawn(std::slice::from_ref(&missing))
            .err()
            .expect("watching a missing path must error");
        // `Debouncer` is not `Debug`, so inspect the error via `.err()` rather
        // than `unwrap_err`. The error must be a `Watch` naming the bad path.
        assert!(
            matches!(&err, DebounceError::Watch { path, .. } if path == &missing),
            "expected a Watch error naming `{missing}`, got: {err:?}"
        );
    }
}
