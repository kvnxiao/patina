//! The per-OS background-service abstraction.
//!
//! `patina watch install` registers the watcher as a per-user background
//! service that launches the foreground watcher (`patina watch --foreground`)
//! at login, and `uninstall` / `start` / `stop` / `restart` / `status` manage
//! that registration. Each OS speaks to its own native supervisor — `launchd`
//! on macOS, `systemd --user` on Linux, a per-user Scheduled Task on Windows —
//! through a common [`ServiceBackend`] trait so the CLI command surface is
//! written once.
//!
//! [`current`] is the factory: it dispatches on [`crate::state_dir::HostOs`]
//! and
//! returns the backend for the running host. macOS returns the `launchd`
//! backend; Linux returns the `systemd` backend when `systemd --user` is
//! reachable and the [`unsupported`] stub otherwise (non-systemd init systems
//! are served by `patina watch --foreground` under the user's own
//! supervisor); Windows returns the `scheduled_task` backend, which registers a
//! per-user, non-elevated Scheduled Task.
//!
//! The `launchd`, `systemd`, and `scheduled_task` backend modules are each
//! gated to their own target OS, so they are referenced as plain code spans
//! rather than intra-doc links: a link to a cfg-excluded module would break
//! the docs build on the other OS (the docs gate runs on Linux, where
//! `launchd` and `scheduled_task` are compiled out).
//!
//! ## Counter recovery
//!
//! [`ServiceStatus`] carries two watcher-internal counters,
//! `subscriptions_count` and `re_applies_since_start`, that the running watcher
//! emits only to its structured log. `status` is a
//! separate, short-lived process and cannot read the watcher's in-memory state,
//! so it recovers the two counters by reading the most recent rotated log file
//! under `<state>/patina/logs/` ([`recover_log_counters`]). When the log is
//! absent or unparseable the counters report `None` rather than failing the
//! command. The supervisor-derived fields (`installed`, `running`,
//! `last_fired_at`, `last_exit_code`) come from the platform query each backend
//! implements.

#[cfg(target_os = "macos")]
pub mod launchd;
#[cfg(windows)]
pub mod scheduled_task;
#[cfg(target_os = "linux")]
pub mod systemd;
pub mod unsupported;

use crate::state_dir::HostOs;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use thiserror::Error;

/// The service name registered with the platform supervisor (the launchd
/// label, the systemd unit stem, the Scheduled Task name root). Each backend
/// renders it into its platform-native form (`com.patina.watcher`,
/// `patina-watcher.service`, `Patina Watcher`).
pub const SERVICE_LABEL: &str = "com.patina.watcher";

/// The subcommand the registered service launches: the foreground watcher.
/// The service descriptor's program arguments are the canonical binary path
/// followed by these tokens.
pub const FOREGROUND_ARGS: [&str; 2] = ["watch", "--foreground"];

/// Errors returned by [`ServiceBackend`] operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ServiceError {
    /// `install` was called but the service is already registered. The user
    /// must `patina watch uninstall` before re-installing.
    #[error("the patina watcher service is already installed; run `patina watch uninstall` first")]
    AlreadyInstalled,

    /// The running host has no implemented service backend yet (the factory
    /// returned the [`unsupported`] stub). Directs the user to the foreground
    /// escape hatch.
    #[error(
        "no background-service backend is available on this host; \
         run `patina watch --foreground` under your own supervisor instead"
    )]
    Unsupported,

    /// Resolving the running binary's canonical path failed, so the service
    /// descriptor cannot name an executable to launch.
    #[error("failed to resolve the running patina binary path: {0}")]
    ResolveBinary(String),

    /// Writing or removing the service descriptor file failed.
    #[error("failed to write the service descriptor `{path}`: {source}")]
    WriteDescriptor {
        /// The descriptor path the backend attempted to write or remove.
        path: Utf8PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Invoking the platform supervisor (`launchctl` / `systemctl` / the
    /// Scheduled Task API) failed.
    #[error("the platform service supervisor failed: {0}")]
    Supervisor(String),
}

/// The result of a single lifecycle action, surfaced in the CLI's `--json`
/// envelope's `result` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleResult {
    /// `install` registered the service.
    Installed,
    /// `uninstall` removed the service.
    Uninstalled,
    /// `start` asked the supervisor to start the service.
    Started,
    /// `stop` asked the supervisor to stop the service.
    Stopped,
    /// `restart` asked the supervisor to stop then start the service.
    Restarted,
    /// The lifecycle action was a no-op because the service was not installed
    /// (lifecycle subcommands on a not-installed service are no-ops with a
    /// clear message, not supervisor errors).
    NotInstalled,
}

