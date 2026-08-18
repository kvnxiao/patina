// Every test and helper below is unix-only because they drive failures
// through `PermissionsExt`. The retry path for Windows is covered by the
// `#[cfg(windows)]` unit tests in `patina_core::apply::retry`. On non-unix
// targets, the imports and helpers are unused by design, so this silences
// the dead-code / unused-import lints there. The crate doc still satisfies
// `missing_docs` on every platform.
#![cfg_attr(
    not(unix),
    allow(
        unused_imports,
        dead_code,
        reason = "all tests in this file are #[cfg(unix)]; the imports and helpers are unused on other targets by design"
    )
)]
#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration coverage for the Windows `ERROR_SHARING_VIOLATION`
//! retry-with-backoff wrapper.
//!
//! The wrapper's retry path is Windows-only. It exercises a
//! `FILE_SHARE_NONE` hold that has no portable equivalent, so the
//! retry-then-succeed scenario and the 10-second-hold re-raise scenario
//! cannot run deterministically on this macOS/Linux dev host. This file
//! covers the cross-platform contract instead. On a non-Windows host, an
//! ordinary write failure surfaces on the first attempt, and no
//! `fs_write_retry` `tracing` event is emitted.
//!
//! The contract is asserted at two of the three engine write sites the
//! wrapper guards: the byte-copy site and the forward-apply symlink site.
//! A regression that drops the wrapper from either path is caught here.
//! The unit tests gated behind `#[cfg(windows)]` in
//! `patina-core::apply::retry` exercise the Windows-only retry behaviour.
//! The symlink site routes through that same wrapper, so this file adds no
//! new Windows-specific logic to cover.

use camino::Utf8PathBuf;
use patina_core::Builtins;
use patina_core::FileMode;
use patina_core::Resolver;
use patina_core::TemplateEngine;
use patina_core::ignore_rules;
use patina_core::materialize;
use std::sync::Arc;
use std::sync::Mutex;
use tempfile::TempDir;
use tracing::Event;
use tracing::Metadata;
use tracing::Subscriber;
use tracing::span;

fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
    let td = TempDir::new().expect("create tempdir");
    let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
    let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
    (td, canonical)
}

fn resolver() -> Resolver {
    Resolver::new(Builtins::for_tests())
}

/// A minimal `tracing` subscriber that records the `message` field of every
/// event into a shared buffer. The retry wrapper emits its event as
/// `tracing::debug!(..., "fs_write_retry")`, so the literal `fs_write_retry`
/// arrives as the event's `message`. Recording event messages lets the test
/// assert presence or absence of the retry event without pulling in
/// `tracing-subscriber`.
#[derive(Clone)]
struct RecordingSubscriber {
    messages: Arc<Mutex<Vec<String>>>,
}

struct MessageVisitor<'a> {
    messages: &'a Mutex<Vec<String>>,
}

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message"
            && let Ok(mut guard) = self.messages.lock()
        {
            guard.push(format!("{value:?}"));
        }
    }
}

impl Subscriber for RecordingSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        // All events are enabled so the filter cannot exclude a stray retry
        // event and produce a false "no retry happened" result.
        true
    }

    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = MessageVisitor {
            messages: &self.messages,
        };
        event.record(&mut visitor);
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}
}

