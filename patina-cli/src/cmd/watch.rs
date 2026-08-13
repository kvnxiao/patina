//! `patina watch` command logic.
//!
//! The command has two modes. `--foreground` runs the watcher loop inline
//! ([`patina_core::run_foreground`]), attached to the invoking shell, until
//! Ctrl-C (SIGINT) or, on POSIX, SIGTERM shuts it down. The
//! lifecycle subcommands (`install` / `uninstall` / `start` / `stop` /
//! `restart` / `status`) manage the per-OS background service through the
//! [`patina_core::watch::service`] backend.
//!
//! All lifecycle subcommands except `status` acquire the exclusive advisory
//! lock; `status` acquires the shared lock. The engine
//! semantics (state-dir resolution, the service backend, log-counter recovery)
//! live in `patina_core`; this module is control flow, lock acquisition, and
//! output formatting only, all routed through the [`Reporter`].
//!
//! Before starting any mode, the command surfaces the forward-compatible-but-
//! ignored `[watcher] debounce_ms` warning through the
//! reporter.

use crate::cli::WatchArgs;
use crate::cli::WatchCommand;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use crate::output::style::Styles;
use crate::output::style::paint;
use crate::output::table::emit_aligned;
use crate::output::table::row;
use anyhow::Context;
use anyhow::Result;
use patina_core::LifecycleResult;
use patina_core::LockKind;
use patina_core::ServiceBackend;
use patina_core::ServiceError;
use patina_core::ServiceStatus;
use patina_core::acquire_lock;
use patina_core::exclusive_timeout;

/// Run `patina watch`. Returns the process exit code.
///
/// Dispatches on the chosen mode: a lifecycle subcommand routes to
/// [`run_lifecycle`]; `--foreground` runs the watcher inline and returns `0`
/// on a clean exit; with neither, the command reports the usage hint and
/// returns a non-zero code.
///
/// # Errors
///
/// Returns an error when the foreground watcher fails to start or run, which
/// covers state-directory resolution, the log appender, the journal read, and
/// watcher arming. Also returns an error when a lifecycle action fails, which
/// covers lock acquisition, the platform supervisor, and descriptor I/O.
pub async fn run(args: &WatchArgs, reporter: &mut impl Reporter) -> Result<i32> {
    emit_debounce_warning(reporter);

    if let Some(command) = &args.command {
        return run_lifecycle(command, args.json, reporter);
    }

    if args.foreground {
        patina_core::run_foreground(shutdown_signal())
            .await
            .context("foreground watcher failed")?;
        return Ok(ExitCode::Success.code());
    }

    // Neither a lifecycle subcommand nor `--foreground`: there is no default
    // action. Point the user at both modes.
    reporter.warn(
        "patina watch needs a mode: run `patina watch --foreground` to watch \
         inline, or `patina watch install` to register the background service",
    );
    Ok(ExitCode::Generic.code())
}

/// Run a background-service lifecycle subcommand.
///
/// Resolves the per-machine state directory, then acquires the advisory lock
/// the subcommand requires. That lock is exclusive for every mutating action,
/// and shared for the read-only `status`. It then drives the matching
/// [`patina_core::ServiceBackend`] method and renders the outcome. A
/// not-installed service is a no-op with a clear stderr message rather than a
/// supervisor error; an already-installed `install` exits 1 with a
/// typed error.
fn run_lifecycle(command: &WatchCommand, json: bool, reporter: &mut impl Reporter) -> Result<i32> {
    let state =
        patina_core::resolve_state_dir().context("failed to resolve the state directory")?;
    let backend = patina_core::current_service_backend(&state);
    let lock_path = state.join("lock");

    // `status` is read-only: it acquires the shared lock and, on a shared-lock
    // timeout, warns and proceeds without it (the read-only escape hatch,
    // matching `patina status`). Every other lifecycle action mutates the
    // service registration and acquires the exclusive lock, mapping a timeout
    // to exit code 4 via the error-chain funnel.
    if let WatchCommand::Status = command {
        let _guard = crate::cmd::shared_lock(&lock_path, false, reporter);
        return Ok(render_status(backend.status(), json, reporter));
    }

    let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .context("failed to acquire the exclusive lock for the watch lifecycle action")?;

    Ok(dispatch_lifecycle(
        backend.as_ref(),
        command,
        json,
        reporter,
    ))
}

