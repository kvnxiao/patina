//! The watcher's drift-detection handler (SPEC-0003 REQ-007 / DEC-003 /
//! DEC-004 / DEC-008 / DEC-013).
//!
//! When a debounced batch touches a watched **content** target (a copy- or
//! template-mode file Patina owns), the foreground loop hands it here. For each
//! touched content target the handler reads the live bytes, computes their
//! `blake3` hash via [`content_hash`], and compares it to the journal-recorded
//! expectation ([`ContentTarget::expected_hash`]). On divergence it:
//!
//! - writes a `drift` warn event to the structured log (`drift_path`,
//!   `drift_expected_hash`, `drift_actual_hash`, in the underscore field-name
//!   convention the watcher's `re_apply_*` metric fields already use);
//! - upserts the `<state>/patina/drift.cache` atomically (the T-004
//!   [`write_drift_cache`] tempfile-then-rename), keyed on the target path so a
//!   repeated edit replaces rather than duplicates the entry;
//! - emits a desktop notification (title `"Patina: drift detected"`, body
//!   naming the target path) through the [`NotificationSink`], gated by the
//!   per-target 60-second rate limit (DEC-004).
//!
//! The cache is the watcher's notification ledger; it is **never** read by
//! `patina status`, which derives DRIFTED from SPEC-0001 REQ-018's own live
//! re-hash (REQ-007). The handler never auto-syncs the target back to source
//! (DEC-003) — it only observes and notifies.
//!
//! ## Clear-on-new-journal (REQ-007 `<done-when>`)
//!
//! A drift entry is only meaningful relative to the apply it was measured
//! against. When a fresh `patina apply` commits, its journal becomes the new
//! truth, and any cache bound to the prior journal `<ts>` is stale. The handler
//! detects this on the next batch — the persisted cache's `journal_ts` no
//! longer matches the journal timestamp the watcher is now reading against —
//! and clears the prior era's entries before binding the new timestamp and
//! upserting this batch's divergences. A cache already bound to the current
//! journal is left intact, so the per-target rate-limit ledger survives across
//! batches within one journal era (DEC-004). This is what keeps a `patina debug
//! drift-cache` view from showing entries mis-bound to a journal they were
//! never measured against.
//!
//! ## Why the rate limit reads the cache's own `detected_at_unix`
//!
//! DEC-004 caps notifications at one per target per 60-second window. The
//! window is keyed on the [`DriftEntry::detected_at_unix`] the previous drift
//! detection persisted: before notifying, the handler checks the cache for a
//! prior entry for the same target and suppresses the notification when the
//! prior detection is within the window. The cache is therefore the single
//! source of truth for the rate limit (no parallel in-memory ledger), so the
//! limit survives across debounced batches the same way the cache does.
//!
//! ## The notification sink (DEC-013)
//!
//! The emit path sits behind [`NotificationSink`] so headless CI — which has no
//! notification daemon — drives a capture sink that records `(title, body)`
//! tuples in memory. Only the production [`NotifySink`] touches `notify-rust`.

use crate::journal::content_hash;
use crate::watch::drift_cache::DriftCache;
use crate::watch::drift_cache::DriftEntry;
use crate::watch::drift_cache::hex_encode;
use crate::watch::drift_cache::load_drift_cache_file;
use crate::watch::drift_cache::write_drift_cache;
use crate::watch::subscriptions::ContentTarget;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Desktop-notification title for a detected drift (REQ-007 `<done-when>`).
pub const NOTIFICATION_TITLE: &str = "Patina: drift detected";

/// The per-target notification rate-limit window in seconds (DEC-004): at most
/// one notification per target per this many seconds, regardless of how many FS
/// events fire. Hardcoded in v1.0; configurability is deferred to v1.1.
pub const RATE_LIMIT_WINDOW_SECS: i64 = 60;

/// The drift cache's filename under the per-machine state directory
/// (`<state>/patina/drift.cache`), the watcher's notification ledger.
pub const DRIFT_CACHE_FILENAME: &str = "drift.cache";

