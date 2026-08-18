//! The watcher's 500ms debounce wrapper.
//!
//! A typical editor save produces a burst of 3-5 filesystem events (write to a
//! tempfile, rename into place, metadata touch, stat). Re-applying once per raw
//! event would be wasteful and racy. The watcher coalesces a burst arriving
//! within a fixed window into a single re-apply trigger. The window is the
//! hardcoded [`DEBOUNCE`] constant (no configuration knob in v1.0; a
//! `[watcher] debounce_ms` key in the root manifest is rejected with a typed
//! warning, see [`super::watcher_config_warning`]).
//!
//! `notify` / `notify-debouncer-full` deliver coalesced event batches on their
//! own OS-managed thread via a synchronous callback. The re-apply path lives on
//! the async runtime, so this module bridges the two. [`spawn`]
//! builds the debouncer with a callback that forwards each batch into a
//! [`tokio::sync::mpsc`] channel, and the foreground watcher's `tokio::select!`
//! loop awaits the receiver. The returned [`Debouncer`] owns the live
//! subscriptions; dropping it tears them down. The watcher holds it for its
//! process lifetime and drops it on shutdown.
//!
//! Wire the debounce and bridge. The select loop interprets each batch as a
//! source edit (re-apply), a content-target edit (drift check), or a
//! journal-directory event (rescan).

use camino::Utf8Path;
use camino::Utf8PathBuf;
use notify::RecursiveMode;
use notify_debouncer_full::DebounceEventResult;
use notify_debouncer_full::Debouncer as InnerDebouncer;
use notify_debouncer_full::RecommendedCache;
use notify_debouncer_full::new_debouncer;
use notify_debouncer_full::notify::RecommendedWatcher;
use std::time::Duration;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

/// The hardcoded debounce window. Burst FS events arriving
/// within this window coalesce into a single trigger. Not configurable in
/// v1.0: a `[watcher] debounce_ms` key in the root manifest produces a typed
/// warning and is otherwise ignored (forward-compatible).
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// One coalesced filesystem-event batch the debouncer delivered. Carries the
/// de-duplicated set of paths the batch touched, in first-occurrence order, so
/// the select-loop can classify them (source edit, content-target edit, or
/// journal-directory event) without re-deriving the path set.
///
/// Each path also carries the time of the most recent event that touched it,
/// which [`EventBatch::observed_since`] uses to drop writes a re-apply already
/// consumed. See that method for why a batch outlives the burst that made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventBatch {
    /// The distinct paths this batch touched, first-occurrence order.
    pub paths: Vec<Utf8PathBuf>,
    /// Latest event time per entry of `paths`, index-aligned. Private so the
    /// two vectors are only ever built and filtered together.
    observed: Vec<Instant>,
}

impl EventBatch {
    /// Build a batch from `paths` alone, stamping every entry with the current
    /// instant. For constructing a batch outside the debouncer callback.
    #[must_use]
    pub fn from_paths(paths: Vec<Utf8PathBuf>) -> Self {
        let observed = vec![Instant::now(); paths.len()];
        Self { paths, observed }
    }

    /// The batch with every path whose latest event predates `cutoff` removed.
    ///
    /// The watcher's select-loop awaits a re-apply inline, so nothing drains
    /// the `notify` stream while one runs. Events that arrived before the
    /// re-apply started are queued behind it and surface afterwards as a fresh
    /// batch, even though the re-apply already read that state and applied it.
    /// Left in, such a straggler re-classifies the batch as a source edit and
    /// drives a redundant second re-apply, which writes another journal
    /// record. Passing the re-apply's start instant here drops exactly those.
    ///
    /// A write landing *during* a re-apply is kept: the apply may have read
    /// the source before that write, so it still needs applying. The
    /// comparison is therefore `>=`, keeping an event stamped at the cutoff.
    #[must_use]
    pub fn observed_since(&self, cutoff: Instant) -> Self {
        let mut paths = Vec::new();
        let mut observed = Vec::new();
        for (path, at) in self.paths.iter().zip(&self.observed) {
            if *at >= cutoff {
                paths.push(path.clone());
                observed.push(*at);
            }
        }
        Self { paths, observed }
    }