/// Dispatch a mutating lifecycle subcommand to its backend method and render
/// the outcome, returning the process exit code.
///
/// Split from [`run_lifecycle`], which owns state-directory resolution and
/// lock acquisition, so the command→method routing is unit-testable against a
/// fake [`ServiceBackend`] with no real supervisor or lock. `status` is handled
/// by [`run_lifecycle`] before this is reached (it takes the shared lock and
/// returns early); the defensive `Status` arm here maps to a no-op.
fn dispatch_lifecycle(
    backend: &dyn ServiceBackend,
    command: &WatchCommand,
    json: bool,
    reporter: &mut impl Reporter,
) -> i32 {
    let result = match command {
        WatchCommand::Install => backend.install(),
        WatchCommand::Uninstall { .. } => backend.uninstall(),
        WatchCommand::Start => backend.start(),
        WatchCommand::Stop => backend.stop(),
        WatchCommand::Restart => backend.restart(),
        // `status` returned above, before the exclusive-lock acquisition, so it
        // never reaches this mutating branch; treat it as a no-op result.
        WatchCommand::Status => Ok(LifecycleResult::NotInstalled),
    };
    render_lifecycle(result, json, reporter)
}

/// Render a lifecycle action's outcome and return the process exit code.
///
/// On success it emits the `result` word (a JSON envelope under `--json`, a
/// human line otherwise) and returns `0`. A [`LifecycleResult::NotInstalled`]
/// is a no-op (no supervisor action, no mutation) that names the clear
/// "service not installed" message on stderr and exits `1` per the
/// behavior block. An error is surfaced through the reporter and returns `1`;
/// an already-installed `install` therefore exits 1 with its typed message.
fn render_lifecycle(
    result: std::result::Result<LifecycleResult, ServiceError>,
    json: bool,
    reporter: &mut impl Reporter,
) -> i32 {
    match result {
        Ok(LifecycleResult::NotInstalled) => {
            // No-op: not a spurious supervisor error, but the behavior block
            // signals exit 1 with this exact stderr message.
            reporter.warn("service not installed; run `patina watch install` first");
            if json {
                reporter.json(
                    &serde_json::json!({ "result": LifecycleResult::NotInstalled.label() })
                        .to_string(),
                );
            }
            ExitCode::Generic.code()
        }
        Ok(outcome) => {
            if json {
                reporter.json(&serde_json::json!({ "result": outcome.label() }).to_string());
            } else {
                reporter.line(&format!("watch service: {}", outcome.label()));
            }
            ExitCode::Success.code()
        }
        Err(error) => {
            reporter.warn(&error.to_string());
            ExitCode::Generic.code()
        }
    }
}

/// Render the `status` outcome and return the process exit code.
///
/// Emits the structured object under `--json` (`installed`, `running`,
/// `last_fired_at`, `last_exit_code`, `subscriptions_count`,
/// `re_applies_since_start`) or a human summary otherwise, and returns `0`. A
/// supervisor query failure is surfaced through the reporter and returns `1`.
fn render_status(
    status: std::result::Result<ServiceStatus, ServiceError>,
    json: bool,
    reporter: &mut impl Reporter,
) -> i32 {
    match status {
        Ok(status) => {
            if json {
                reporter.json(&status_envelope(&status));
            } else {
                render_status_human(&status, reporter);
            }
            ExitCode::Success.code()
        }
        Err(error) => {
            reporter.warn(&error.to_string());
            ExitCode::Generic.code()
        }
    }
}