impl LifecycleResult {
    /// The stable lower-case word naming this result in the `--json`
    /// envelope's `result` field.
    ///
    /// # Examples
    ///
    /// ```
    /// use patina_core::watch::service::LifecycleResult;
    ///
    /// assert_eq!(LifecycleResult::Installed.label(), "installed");
    /// assert_eq!(LifecycleResult::NotInstalled.label(), "not_installed");
    /// ```
    #[must_use = "the label is a value to render, not a side effect"]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Uninstalled => "uninstalled",
            Self::Started => "started",
            Self::Stopped => "stopped",
            Self::Restarted => "restarted",
            Self::NotInstalled => "not_installed",
        }
    }
}

/// The current state of the registered service (the `status` `--json`
/// object).
///
/// `installed`, `running`, `last_fired_at`, and `last_exit_code` are derived
/// from the platform supervisor query; `subscriptions_count` and
/// `re_applies_since_start` are recovered from the watcher's structured log.
/// A field is `None` when its source is absent (no apply ever ran,
/// or the log is unreadable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    /// Whether the service descriptor is registered with the supervisor.
    pub installed: bool,
    /// Whether the supervisor reports the service currently running.
    pub running: bool,
    /// The supervisor's most recent last-fired timestamp, if recorded.
    pub last_fired_at: Option<String>,
    /// The supervisor's most recent recorded exit code, if any.
    pub last_exit_code: Option<i64>,
    /// The watcher's last-logged subscription count, recovered from the log.
    pub subscriptions_count: Option<u64>,
    /// The watcher's last-logged re-apply count since start, from the log.
    pub re_applies_since_start: Option<u64>,
}

/// A per-OS background-service backend.
///
/// Each method is a thin wrapper over the platform's native service
/// primitives; none requires admin or sudo on its default path. `install`
/// points the service at the running binary's canonical absolute path invoked
/// with `watch --foreground`.
pub trait ServiceBackend {
    /// Register the service with the platform supervisor and load it.
    /// Returns [`ServiceError::AlreadyInstalled`] when the service
    /// is already registered.
    ///
    /// # Errors
    ///
    /// Returns a [`ServiceError`] when the binary path cannot be resolved, the
    /// descriptor cannot be written, or the supervisor invocation fails.
    fn install(&self) -> Result<LifecycleResult, ServiceError>;

    /// Stop the running watcher (if any), unregister the service, and remove
    /// its descriptor. A not-installed service is a no-op returning
    /// [`LifecycleResult::NotInstalled`].
    ///
    /// # Errors
    ///
    /// Returns a [`ServiceError`] when the supervisor invocation or descriptor
    /// removal fails.
    fn uninstall(&self) -> Result<LifecycleResult, ServiceError>;

    /// Ask the supervisor to start the installed service. A
    /// not-installed service is a no-op returning
    /// [`LifecycleResult::NotInstalled`].
    ///
    /// # Errors
    ///
    /// Returns a [`ServiceError`] when the supervisor invocation fails.
    fn start(&self) -> Result<LifecycleResult, ServiceError>;