/// A sink the drift handler emits desktop notifications through (DEC-013).
///
/// The production [`NotifySink`] calls `notify-rust`; a test sink records the
/// `(title, body)` pairs in memory so the drift scenarios assert
/// deterministically on headless CI runners that have no notification daemon.
pub trait NotificationSink {
    /// Emit one desktop notification with the given title and body. The sink is
    /// best-effort: a failure to reach the OS notification daemon must not
    /// crash the watcher, so an implementation logs and swallows transport
    /// errors rather than returning them.
    fn notify(&self, title: &str, body: &str);
}

/// The production notification sink: emits a real desktop notification via
/// `notify-rust` (REQ-007 / DEC-013).
///
/// A failure to reach the OS notification daemon (no `DBus` session bus on
/// Linux, a denied notification permission on macOS) is logged at `warn` and
/// swallowed: a missing notification daemon must never crash the watcher.
#[derive(Debug, Default, Clone, Copy)]
pub struct NotifySink;

impl NotificationSink for NotifySink {
    fn notify(&self, title: &str, body: &str) {
        match notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .show()
        {
            Ok(_handle) => {}
            Err(error) => {
                // Best-effort: a headless host or a denied permission must not
                // crash the watcher. Surface the failure in the log and move on.
                tracing::warn!(
                    target: "patina_core",
                    error = %error,
                    "drift_notify_failed"
                );
            }
        }
    }
}

/// How one content-target drift check settled, returned so the foreground loop
/// (and tests) can observe the outcome without re-parsing the structured log.
/// Mirrors [`crate::watch::reapply::ReapplyOutcome`]'s observable-outcome
/// shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftOutcome {
    /// The live bytes matched the journal-recorded hash: no drift, no
    /// notification, no cache write.
    Clean,
    /// Drift was detected and a notification emitted; the cache holds the
    /// entry.
    Notified,
    /// Drift was detected but the notification was suppressed by the per-target
    /// 60-second rate limit (DEC-004); the cache entry was still refreshed.
    RateLimited,
    /// The target could not be read (e.g. it was deleted between the FS event
    /// and the hash). Logged and skipped; no notification, no cache write. A
    /// read failure never crashes the watcher.
    Unreadable,
}

