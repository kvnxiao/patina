//! Integration tests for fs retry.

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

#[cfg(unix)]
#[test]
fn non_windows_write_failure_surfaces_without_retry_event() {
    use std::os::unix::fs::PermissionsExt;

    let (_td, dir) = utf8_tempdir();
    let source = dir.join("source.txt");
    fs_err::write(&source, b"payload").expect("write source");

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

#[cfg(unix)]
#[test]
fn non_windows_symlink_failure_surfaces_without_retry_event() {
    use std::os::unix::fs::PermissionsExt;

    let (_td, dir) = utf8_tempdir();
    let source = dir.join("source.txt");
    fs_err::write(&source, b"payload").expect("write source");

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