    /// Ask the supervisor to stop the running service without unregistering it.
    /// A not-installed service is a no-op returning
    /// [`LifecycleResult::NotInstalled`].
    ///
    /// # Errors
    ///
    /// Returns a [`ServiceError`] when the supervisor invocation fails.
    fn stop(&self) -> Result<LifecycleResult, ServiceError>;

    /// Stop then start the service. A not-installed service is a
    /// no-op returning [`LifecycleResult::NotInstalled`].
    ///
    /// # Errors
    ///
    /// Returns a [`ServiceError`] when either underlying action fails.
    fn restart(&self) -> Result<LifecycleResult, ServiceError>;

    /// Query the supervisor for the service's current state. The
    /// `subscriptions_count` / `re_applies_since_start` counters are recovered
    /// from the structured log by the caller, not here.
    ///
    /// # Errors
    ///
    /// Returns a [`ServiceError`] when the supervisor query fails for a reason
    /// other than the service simply not being installed (which reports
    /// `installed = false`).
    fn status(&self) -> Result<ServiceStatus, ServiceError>;
}

/// Return the background-service backend for the running host.
///
/// Dispatches on [`HostOs::current`]: macOS returns the `launchd` backend;
/// Linux returns the `systemd` backend when `systemd --user` is reachable
/// and the [`unsupported`] stub otherwise (non-systemd init); Windows
/// returns the `scheduled_task` backend (a per-user, non-elevated Scheduled
/// Task). The backend is bound to `state_dir`, the resolved per-machine state
/// root, so `status` can recover the watcher's log counters from
/// `<state_dir>/logs/`.
///
/// # Examples
///
/// ```no_run
/// let state = patina_core::state_dir::resolve()?;
/// let backend = patina_core::watch::service::current(&state);
/// let status = backend.status()?;
/// println!("installed: {}", status.installed);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the returned backend performs the lifecycle action; call one of its methods"]
pub fn current(state_dir: &Utf8Path) -> Box<dyn ServiceBackend> {
    match HostOs::current() {
        #[cfg(target_os = "macos")]
        HostOs::MacOs => Box::new(launchd::LaunchdBackend::new(state_dir.to_path_buf())),
        // Linux: drive `systemd --user` when its user bus is reachable; on a
        // non-systemd init the user manager is absent, so fall back to the
        // foreground-escape-hatch stub.
        #[cfg(target_os = "linux")]
        HostOs::Linux if systemd::SystemdBackend::is_available() => {
            Box::new(systemd::SystemdBackend::new(state_dir.to_path_buf()))
        }
        // Windows: a per-user, non-elevated Scheduled Task. HKCU-
        // scoped, so it lives in `patina-core` rather than `patina-elevate`.
        #[cfg(windows)]
        HostOs::Windows => Box::new(scheduled_task::ScheduledTaskBackend::new(
            state_dir.to_path_buf(),
        )),
        // Non-systemd Linux (and any other host without a reachable supervisor)
        // falls back to the foreground escape hatch.
        _ => Box::new(unsupported::UnsupportedBackend),
    }
}

/// The canonical absolute path of the running `patina` binary, for the service
/// descriptor's program arguments (`current_exe` → canonicalize).
///
/// # Errors
///
/// Returns [`ServiceError::ResolveBinary`] when the running executable path
/// cannot be read or canonicalized, or is not valid UTF-8.
pub fn canonical_binary_path() -> Result<Utf8PathBuf, ServiceError> {
    let exe = std::env::current_exe()
        .map_err(|source| ServiceError::ResolveBinary(source.to_string()))?;
    let canonical = exe
        .canonicalize()
        .map_err(|source| ServiceError::ResolveBinary(source.to_string()))?;
    Utf8PathBuf::from_path_buf(canonical)
        .map_err(|raw| ServiceError::ResolveBinary(format!("non-UTF-8 path: {}", raw.display())))
}

use crate::watch::logging::LOGS_DIR;

