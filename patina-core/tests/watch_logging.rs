#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; the lint's allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for the watcher's `<state>/patina/logs/` directory and
//! its rotating-log stack.
//!
//! These tests pin two contracts at the public-API boundary:
//! `state_dir::resolve_with_env` creates `journal/` and `backups/` but NOT
//! `logs/`, and `watch::logging::build_file_appender` lazily creates `logs/`
//! and writes a daily-rotating log file into it.

use camino::Utf8PathBuf;
use patina_core::HostOs;
use patina_core::build_file_appender;
use patina_core::state_dir::resolve_with_env;
use std::io::Write;
use tempfile::TempDir;

fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
    let td = TempDir::new().expect("create tempdir");
    let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
    let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
    (td, canonical)
}

fn env_map(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> + use<> {
    move |name| {
        pairs
            .iter()
            .find_map(|(k, v)| (*k == name).then(|| v.clone()))
            .filter(|v| !v.is_empty())
    }
}

#[test]
fn resolve_creates_journal_and_backups_but_not_logs() {
    // Scenario: a fresh state directory. resolve() creates journal/ and
    // backups/; logs/ is owned by the watcher and must NOT be created here.
    let (_keep, t) = utf8_tempdir();
    let env = env_map(vec![("XDG_STATE_HOME", t.to_string())]);

    let root = resolve_with_env(HostOs::Linux, &env).expect("resolve");
    assert!(root.join("journal").is_dir(), "journal/ must exist");
    assert!(root.join("backups").is_dir(), "backups/ must exist");
    assert!(
        !root.join("logs").exists(),
        "resolve must NOT create logs/ — the watcher owns it"
    );
}

#[test]
fn build_file_appender_creates_logs_dir_and_writes_a_rotating_file() {
    // Scenario: the logging stack initialized against a state directory for
    // the first time creates <state>/logs/ and a daily-rotating appender
    // writes a log line into a file under it.
    let (_keep, root) = utf8_tempdir();
    let logs_dir = root.join("logs");
    assert!(
        !logs_dir.exists(),
        "logs/ must not exist before the appender is built"
    );

    let mut appender = build_file_appender(&root).expect("build appender");
    assert!(
        logs_dir.is_dir(),
        "build_file_appender must lazily create logs/"
    );

    writeln!(appender.writer, "re_apply id=1").expect("write log line");
    // Drop flushes the non-blocking worker before we inspect the file.
    drop(appender);

    let mut log_files: Vec<Utf8PathBuf> = fs_err::read_dir(logs_dir.as_std_path())
        .expect("read logs dir")
        .filter_map(Result::ok)
        .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
        .collect();
    log_files.sort();
    assert_eq!(
        log_files.len(),
        1,
        "exactly one rotating log file expected, got {log_files:?}"
    );

    let log_file = log_files.first().expect("one log file");
    let name = log_file.file_name().expect("log file has a name");
    assert!(
        name.starts_with("watch.log"),
        "rotating file `{name}` should carry the watcher prefix"
    );

    let contents = fs_err::read_to_string(log_file.as_std_path()).expect("read log file");
    assert!(
        contents.contains("re_apply id=1"),
        "the written line should land in the rotating file, got {contents:?}"
    );
}

#[test]
fn build_file_appender_is_idempotent_over_an_existing_logs_dir() {
    // A second start against a state directory whose logs/ already exists is
    // not an error.
    let (_keep, root) = utf8_tempdir();
    fs_err::create_dir_all(root.join("logs").as_std_path()).expect("pre-create logs/");

    let appender = build_file_appender(&root).expect("build over existing logs dir");
    drop(appender);
}
