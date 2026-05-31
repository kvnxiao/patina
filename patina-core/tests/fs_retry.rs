#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration coverage for the Windows `ERROR_SHARING_VIOLATION`
//! retry-with-backoff wrapper (REQ-010, T-003).
//!
//! The wrapper's retry path is Windows-only and exercises a `FILE_SHARE_NONE`
//! hold that has no portable equivalent, so CHK-015 (retry-then-succeed) and
//! the 10s-hold re-raise scenario cannot run deterministically on this
//! macOS/Linux dev host. This file covers the cross-platform contract
//! (CHK-016): on a non-Windows host an ordinary write failure surfaces on the
//! first attempt with no `fs_write_retry` `tracing` event emitted. The
//! contract is asserted at two of the three engine write sites the wrapper
//! guards — the byte-copy site and the forward-apply symlink site — so a
//! regression that drops the wrapper from either path is caught here. The
//! Windows-only retry behaviour is exercised by the unit tests gated behind
//! `#[cfg(windows)]` in `patina-core::apply::retry`; the symlink site routes
//! through that same wrapper, so it adds no new Windows-specific logic to
//! cover.

use camino::Utf8PathBuf;
use patina_core::Builtins;
use patina_core::FileMode;
use patina_core::Resolver;
use patina_core::TemplateEngine;
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
/// assert presence/absence of the retry event without pulling in
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
        // Enable everything so a stray retry event cannot slip past the
        // filter and produce a false "no retry happened" pass.
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

/// CHK-016: on a non-Windows host an ordinary write failure surfaces on the
/// first attempt and the `tracing` log contains no `fs_write_retry` event.
///
/// We make the parent directory non-writable so the byte-copy write fails
/// with the OS's normal permission error — the closest portable analogue to
/// the Windows `FILE_SHARE_NONE` scenario the CHK describes. The retry
/// wrapper is a pass-through off Windows, so the error must surface
/// immediately and no `fs_write_retry` event may be recorded.
#[cfg(unix)]
#[test]
fn non_windows_write_failure_surfaces_without_retry_event() {
    use std::os::unix::fs::PermissionsExt;

    let (_td, dir) = utf8_tempdir();
    let source = dir.join("source.txt");
    fs_err::write(&source, b"payload").expect("write source");

    // A target inside a directory we strip of write permission: the copy's
    // write into it fails with EACCES, an ordinary I/O error.
    let locked_dir = dir.join("locked");
    fs_err::create_dir(&locked_dir).expect("create locked dir");
    let target = locked_dir.join("dest.txt");
    let mut perms = fs_err::metadata(&locked_dir)
        .expect("locked dir metadata")
        .permissions();
    perms.set_mode(0o500); // r-x: readable/traversable but not writable
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

/// CHK-016 at the forward-apply symlink site: a symlink whose creation fails
/// with an ordinary I/O error surfaces on the first attempt with no
/// `fs_write_retry` event off Windows.
///
/// REQ-010 names symlink creation as one of the "all file writes" the retry
/// policy guards. The forward-apply symlink executor
/// (`apply::symlink::create_symlink`) routes its OS primitive through
/// `with_sharing_violation_retry`; this test pins that wiring by driving a
/// real `FileMode::Symlink` apply into a non-writable directory (the symlink
/// `create` fails with EACCES, the closest portable analogue to the Windows
/// `FILE_SHARE_NONE` scenario). Since the wrapper is a pass-through off
/// Windows, the error must surface immediately with no retry event. The
/// parent directory already exists, so the failure originates at the wrapped
/// `create_symlink` call, not at `ensure_parent`.
#[cfg(unix)]
#[test]
fn non_windows_symlink_failure_surfaces_without_retry_event() {
    use std::os::unix::fs::PermissionsExt;

    let (_td, dir) = utf8_tempdir();
    let source = dir.join("source.txt");
    fs_err::write(&source, b"payload").expect("write source");

    // The symlink target lives inside a directory we strip of write
    // permission: creating any entry (including a symlink) in it fails with
    // EACCES, an ordinary I/O error that is not ERROR_SHARING_VIOLATION.
    let locked_dir = dir.join("locked");
    fs_err::create_dir(&locked_dir).expect("create locked dir");
    let target = locked_dir.join("link");
    let mut perms = fs_err::metadata(&locked_dir)
        .expect("locked dir metadata")
        .permissions();
    perms.set_mode(0o500); // r-x: traversable but not writable
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