/// Recover the watcher's `subscriptions_count` and `re_applies_since_start`
/// counters from the most recent rotated log under `<state>/logs/`.
///
/// Reads the newest `watch.log*` file in the log directory and scans it for the
/// last `subscriptions=<n>` field (logged on each `watch_started` /
/// `journal_rescan`) and counts the `re_apply` success events since the most
/// recent `watch_started`. Returns `(None, None)` when the log directory is
/// absent, empty, or unreadable — a missing log is not an error (the watcher
/// may never have started since the last rotation).
///
/// The pair is `(subscriptions_count, re_applies_since_start)`.
#[must_use = "the recovered counters populate the status object; ignoring them drops the metrics"]
pub fn recover_log_counters(state_dir: &Utf8Path) -> (Option<u64>, Option<u64>) {
    let Some(log_path) = most_recent_log(state_dir) else {
        return (None, None);
    };
    let Ok(contents) = fs_err::read_to_string(log_path.as_std_path()) else {
        return (None, None);
    };
    parse_counters(&contents)
}

/// The most recent (lexically greatest, which for the daily `watch.log.DATE`
/// suffix is also the newest) `watch.log*` file under `<state>/logs/`, or
/// `None` when the directory is absent or holds no log file.
fn most_recent_log(state_dir: &Utf8Path) -> Option<Utf8PathBuf> {
    let logs_dir = state_dir.join(LOGS_DIR);
    let entries = fs_err::read_dir(logs_dir.as_std_path()).ok()?;
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| Utf8PathBuf::from_path_buf(entry.path()).ok())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.starts_with("watch.log"))
        })
        .max()
}

/// Parse the two counters from the watcher's structured-log text.
///
/// `subscriptions_count` is the value of the last `subscriptions=<n>` field in
/// the log (the watcher logs it on `watch_started` and every
/// `journal_rescan`). `re_applies_since_start` counts `re_apply` success events
/// after the most recent `watch_started` line, so a restart resets the count.
/// Either is `None` when no corresponding line is present.
fn parse_counters(contents: &str) -> (Option<u64>, Option<u64>) {
    let mut subscriptions: Option<u64> = None;
    let mut re_applies: u64 = 0;
    let mut saw_start = false;

    for line in contents.lines() {
        if line.contains("watch_started") {
            // A fresh watcher run resets the re-apply count for "since start".
            saw_start = true;
            re_applies = 0;
        }
        if let Some(value) = field_value(line, "subscriptions=") {
            subscriptions = Some(value);
        }
        if is_reapply_success(line) {
            re_applies = re_applies.saturating_add(1);
        }
    }

    let re_applies_since_start = saw_start.then_some(re_applies);
    (subscriptions, re_applies_since_start)
}

/// Whether a log line is a `re_apply` *success* event (`re_apply
/// re_apply_id=…`) rather than a `re_apply_failed` warning. The trailing-space
/// match mirrors the integration suite's `re_apply re_apply_id` discriminator.
fn is_reapply_success(line: &str) -> bool {
    line.contains("re_apply re_apply_id=")
}