/// Process a debounced batch's content-target paths for drift, given the
/// committed expectations, the resolved state directory, a notification sink,
/// and the wall-clock time of detection.
///
/// For every `content_target` whose path appears in `batch_paths`, the handler
/// re-hashes the live bytes and compares to the recorded hash. Each diverging
/// target produces one [`DriftOutcome`] in the returned vector, in the order
/// the content targets were supplied. A target whose path is not in the batch
/// is skipped entirely (it produces no outcome).
///
/// Cache writes are coalesced: all detected divergences from this batch are
/// upserted into a single atomic [`write_drift_cache`], so a batch touching
/// several drifted targets rewrites the cache once rather than per target.
///
/// # Arguments
///
/// * `content_targets` - the committed content targets and their recorded
///   hashes ([`crate::watch::subscriptions::WatchSet::content_targets`]).
/// * `batch_paths` - the paths the debounced batch touched.
/// * `state_dir` - the resolved per-machine state directory; the cache lives at
///   `<state_dir>/drift.cache`.
/// * `journal_ts` - the journal `<ts>` the current expectations are measured
///   against, recorded in the cache so a reader can join it to the apply.
/// * `now_unix` - the detection time in Unix seconds; recorded per entry and
///   used for the rate-limit window.
/// * `sink` - the notification sink to emit through.
///
/// # Errors
///
/// Returns the per-target outcomes; a cache-write failure is logged at `warn`
/// and folds the affected outcomes to their non-cached form rather than
/// propagating — a transient cache-write failure must not crash the watcher.
pub fn handle_target_events(
    content_targets: &[ContentTarget],
    batch_paths: &[Utf8PathBuf],
    state_dir: &Utf8Path,
    journal_ts: &str,
    now_unix: i64,
    sink: &impl NotificationSink,
) -> Vec<DriftOutcome> {
    let cache_path = state_dir.join(DRIFT_CACHE_FILENAME);
    // The prior cache backs the rate-limit decision (DEC-004). A missing or
    // unreadable cache is treated as "no prior detections": the first drift
    // after a watcher start always notifies.
    let mut cache = load_drift_cache_file(&cache_path)
        .unwrap_or_else(|_| DriftCache::new(journal_ts, Vec::new()));
    // Clear-on-new-journal (REQ-007 `<done-when>`): when a fresh `patina apply`
    // commits, its journal becomes the new truth and the prior drift cache is
    // stale — every entry was measured against the superseded apply's
    // expectations, so it must be dropped rather than silently re-labeled with
    // the new timestamp. A persisted cache whose bound `journal_ts` no longer
    // matches the journal the watcher is now reading against is exactly that
    // superseded cache, so its entries (and the rate-limit ledger they carry)
    // are cleared here before the new timestamp is bound and this batch's
    // divergences are upserted. A cache that is already bound to the current
    // journal is left intact, so the rate limit survives across batches within
    // one journal era (DEC-004). A fresh/empty cache (no prior file) is bound to
    // the current timestamp by construction above and has nothing to clear.
    if cache.journal_ts != journal_ts {
        cache.entries.clear();
        journal_ts.clone_into(&mut cache.journal_ts);
    }

    let mut outcomes = Vec::new();
    let mut cache_dirty = false;

    for content in content_targets {
        if !batch_paths.contains(&content.target) {
            continue;
        }

        let live = match fs_err::read(content.target.as_std_path()) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    target: "patina_core",
                    drift_path = %content.target,
                    error = %error,
                    "drift_target_unreadable"
                );
                outcomes.push(DriftOutcome::Unreadable);
                continue;
            }
        };

        let actual_hash = content_hash(&live);
        if actual_hash == content.expected_hash {
            outcomes.push(DriftOutcome::Clean);
            continue;
        }

        // Divergence: log the DRIFTED event regardless of the rate limit, so
        // the structured log records every detection even when the
        // notification is suppressed.
        tracing::warn!(
            target: "patina_core",
            drift_path = %content.target,
            drift_expected_hash = %hex_encode(&content.expected_hash),
            drift_actual_hash = %hex_encode(&actual_hash),
            "drift"
        );

        let suppressed = recently_notified(&cache, &content.target, now_unix);
        upsert_entry(&mut cache, content, actual_hash, now_unix);
        cache_dirty = true;

        if suppressed {
            outcomes.push(DriftOutcome::RateLimited);
        } else {
            sink.notify(
                NOTIFICATION_TITLE,
                &format!("{} was modified outside Patina", content.target),
            );
            outcomes.push(DriftOutcome::Notified);
        }
    }

    if cache_dirty && let Err(error) = write_drift_cache(&cache_path, &cache) {
        tracing::warn!(
            target: "patina_core",
            error = %error,
            "drift_cache_write_failed"
        );
    }

    outcomes
}

/// Whether `target` was notified within the last [`RATE_LIMIT_WINDOW_SECS`]
/// seconds, per the cache's recorded `detected_at_unix` (DEC-004). A target
/// with no prior entry, or a prior entry older than the window, is not
/// rate-limited.
fn recently_notified(cache: &DriftCache, target: &Utf8Path, now_unix: i64) -> bool {
    cache
        .entries
        .iter()
        .find(|entry| entry.target == target)
        .is_some_and(|entry| {
            now_unix.saturating_sub(entry.detected_at_unix) < RATE_LIMIT_WINDOW_SECS
        })
}

