//! The watcher's structured-log sink.
//!
//! The watcher owns `<state>/patina/logs/` and the rotating-log stack the
//! watcher writes its metrics into. [`state_dir::resolve`]
//! creates only `journal/` and `backups/`; it deliberately does not create
//! `logs/`. This module fills that gap: [`build_file_appender`] lazily
//! creates `<state>/patina/logs/` on first start and builds a daily-rotating
//! [`tracing_appender::rolling::RollingFileAppender`] that keeps the seven
//! most recent files.
//!
//! The watcher composes the returned non-blocking writer into its
//! `tracing` subscriber as the file layer (in foreground mode it also keeps a
//! stderr layer). The returned [`tracing_appender::non_blocking::WorkerGuard`]
//! must be held for the watcher's process lifetime: dropping it flushes and
//! tears down the background writer thread, so a watcher that drops the guard
//! early loses buffered log lines.
//!
//! [`state_dir::resolve`]: crate::state_dir::resolve
//!
//! # Examples
//!
//! ```no_run
//! let state = patina_core::state_dir::resolve()?;
//! let appender = patina_core::watch::logging::build_file_appender(&state)?;
//! // Hold `appender.guard` for the process lifetime; hand
//! // `appender.writer` to the `tracing` subscriber's file layer.
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use camino::Utf8Path;
use camino::Utf8PathBuf;
use thiserror::Error;
use tracing_appender::non_blocking::NonBlocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::Builder;
use tracing_appender::rolling::Rotation;

/// Name of the log subdirectory under the per-machine state root. Promoted to
/// `pub(super)` so the sibling [`crate::watch::service`] counter-recovery code
/// reuses this single definition rather than duplicating the literal.
pub(super) const LOGS_DIR: &str = "logs";

/// Filename prefix for the watcher's rotating log files. Daily rotation
/// appends a `.YYYY-MM-DD` date suffix, yielding files such as
/// `watch.log.2026-05-31`.
const FILENAME_PREFIX: &str = "watch.log";

/// Number of rotated log files to retain. Older files are pruned by the
/// appender on rotation, keeping the 7 most recent files.
const MAX_LOG_FILES: usize = 7;

/// Errors returned when building the watcher's file-appender stack.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LoggingError {
    /// Creating `<state>/patina/logs/` failed.
    #[error("failed to create watcher log directory `{path}`: {source}")]
    CreateLogDir {
        /// The log directory the watcher attempted to create.
        path: Utf8PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Initializing the rolling file appender failed.
    #[error("failed to initialize rolling log appender in `{path}`: {source}")]
    Appender {
        /// The log directory the appender was built against.
        path: Utf8PathBuf,
        /// The underlying appender-initialization error.
        #[source]
        source: tracing_appender::rolling::InitError,
    },
}

/// A non-blocking file-appender handle for the watcher's structured log.
///
/// `writer` is the non-blocking writer the `tracing` subscriber's file layer
/// wraps. `guard` is the flush guard that must outlive every log call: drop it
/// and the background writer thread shuts down, discarding any not-yet-flushed
/// lines. The watcher holds the whole [`FileAppender`] for its process
/// lifetime.
#[must_use = "the WorkerGuard inside must be held for the watcher's lifetime or buffered logs are lost"]
pub struct FileAppender {
    /// Non-blocking writer for the `tracing` subscriber's file layer.
    pub writer: NonBlocking,
    /// Flush guard; must be held for the watcher's process lifetime.
    pub guard: WorkerGuard,
}

/// Lazily create `<state>/patina/logs/` and build the watcher's
/// daily-rotating, keep-7 non-blocking file appender.
///
/// The directory is created on first call (idempotent — a pre-existing
/// directory is not an error). The appender rotates daily and prunes all but
/// the seven most recent files. The caller must hold the returned
/// [`FileAppender`] (specifically its `guard`) for as long as it logs.
///
/// # Arguments
///
/// * `state_dir` - The resolved per-machine state root (`<state>/patina/`), as
///   returned by [`crate::state_dir::resolve`]. The log directory is created at
///   `<state_dir>/logs/`.
///
/// # Errors
///
/// Returns [`LoggingError::CreateLogDir`] when the log directory cannot be
/// created and [`LoggingError::Appender`] when the rolling appender cannot be
/// initialized.
pub fn build_file_appender(state_dir: &Utf8Path) -> Result<FileAppender, LoggingError> {
    let logs_dir = state_dir.join(LOGS_DIR);
    fs_err::create_dir_all(logs_dir.as_std_path()).map_err(|source| {
        LoggingError::CreateLogDir {
            path: logs_dir.clone(),
            source,
        }
    })?;

    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILENAME_PREFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(logs_dir.as_std_path())
        .map_err(|source| LoggingError::Appender {
            path: logs_dir.clone(),
            source,
        })?;

    let (writer, guard) = tracing_appender::non_blocking(appender);
    Ok(FileAppender { writer, guard })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn creates_logs_dir_and_writes_a_line_to_a_file_under_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state =
            Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("temp path is utf-8");
        let logs_dir = state.join(LOGS_DIR);
        assert!(
            !logs_dir.exists(),
            "logs/ must not exist before the appender is built"
        );

        let mut appender = build_file_appender(&state).expect("build appender");
        assert!(
            logs_dir.is_dir(),
            "build_file_appender must lazily create logs/"
        );

        // Write a line through the non-blocking writer, then drop the guard
        // to flush the background worker before inspecting the directory.
        writeln!(appender.writer, "re_apply line").expect("write log line");
        drop(appender);

        let entries: Vec<_> = fs_err::read_dir(logs_dir.as_std_path())
            .expect("read logs dir")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "exactly one rotating log file should exist, got {entries:?}"
        );

        let log_path = entries.first().expect("one log entry").path();
        let file_name = log_path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("log file name is utf-8");
        assert!(
            file_name.starts_with(FILENAME_PREFIX),
            "log file `{file_name}` should start with the `{FILENAME_PREFIX}` prefix"
        );

        let contents = fs_err::read_to_string(&log_path).expect("read log file");
        assert!(
            contents.contains("re_apply line"),
            "the written line should land in the rotating file, got: {contents:?}"
        );
    }

    #[test]
    fn build_is_idempotent_when_logs_dir_already_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state =
            Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("temp path is utf-8");
        fs_err::create_dir_all(state.join(LOGS_DIR).as_std_path()).expect("pre-create logs dir");

        // A pre-existing logs/ directory must not be an error.
        let appender = build_file_appender(&state).expect("build appender over existing dir");
        drop(appender);
    }
}