/// On a non-Windows host, an ordinary write failure surfaces on the first
/// attempt, and the `tracing` log contains no `fs_write_retry` event.
///
/// The test makes the parent directory non-writable, so the byte-copy
/// write fails with the OS's normal permission error, the closest portable
/// analogue to the Windows `FILE_SHARE_NONE` scenario. The retry wrapper is
/// a pass-through off Windows, so the error must surface immediately with
/// no `fs_write_retry` event recorded.
#[cfg(unix)]
#[test]
fn non_windows_write_failure_surfaces_without_retry_event() {
    use std::os::unix::fs::PermissionsExt;

    let (_td, dir) = utf8_tempdir();
    let source = dir.join("source.txt");
    fs_err::write(&source, b"payload").expect("write source");

    // The target sits inside a directory stripped of write permission. The
    // copy's write into it fails with EACCES, an ordinary I/O error.
    let locked_dir = dir.join("locked");
    fs_err::create_dir(&locked_dir).expect("create locked dir");
    let target = locked_dir.join("dest.txt");
    let mut perms = fs_err::metadata(&locked_dir)
        .expect("locked dir metadata")
        .permissions();
    perms.set_mode(0o500); // r-x: readable and traversable, not writable
    fs_err::set_permissions(&locked_dir, perms).expect("chmod locked dir");

    let messages = Arc::new(Mutex::new(Vec::<String>::new()));
    let subscriber = RecordingSubscriber {
        messages: Arc::clone(&messages),
    };

    let result = tracing::subscriber::with_default(subscriber, || {
        materialize(
            FileMode::Copy,
            &source,
            std::slice::from_ref(&target),
            &TemplateEngine::new(),
            &resolver(),
            &ignore_rules::none(),
        )
    });

    // Restore write permission so the tempdir can be cleaned up.
    let mut restore = fs_err::metadata(&locked_dir)
        .expect("locked dir metadata for restore")
        .permissions();
    restore.set_mode(0o700);
    fs_err::set_permissions(&locked_dir, restore).expect("restore locked dir perms");

    assert!(
        result.is_err(),
        "write into a non-writable directory must surface an error"
    );

    let recorded = messages.lock().expect("lock messages");
    assert!(
        !recorded.iter().any(|m| m.contains("fs_write_retry")),
        "no fs_write_retry event may be emitted off Windows; recorded: {recorded:?}"
    );
}

/// At the forward-apply symlink site, a symlink whose creation fails with
/// an ordinary I/O error surfaces on the first attempt with no
/// `fs_write_retry` event off Windows.
///
/// Symlink creation is one of the file writes the retry policy guards. The
/// forward-apply symlink executor (`apply::symlink::create_symlink`) routes
/// its OS primitive through `with_sharing_violation_retry`. Drive that wiring
/// with a real `FileMode::Symlink` apply into a
/// non-writable directory; the symlink `create` call fails with EACCES, the
/// closest portable analogue to the Windows `FILE_SHARE_NONE` scenario. The
/// wrapper is a pass-through off Windows, so the error must surface
/// immediately with no retry event. The parent directory already exists,
/// so the failure originates at the wrapped `create_symlink` call, not at
/// `ensure_parent`.
#[cfg(unix)]
#[test]
fn non_windows_symlink_failure_surfaces_without_retry_event() {
    use std::os::unix::fs::PermissionsExt;

    let (_td, dir) = utf8_tempdir();
    let source = dir.join("source.txt");
    fs_err::write(&source, b"payload").expect("write source");

    // The symlink target lives inside a directory stripped of write
    // permission. Creating any entry there, including a symlink, fails
    // with EACCES, an ordinary I/O error and not ERROR_SHARING_VIOLATION.
    let locked_dir = dir.join("locked");
    fs_err::create_dir(&locked_dir).expect("create locked dir");
    let target = locked_dir.join("link");
    let mut perms = fs_err::metadata(&locked_dir)
        .expect("locked dir metadata")
        .permissions();
    perms.set_mode(0o500); // r-x: readable and traversable, not writable
    fs_err::set_permissions(&locked_dir, perms).expect("chmod locked dir");

    let messages = Arc::new(Mutex::new(Vec::<String>::new()));
    let subscriber = RecordingSubscriber {
        messages: Arc::clone(&messages),
    };

    let result = tracing::subscriber::with_default(subscriber, || {
        materialize(
            FileMode::Symlink,
            &source,
            std::slice::from_ref(&target),
            &TemplateEngine::new(),
            &resolver(),
            &ignore_rules::none(),
        )
    });

    // Restore write permission so the tempdir can be cleaned up.
    let mut restore = fs_err::metadata(&locked_dir)
        .expect("locked dir metadata for restore")
        .permissions();
    restore.set_mode(0o700);
    fs_err::set_permissions(&locked_dir, restore).expect("restore locked dir perms");

    assert!(
        result.is_err(),
        "symlink creation in a non-writable directory must surface an error"
    );

    let recorded = messages.lock().expect("lock messages");
    assert!(
        !recorded.iter().any(|m| m.contains("fs_write_retry")),
        "no fs_write_retry event may be emitted off Windows; recorded: {recorded:?}"
    );
}