/// Parse the `u64` value following `key` in a structured-log line, reading up
/// to the next whitespace. Returns `None` when the key is absent or the value
/// does not parse as a `u64`.
fn field_value(line: &str, key: &str) -> Option<u64> {
    let start = line.find(key)? + key.len();
    let rest = line.get(start..)?;
    let token = rest.split_whitespace().next()?;
    token.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_result_labels_are_stable_distinct_words() {
        // The `result` field is part of the CLI's JSON surface; each variant
        // must map to its own stable word so a consumer can switch on it.
        // Asserting every arm gates the mapping against a variant being
        // collapsed or renamed.
        assert_eq!(LifecycleResult::Installed.label(), "installed");
        assert_eq!(LifecycleResult::Uninstalled.label(), "uninstalled");
        assert_eq!(LifecycleResult::Started.label(), "started");
        assert_eq!(LifecycleResult::Stopped.label(), "stopped");
        assert_eq!(LifecycleResult::Restarted.label(), "restarted");
        assert_eq!(LifecycleResult::NotInstalled.label(), "not_installed");
    }

    #[test]
    fn field_value_reads_the_u64_after_the_key() {
        assert_eq!(
            field_value(
                "patina_core: watch_started subscriptions=3",
                "subscriptions="
            ),
            Some(3)
        );
        // Trailing fields after the value are ignored (read to whitespace).
        assert_eq!(
            field_value("x subscriptions=12 other=y", "subscriptions="),
            Some(12)
        );
        // A missing key yields None.
        assert_eq!(field_value("no such field here", "subscriptions="), None);
        // A non-numeric value yields None rather than a panic.
        assert_eq!(field_value("subscriptions=abc", "subscriptions="), None);
    }

    #[test]
    fn parse_counters_reads_last_subscriptions_and_counts_reapplies_since_start() {
        // A log with a start (subscriptions=2), one re-apply, a rescan that
        // bumps subscriptions to 4, and a second re-apply: the recovered
        // subscription count is the *last* value (4) and the re-apply count
        // since the (single) start is 2.
        let log = "\
patina_core: watch_started subscriptions=2
patina_core: re_apply re_apply_id=20260531T000001Z re_apply_files_changed=1
patina_core: journal_rescan subscriptions=4
patina_core: re_apply re_apply_id=20260531T000002Z re_apply_files_changed=1
";
        assert_eq!(parse_counters(log), (Some(4), Some(2)));
    }

    #[test]
    fn parse_counters_resets_reapply_count_on_a_later_start() {
        // A second `watch_started` (a watcher restart) resets the
        // "since start" re-apply count: only the re-apply after the second
        // start is counted.
        let log = "\
patina_core: watch_started subscriptions=1
patina_core: re_apply re_apply_id=a re_apply_files_changed=1
patina_core: watch_started subscriptions=5
patina_core: re_apply re_apply_id=b re_apply_files_changed=1
";
        assert_eq!(parse_counters(log), (Some(5), Some(1)));
    }

    #[test]
    fn parse_counters_does_not_count_failed_reapplies() {
        // A `re_apply_failed` warning is not a successful re-apply and must not
        // be counted; only the success event (`re_apply re_apply_id=`) is.
        let log = "\
patina_core: watch_started subscriptions=1
patina_core: re_apply_failed re_apply_id=a error=boom
patina_core: re_apply re_apply_id=b re_apply_files_changed=0
";
        assert_eq!(parse_counters(log), (Some(1), Some(1)));
    }

    #[test]
    fn parse_counters_reports_none_when_the_watcher_never_started() {
        // A log with no `watch_started` line (e.g. only rotated noise) yields a
        // None re-apply count: "since start" is undefined without a start.
        let log = "some unrelated line\nanother line\n";
        assert_eq!(parse_counters(log), (None, None));
    }

    #[test]
    fn recover_log_counters_reports_none_when_logs_dir_is_absent() {
        // No `<state>/logs/` at all: both counters are None, not an error
        // (a missing log reports null rather than failing status).
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf-8 temp path");
        assert_eq!(recover_log_counters(&state), (None, None));
    }

    #[test]
    fn recover_log_counters_reads_the_most_recent_rotated_log() {
        // Two daily-rotated files: the lexically-greatest date suffix is the
        // newest, and its counters are the ones recovered.
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf-8 temp path");
        let logs = state.join(LOGS_DIR);
        fs_err::create_dir_all(logs.as_std_path()).expect("mkdir logs");
        fs_err::write(
            logs.join("watch.log.2026-05-30").as_std_path(),
            "patina_core: watch_started subscriptions=1\n",
        )
        .expect("write old log");
        fs_err::write(
            logs.join("watch.log.2026-05-31").as_std_path(),
            "patina_core: watch_started subscriptions=9\n\
             patina_core: re_apply re_apply_id=x re_apply_files_changed=1\n",
        )
        .expect("write new log");

        assert_eq!(recover_log_counters(&state), (Some(9), Some(1)));
    }
}