    /// Whether the batch touches no paths.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
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
/// watch alive and dropping it releases every subscription. `events`
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
/// Each path is watched non-recursively ([`RecursiveMode::NonRecursive`]),
/// subscribing to exactly the journal-recorded paths and the journal
/// directory rather than the repository tree. The debouncer's
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
    // non-blocking and callable from any thread, so it bridges into the
    // async loop without blocking the OS thread.
    let mut debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
        if let Ok(events) = result {
            let mut paths: Vec<Utf8PathBuf> = Vec::new();
            let mut observed: Vec<Instant> = Vec::new();
            for event in events {
                for path in &event.event.paths {
                    if let Ok(utf8) = Utf8PathBuf::try_from(path.clone()) {
                        // A path written twice inside one window keeps the
                        // later stamp, so a write during a re-apply survives
                        // `observed_since` even when an earlier write to the
                        // same path does not.
                        if let Some(at) = paths
                            .iter()
                            .position(|seen| *seen == utf8)
                            .and_then(|index| observed.get_mut(index))
                        {
                            *at = (*at).max(event.time);
                        } else {
                            paths.push(utf8);
                            observed.push(event.time);
                        }
                    }
                }
            }
            if !paths.is_empty() && tx.send(EventBatch { paths, observed }).is_err() {
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
/// A subscription path may not yet exist on disk at watch time, for example a
/// content target a future apply will create. `notify` errors on a missing
/// path, so a path that cannot be registered is surfaced as
/// [`DebounceError::Watch`] rather than silently skipped. The caller, the
/// rescan, re-derives the set after each apply, once the recorded paths exist.
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
        // The coalescing scenario fires five touches within a 100ms burst and
        // requires them to coalesce into one trigger. That only holds if the
        // debounce window is comfortably wider than the burst spread, so the
        // window must be at least 100ms. The integration tests
        // (`watch_foreground_cli.rs`) wait on the loop with a 5s timeout, so a
        // window approaching that budget would miss the deadline. Cap the
        // window well under 5s. These bounds come from the scenario timings,
        // not from the constant's current value. Editing `DEBOUNCE` to 50ms
        // or to 5s would fail this test unless the test and the constant
        // changed together.
        let burst_spread = Duration::from_millis(100);
        let test_wait_budget = Duration::from_secs(5);
        assert!(
            DEBOUNCE >= burst_spread,
            "DEBOUNCE ({DEBOUNCE:?}) must be at least the {burst_spread:?} \
             burst spread so the five touches coalesce"
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

        // `notify` may report a canonicalized path (on macOS `/tmp` resolves
        // to `/private/tmp`), so assert on the touched file's name rather
        // than the tempdir prefix. A coalesced batch naming `touched` proves
        // the write under the watched directory was debounced and bridged
        // into the async channel.
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

    /// A batch whose paths carry the given stamps, index-aligned.
    fn stamped(entries: &[(&str, Instant)]) -> EventBatch {
        EventBatch {
            paths: entries.iter().map(|(p, _)| Utf8PathBuf::from(*p)).collect(),
            observed: entries.iter().map(|(_, at)| *at).collect(),
        }
    }

    #[test]
    fn observed_since_drops_writes_the_reapply_already_consumed() {
        // The flake this guards: five writes land, a re-apply starts and runs
        // long enough that their queued events surface afterwards alongside
        // the target the re-apply itself rewrote. Keeping the stale source
        // path re-classifies the batch as a source edit and drives a second,
        // redundant re-apply.
        let started = Instant::now();
        let before = started
            .checked_sub(Duration::from_millis(50))
            .expect("the test clock is well past 50ms");
        let during = started
            .checked_add(Duration::from_millis(50))
            .expect("no Instant overflow");

        let kept = stamped(&[
            ("/repo/git/gitconfig", before),
            ("/home/.gitconfig", during),
        ])
        .observed_since(started);

        assert_eq!(
            kept.paths,
            vec![Utf8PathBuf::from("/home/.gitconfig")],
            "the pre-apply source write is consumed; only the target rewrite survives"
        );
    }

    #[test]
    fn observed_since_keeps_a_write_landing_during_the_reapply() {
        // The apply may have read the source before this write, so it still
        // needs applying. An event stamped exactly at the cutoff is kept.
        let started = Instant::now();
        let kept = stamped(&[
            ("/repo/a", started),
            (
                "/repo/b",
                started
                    .checked_add(Duration::from_millis(1))
                    .expect("no Instant overflow"),
            ),
        ])
        .observed_since(started);

        assert_eq!(kept.paths.len(), 2, "neither write predates the re-apply");
    }

    #[test]
    fn observed_since_empties_a_wholly_stale_batch() {
        let started = Instant::now();
        let stale = started
            .checked_sub(Duration::from_millis(1))
            .expect("the test clock is well past 1ms");

        assert!(
            stamped(&[("/repo/a", stale)])
                .observed_since(started)
                .is_empty()
        );
    }

    #[test]
    fn a_repeat_write_keeps_the_later_stamp() {
        // The debouncer callback folds a repeat write into the existing entry
        // by taking the later stamp. A path written both before and during a
        // re-apply must therefore survive the filter.
        let started = Instant::now();
        let before = started
            .checked_sub(Duration::from_millis(50))
            .expect("the test clock is well past 50ms");
        let during = started
            .checked_add(Duration::from_millis(50))
            .expect("no Instant overflow");

        let folded = stamped(&[("/repo/a", before.max(during))]).observed_since(started);

        assert_eq!(
            folded.paths.len(),
            1,
            "the later write outranks the earlier"
        );
    }
}