/// Insert or replace the cache entry for `content.target` with the freshly
/// observed hash and detection time, keyed on the target path so a repeated
/// edit refreshes the single entry rather than appending a duplicate.
fn upsert_entry(
    cache: &mut DriftCache,
    content: &ContentTarget,
    actual_hash: [u8; 32],
    now_unix: i64,
) {
    let entry = DriftEntry::new(
        content.target.clone(),
        content.expected_hash,
        actual_hash,
        now_unix,
    );
    match cache
        .entries
        .iter_mut()
        .find(|existing| existing.target == content.target)
    {
        Some(existing) => *existing = entry,
        None => cache.entries.push(entry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::drift_cache::load_drift_cache_file;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// A capture notification sink (DEC-013): records the `(title, body)` pairs
    /// it was asked to emit so a test can assert on the count and content
    /// without a real OS notification daemon.
    #[derive(Default)]
    struct CaptureSink {
        captured: Mutex<Vec<(String, String)>>,
    }

    impl CaptureSink {
        fn count(&self) -> usize {
            self.captured.lock().expect("capture lock").len()
        }

        fn last(&self) -> Option<(String, String)> {
            self.captured.lock().expect("capture lock").last().cloned()
        }
    }

    impl NotificationSink for CaptureSink {
        fn notify(&self, title: &str, body: &str) {
            self.captured
                .lock()
                .expect("capture lock")
                .push((title.to_owned(), body.to_owned()));
        }
    }

    fn temp_state() -> (TempDir, Utf8PathBuf) {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path())
            .expect("utf8 tempdir")
            .to_owned();
        (temp, dir)
    }

    /// Write `bytes` to `<dir>/<name>` and return its path.
    fn write_target(dir: &Utf8Path, name: &str, bytes: &[u8]) -> Utf8PathBuf {
        let path = dir.join(name);
        fs_err::write(path.as_std_path(), bytes).expect("write target");
        path
    }

    /// CHK-011: an applied copy-mode target recorded with hash H1, overwritten
    /// to bytes hashing to H2 ≠ H1, drives exactly one notification and a
    /// drift-cache entry naming the target with `expected = H1, actual = H2`.
    #[test]
    fn divergent_target_notifies_once_and_records_h1_h2() {
        let (_temp, dir) = temp_state();
        // The target lives outside the state dir in practice; co-locating it in
        // the tempdir is fine — the handler reads it by absolute path.
        let target = write_target(&dir, ".gitconfig", b"H2");
        let h1 = content_hash(b"H1");
        let h2 = content_hash(b"H2");
        let content = ContentTarget {
            target: target.clone(),
            expected_hash: h1,
        };
        let sink = CaptureSink::default();

        let outcomes = handle_target_events(
            &[content],
            std::slice::from_ref(&target),
            &dir,
            "20260528T120000Z",
            1_716_897_600,
            &sink,
        );

        assert_eq!(outcomes, vec![DriftOutcome::Notified]);
        assert_eq!(sink.count(), 1, "exactly one notification (CHK-011)");
        let (title, body) = sink.last().expect("one notification");
        assert_eq!(title, NOTIFICATION_TITLE);
        assert!(body.contains(".gitconfig"), "body names the target: {body}");

        let cache = load_drift_cache_file(dir.join(DRIFT_CACHE_FILENAME)).expect("cache written");
        let entry = cache.entries.first().expect("one drift entry");
        assert_eq!(entry.target, target);
        assert_eq!(entry.expected_hash, h1, "expected hash is the recorded H1");
        assert_eq!(entry.actual_hash, h2, "actual hash is the observed H2");
        assert_ne!(entry.expected_hash, entry.actual_hash);
    }

    /// DEC-004: a second drift detection on the same target within the
    /// 60-second window is rate-limited — at most one notification, though the
    /// cache entry is refreshed to the latest observation.
    #[test]
    fn second_detection_within_the_window_is_rate_limited() {
        let (_temp, dir) = temp_state();
        let target = write_target(&dir, ".gitconfig", b"H2");
        let content = ContentTarget {
            target: target.clone(),
            expected_hash: content_hash(b"H1"),
        };
        let sink = CaptureSink::default();

        // First detection at t=1000 notifies.
        let first = handle_target_events(
            std::slice::from_ref(&content),
            std::slice::from_ref(&target),
            &dir,
            "20260528T120000Z",
            1000,
            &sink,
        );
        assert_eq!(first, vec![DriftOutcome::Notified]);

        // Overwrite to a third distinct content so the target still diverges,
        // then a second detection 30s later — within the 60s window.
        fs_err::write(target.as_std_path(), b"H3").expect("rewrite target");
        let second = handle_target_events(
            std::slice::from_ref(&content),
            std::slice::from_ref(&target),
            &dir,
            "20260528T120000Z",
            1030,
            &sink,
        );

        assert_eq!(second, vec![DriftOutcome::RateLimited]);
        assert_eq!(
            sink.count(),
            1,
            "the second detection within 60s is suppressed (DEC-004)"
        );
        // The cache entry is still refreshed to the latest observation.
        let cache = load_drift_cache_file(dir.join(DRIFT_CACHE_FILENAME)).expect("cache");
        let entry = cache.entries.first().expect("one entry");
        assert_eq!(entry.actual_hash, content_hash(b"H3"));
        assert_eq!(entry.detected_at_unix, 1030);
    }

    /// A detection past the 60-second window notifies again (the rate limit is
    /// a sliding window, not a one-shot mute).
    #[test]
    fn detection_past_the_window_notifies_again() {
        let (_temp, dir) = temp_state();
        let target = write_target(&dir, ".gitconfig", b"H2");
        let content = ContentTarget {
            target: target.clone(),
            expected_hash: content_hash(b"H1"),
        };
        let sink = CaptureSink::default();

        handle_target_events(
            std::slice::from_ref(&content),
            std::slice::from_ref(&target),
            &dir,
            "ts",
            1000,
            &sink,
        );
        // 61 seconds later — past the window.
        let later = handle_target_events(
            std::slice::from_ref(&content),
            std::slice::from_ref(&target),
            &dir,
            "ts",
            1061,
            &sink,
        );

        assert_eq!(later, vec![DriftOutcome::Notified]);
        assert_eq!(sink.count(), 2, "a detection past 60s notifies again");
    }

    /// A target whose live bytes match the recorded hash is CLEAN: no
    /// notification, no cache write.
    #[test]
    fn matching_target_is_clean_with_no_notification_or_cache() {
        let (_temp, dir) = temp_state();
        let target = write_target(&dir, ".gitconfig", b"H1");
        let content = ContentTarget {
            target: target.clone(),
            expected_hash: content_hash(b"H1"),
        };
        let sink = CaptureSink::default();

        let outcomes = handle_target_events(&[content], &[target], &dir, "ts", 1000, &sink);

        assert_eq!(outcomes, vec![DriftOutcome::Clean]);
        assert_eq!(sink.count(), 0, "an unchanged target emits no notification");
        assert!(
            !dir.join(DRIFT_CACHE_FILENAME).exists(),
            "a clean check writes no cache"
        );
    }

    /// A content target whose path is not in the batch is skipped entirely —
    /// it produces no outcome and no notification.
    #[test]
    fn target_not_in_batch_is_skipped() {
        let (_temp, dir) = temp_state();
        let target = write_target(&dir, ".gitconfig", b"H2");
        let content = ContentTarget {
            target,
            expected_hash: content_hash(b"H1"),
        };
        let sink = CaptureSink::default();

        // The batch touches an unrelated path, not the content target.
        let outcomes = handle_target_events(
            &[content],
            std::slice::from_ref(&dir.join("other")),
            &dir,
            "ts",
            1000,
            &sink,
        );

        assert!(outcomes.is_empty(), "an untouched target yields no outcome");
        assert_eq!(sink.count(), 0);
    }

    /// A target deleted between the FS event and the hash read is Unreadable:
    /// logged and skipped, never a crash, no notification, no cache write.
    #[test]
    fn unreadable_target_is_skipped_without_notification() {
        let (_temp, dir) = temp_state();
        let missing = dir.join(".gitconfig");
        let content = ContentTarget {
            target: missing.clone(),
            expected_hash: content_hash(b"H1"),
        };
        let sink = CaptureSink::default();

        let outcomes = handle_target_events(&[content], &[missing], &dir, "ts", 1000, &sink);

        assert_eq!(outcomes, vec![DriftOutcome::Unreadable]);
        assert_eq!(sink.count(), 0);
        assert!(!dir.join(DRIFT_CACHE_FILENAME).exists());
    }

    /// REQ-007 `<done-when>` (clear-on-new-journal): once a new `patina apply`
    /// commits, its journal becomes the new truth, so the next drift batch the
    /// watcher processes against the new `journal_ts` drops the prior journal
    /// era's entries rather than re-labeling them with the new timestamp. The
    /// surviving cache holds only this batch's divergences, bound to the new
    /// timestamp.
    #[test]
    fn a_journal_timestamp_advance_clears_prior_era_entries() {
        let (_temp, dir) = temp_state();
        let stale_target = write_target(&dir, "old.conf", b"OLD2");
        let live_target = write_target(&dir, "new.conf", b"NEW2");

        // Detect drift on `old.conf` under the first journal era. The cache now
        // holds one entry bound to the first timestamp.
        let stale_content = ContentTarget {
            target: stale_target.clone(),
            expected_hash: content_hash(b"OLD1"),
        };
        let sink = CaptureSink::default();
        handle_target_events(
            std::slice::from_ref(&stale_content),
            std::slice::from_ref(&stale_target),
            &dir,
            "20260101T000000Z",
            1000,
            &sink,
        );
        let before = load_drift_cache_file(dir.join(DRIFT_CACHE_FILENAME)).expect("cache");
        assert_eq!(before.journal_ts, "20260101T000000Z");
        assert_eq!(before.entries.len(), 1, "the first-era entry is recorded");

        // A new apply commits (advancing `journal_ts`); the next batch detects
        // drift on a different target, `new.conf`. The handler must drop the
        // superseded `old.conf` entry, not re-label it under the new timestamp.
        let live_content = ContentTarget {
            target: live_target.clone(),
            expected_hash: content_hash(b"NEW1"),
        };
        let outcomes = handle_target_events(
            std::slice::from_ref(&live_content),
            std::slice::from_ref(&live_target),
            &dir,
            "20260202T000000Z",
            2000,
            &sink,
        );
        assert_eq!(outcomes, vec![DriftOutcome::Notified]);

        let after = load_drift_cache_file(dir.join(DRIFT_CACHE_FILENAME)).expect("cache");
        assert_eq!(
            after.journal_ts, "20260202T000000Z",
            "the cache rebinds to the new journal era"
        );
        assert_eq!(
            after.entries.len(),
            1,
            "only the new era's divergence survives; the stale entry is cleared"
        );
        let entry = after.entries.first().expect("one entry");
        assert_eq!(
            entry.target, live_target,
            "the surviving entry is the new-era target, not the superseded one"
        );
        assert!(
            after.entries.iter().all(|e| e.target != stale_target),
            "no prior-era entry leaks into the new journal era"
        );
    }

    /// A repeated drift on the **same** target within one journal era keeps the
    /// rate-limit ledger intact: the timestamp does not advance, so the
    /// clear-on-new-journal path does not fire and the prior entry survives to
    /// suppress the second notification (DEC-004 boundary against the new
    /// clear-on-new-journal behaviour).
    #[test]
    fn same_journal_era_preserves_the_rate_limit_ledger() {
        let (_temp, dir) = temp_state();
        let target = write_target(&dir, ".gitconfig", b"H2");
        let content = ContentTarget {
            target: target.clone(),
            expected_hash: content_hash(b"H1"),
        };
        let sink = CaptureSink::default();

        // Two batches under the *same* journal_ts, 30s apart.
        handle_target_events(
            std::slice::from_ref(&content),
            std::slice::from_ref(&target),
            &dir,
            "20260101T000000Z",
            1000,
            &sink,
        );
        let second = handle_target_events(
            std::slice::from_ref(&content),
            std::slice::from_ref(&target),
            &dir,
            "20260101T000000Z",
            1030,
            &sink,
        );

        // The second is suppressed: the same-era entry was NOT cleared, so the
        // rate-limit ledger persisted across batches.
        assert_eq!(second, vec![DriftOutcome::RateLimited]);
        assert_eq!(sink.count(), 1, "the rate-limit ledger survived the batch");
    }

    /// Two distinct drifted targets in one batch each notify (the rate limit is
    /// per-target, not global) and the cache is written once with both entries.
    #[test]
    fn two_drifted_targets_each_notify_and_coalesce_to_one_cache_write() {
        let (_temp, dir) = temp_state();
        let a = write_target(&dir, "a.conf", b"A2");
        let b = write_target(&dir, "b.conf", b"B2");
        let targets = vec![
            ContentTarget {
                target: a.clone(),
                expected_hash: content_hash(b"A1"),
            },
            ContentTarget {
                target: b.clone(),
                expected_hash: content_hash(b"B1"),
            },
        ];
        let sink = CaptureSink::default();

        let outcomes =
            handle_target_events(&targets, &[a.clone(), b.clone()], &dir, "ts", 1000, &sink);

        assert_eq!(
            outcomes,
            vec![DriftOutcome::Notified, DriftOutcome::Notified]
        );
        assert_eq!(sink.count(), 2, "the per-target limit lets both notify");
        let cache = load_drift_cache_file(dir.join(DRIFT_CACHE_FILENAME)).expect("cache");
        assert_eq!(cache.entries.len(), 2, "one entry per drifted target");
    }
}