/// Build the `status --json` envelope: the six fields,
/// with the recovered counters rendered as JSON `null` when absent.
fn status_envelope(status: &ServiceStatus) -> String {
    let envelope = serde_json::json!({
        "installed": status.installed,
        "running": status.running,
        "last_fired_at": status.last_fired_at,
        "last_exit_code": status.last_exit_code,
        "subscriptions_count": status.subscriptions_count,
        "re_applies_since_start": status.re_applies_since_start,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Render the human-readable `status` summary as one aligned row per field,
/// with `unknown` standing in for an absent recovered value.
///
/// The key keeps its trailing colon inside its own cell, so `installed:` and
/// `running:` stay single literal tokens for a script that greps them.
fn render_status_human(status: &ServiceStatus, reporter: &mut impl Reporter) {
    let styles = &reporter.styles();
    // Only `true` is painted. A machine that never installed the service reads
    // `false` on both fields, and the warn color would call that intended state
    // a fault.
    let liveness = |state: bool| {
        if state {
            paint(styles.success, "true")
        } else {
            "false".to_owned()
        }
    };
    let table: String = [
        ("installed:", liveness(status.installed)),
        ("running:", liveness(status.running)),
        (
            "last fired at:",
            recovered(status.last_fired_at.as_deref(), styles),
        ),
        ("last exit code:", recovered(status.last_exit_code, styles)),
        (
            "subscriptions:",
            recovered(status.subscriptions_count, styles),
        ),
        (
            "re-applies since start:",
            recovered(status.re_applies_since_start, styles),
        ),
    ]
    .iter()
    .map(|(key, value)| row(&[key, value.as_str()]))
    .collect();
    emit_aligned(&table, reporter);
}

/// A recovered field's value, or the literal `unknown` when it could not be
/// read. `unknown` takes the hint color. It marks the absence of a reading
/// rather than a reading of zero, and must not compete with the values around
/// it.
fn recovered<T: std::fmt::Display>(value: Option<T>, styles: &Styles) -> String {
    value.map_or_else(|| paint(styles.hint, "unknown"), |value| value.to_string())
}

/// Read the root manifest and, if it declares the ignored `[watcher]
/// debounce_ms` key, surface the typed warning.
///
/// Best-effort. A repository that cannot be discovered, or a manifest that
/// cannot be read, is not this warning's concern; the foreground start path
/// surfaces real discovery errors. A lookup miss is therefore silently
/// skipped.
fn emit_debounce_warning(reporter: &mut impl Reporter) {
    let Ok(repo_root) = patina_core::resolve_repository_root() else {
        return;
    };
    let manifest = repo_root.join("patina.toml");
    let Ok(text) = fs_err::read_to_string(manifest.as_std_path()) else {
        return;
    };
    if let Some(warning) = patina_core::watcher_config_warning(&text) {
        reporter.warn(&warning);
    }
}

/// The shutdown future for the foreground watcher: resolve on Ctrl-C
/// (SIGINT) on every platform, or, on POSIX, on SIGTERM, whichever arrives
/// first. A failure to install a handler resolves the future (shutting the
/// watcher down) rather than leaving it unstoppable.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::SignalKind;
        use tokio::signal::unix::signal;

        if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = sigterm.recv() => {}
            }
        } else {
            // Could not install the SIGTERM handler; fall back to Ctrl-C only
            // rather than leaving the watcher unstoppable.
            let _outcome = tokio::signal::ctrl_c().await;
        }
    }

    #[cfg(not(unix))]
    {
        let _outcome = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;
    use crate::output::reporter::assert_color_is_additive;
    use std::cell::RefCell;

    /// An in-memory [`ServiceBackend`] fake that records which method the
    /// dispatch called and returns a configured [`LifecycleResult`] from every
    /// mutating action. Recording the call proves routing without depending on
    /// the rendered label; the configured result drives the not-installed path.
    /// It performs no supervisor or filesystem I/O, so it runs on every CI OS
    /// where the real per-OS backends cannot.
    struct RecordingBackend {
        calls: RefCell<Vec<&'static str>>,
        result: LifecycleResult,
    }

    impl RecordingBackend {
        fn new(result: LifecycleResult) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                result,
            }
        }

        fn record(&self, method: &'static str) -> LifecycleResult {
            self.calls.borrow_mut().push(method);
            self.result
        }
    }

    impl ServiceBackend for RecordingBackend {
        fn install(&self) -> std::result::Result<LifecycleResult, ServiceError> {
            Ok(self.record("install"))
        }
        fn uninstall(&self) -> std::result::Result<LifecycleResult, ServiceError> {
            Ok(self.record("uninstall"))
        }
        fn start(&self) -> std::result::Result<LifecycleResult, ServiceError> {
            Ok(self.record("start"))
        }
        fn stop(&self) -> std::result::Result<LifecycleResult, ServiceError> {
            Ok(self.record("stop"))
        }
        fn restart(&self) -> std::result::Result<LifecycleResult, ServiceError> {
            Ok(self.record("restart"))
        }
        fn status(&self) -> std::result::Result<ServiceStatus, ServiceError> {
            self.calls.borrow_mut().push("status");
            Ok(ServiceStatus {
                installed: true,
                running: true,
                last_fired_at: None,
                last_exit_code: Some(0),
                subscriptions_count: Some(3),
                re_applies_since_start: Some(1),
            })
        }
    }

    #[test]
    fn dispatch_routes_each_subcommand_to_its_backend_method() {
        // The recorded call proves which backend method the dispatch invoked, so
        // a miswired match arm (e.g. Restart calling stop) fails here. `status`
        // dispatches separately (it takes the shared lock), so it is not part of
        // this mutating path.
        let cases = [
            (WatchCommand::Install, "install"),
            (WatchCommand::Uninstall { yes: true }, "uninstall"),
            (WatchCommand::Start, "start"),
            (WatchCommand::Stop, "stop"),
            (WatchCommand::Restart, "restart"),
        ];
        for (command, expected_method) in cases {
            let backend = RecordingBackend::new(LifecycleResult::Installed);
            let mut reporter = BufferReporter::new();
            let code = dispatch_lifecycle(&backend, &command, true, &mut reporter);
            assert_eq!(code, ExitCode::Success.code(), "{command:?} must exit 0");
            assert_eq!(
                backend.calls.borrow().as_slice(),
                &[expected_method],
                "{command:?} must route to {expected_method}"
            );
        }
    }

    #[test]
    fn dispatch_status_arm_is_a_defensive_no_op_that_never_queries_the_backend() {
        // `status` is handled by `run_lifecycle` before dispatch (it takes the
        // shared lock and returns early), so the `Status` arm here is defensive:
        // it maps to NotInstalled → exit 1 without querying the backend.
        let backend = RecordingBackend::new(LifecycleResult::Installed);
        let mut reporter = BufferReporter::new();
        let code = dispatch_lifecycle(&backend, &WatchCommand::Status, false, &mut reporter);
        assert_eq!(code, ExitCode::Generic.code());
        assert!(
            reporter.err.contains("service not installed"),
            "the defensive Status arm must surface the not-installed message, got: {}",
            reporter.err
        );
        assert!(
            backend.calls.borrow().is_empty(),
            "the defensive Status arm must not call any backend method"
        );
    }

    #[test]
    fn dispatch_on_a_not_installed_service_warns_and_exits_one() {
        // A lifecycle action whose backend reports the service is not installed
        // is a no-op: dispatch surfaces the "service not installed" message and
        // exits 1 rather than reporting a spurious success.
        let backend = RecordingBackend::new(LifecycleResult::NotInstalled);
        let mut reporter = BufferReporter::new();
        let code = dispatch_lifecycle(&backend, &WatchCommand::Start, false, &mut reporter);
        assert_eq!(code, ExitCode::Generic.code());
        assert!(
            reporter
                .err
                .contains("service not installed; run `patina watch install` first"),
            "stderr must carry the not-installed message, got: {}",
            reporter.err
        );
    }

    #[test]
    fn render_status_emits_the_backend_status_in_both_modes() {
        // Exercises `render_status` (and the backend's `status`) directly: the
        // human path names each field, the JSON path emits the structured
        // object. Both exit 0.
        let backend = RecordingBackend::new(LifecycleResult::Installed);

        let mut human = BufferReporter::new();
        let code = render_status(backend.status(), false, &mut human);
        assert_eq!(code, ExitCode::Success.code());
        assert!(
            human.out.contains("installed:") && human.out.contains("true"),
            "human status must name the installed field, got: {}",
            human.out
        );

        let mut json = BufferReporter::new();
        let code = render_status(backend.status(), true, &mut json);
        assert_eq!(code, ExitCode::Success.code());
        let doc: serde_json::Value =
            serde_json::from_str(json.out.trim()).expect("status --json is one JSON doc");
        assert_eq!(doc.get("installed"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(doc.get("subscriptions_count"), Some(&serde_json::json!(3)));
    }

    /// A mix of read and unread fields, so both the liveness color and the
    /// `unknown` stand-in are exercised.
    fn mixed_status() -> ServiceStatus {
        ServiceStatus {
            installed: true,
            running: false,
            last_fired_at: None,
            last_exit_code: Some(0),
            subscriptions_count: Some(3),
            re_applies_since_start: None,
        }
    }

    #[test]
    fn human_status_color_strips_back_to_the_plain_summary() {
        let status = mixed_status();
        assert_color_is_additive(|reporter| {
            render_status_human(&status, reporter);
        });
    }

    #[test]
    fn status_envelope_carries_the_six_fields_with_null_for_absent_counters() {
        // The JSON object names all six fields, and an
        // absent recovered counter renders as JSON null rather than being
        // dropped.
        let status = ServiceStatus {
            installed: true,
            running: false,
            last_fired_at: None,
            last_exit_code: Some(0),
            subscriptions_count: None,
            re_applies_since_start: None,
        };
        let doc: serde_json::Value =
            serde_json::from_str(&status_envelope(&status)).expect("envelope is valid JSON");
        assert_eq!(doc.get("installed"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(doc.get("running"), Some(&serde_json::Value::Bool(false)));
        assert_eq!(doc.get("last_exit_code"), Some(&serde_json::json!(0)));
        assert_eq!(
            doc.get("subscriptions_count"),
            Some(&serde_json::Value::Null)
        );
        assert!(
            doc.get("re_applies_since_start")
                .is_some_and(serde_json::Value::is_null)
        );
    }

    #[test]
    fn render_lifecycle_not_installed_warns_and_exits_one() {
        // The behavior block: a lifecycle action on a not-installed service
        // names the clear "service not installed" message on stderr and exits
        // 1 (a no-op, not a spurious supervisor error).
        let mut reporter = BufferReporter::new();
        let code = render_lifecycle(Ok(LifecycleResult::NotInstalled), false, &mut reporter);
        assert_eq!(code, ExitCode::Generic.code());
        assert!(
            reporter
                .err
                .contains("service not installed; run `patina watch install` first"),
            "stderr must carry the exact not-installed message, got: {}",
            reporter.err
        );
    }

    #[test]
    fn render_lifecycle_already_installed_error_exits_one() {
        // Install on an already-installed service exits 1 with the
        // typed message surfaced to stderr.
        let mut reporter = BufferReporter::new();
        let code = render_lifecycle(Err(ServiceError::AlreadyInstalled), true, &mut reporter);
        assert_eq!(code, ExitCode::Generic.code());
        assert!(
            reporter.err.contains("already installed"),
            "stderr must carry the already-installed message, got: {}",
            reporter.err
        );
    }

    #[test]
    fn render_lifecycle_success_emits_the_result_word() {
        let mut reporter = BufferReporter::new();
        let code = render_lifecycle(Ok(LifecycleResult::Installed), true, &mut reporter);
        assert_eq!(code, ExitCode::Success.code());
        let doc: serde_json::Value =
            serde_json::from_str(reporter.out.trim()).expect("one JSON doc on stdout");
        assert_eq!(doc.get("result"), Some(&serde_json::json!("installed")));
    }
}
