//! `patina watch` command logic (REQ-001 / REQ-003 / REQ-004 / REQ-006).
//!
//! The command has two modes. `--foreground` runs the watcher loop inline
//! ([`patina_core::run_foreground`]), attached to the invoking shell, until
//! Ctrl-C (SIGINT) or — on POSIX — SIGTERM shuts it down (DEC-011). The
//! lifecycle subcommands (`install` / `uninstall` / `start` / `stop` /
//! `restart` / `status`) manage the per-OS background service through the
//! [`patina_core::watch::service`] backend (REQ-001 / REQ-003).
//!
//! All lifecycle subcommands except `status` acquire the exclusive advisory
//! lock (SPEC-0001 REQ-023); `status` acquires the shared lock. The engine
//! semantics (state-dir resolution, the service backend, log-counter recovery)
//! live in `patina_core`; this module is control flow, lock acquisition, and
//! output formatting only, all routed through the [`Reporter`].
//!
//! Before starting any mode, the command surfaces the forward-compatible-but-
//! ignored `[watcher] debounce_ms` warning (REQ-006 / DEC-002) through the
//! reporter.

use crate::cli::WatchArgs;
use crate::cli::WatchCommand;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use patina_core::LifecycleResult;
use patina_core::LockKind;
use patina_core::SHARED_TIMEOUT;
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
/// Returns an error when the foreground watcher fails to start or run
/// (state-directory resolution, log appender, journal read, or watcher
/// arming), or when a lifecycle action fails (lock acquisition, the platform
/// supervisor, or descriptor I/O).
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

/// Run a background-service lifecycle subcommand (REQ-001 / REQ-003).
///
/// Resolves the per-machine state directory, acquires the advisory lock the
/// subcommand requires (exclusive for every mutating action, shared for the
/// read-only `status`), then drives the matching
/// [`patina_core::ServiceBackend`] method and renders the outcome. A
/// not-installed service is a no-op with a clear stderr message rather than a
/// supervisor error (REQ-003); an already-installed `install` exits 1 with a
/// typed error.
fn run_lifecycle(command: &WatchCommand, json: bool, reporter: &mut impl Reporter) -> Result<i32> {
    let state =
        patina_core::resolve_state_dir().context("failed to resolve the state directory")?;
    let backend = patina_core::current_service_backend(&state);
    let lock_path = state.join("lock");

    // `status` is read-only: it acquires the shared lock and, on a shared-lock
    // timeout, warns and proceeds without it (REQ-023's read-only escape hatch,
    // matching `patina status`). Every other lifecycle action mutates the
    // service registration and acquires the exclusive lock, mapping a timeout
    // to exit code 4 via the error-chain funnel (SPEC-0001 REQ-023).
    if let WatchCommand::Status = command {
        let _guard = match acquire_lock(&lock_path, LockKind::Shared, SHARED_TIMEOUT) {
            Ok(guard) => Some(guard),
            Err(error) => {
                reporter.warn(&format!("proceeding without the shared lock: {error}"));
                None
            }
        };
        return Ok(render_status(backend.status(), json, reporter));
    }

    let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .context("failed to acquire the exclusive lock for the watch lifecycle action")?;

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
    Ok(render_lifecycle(result, json, reporter))
}

/// Render a lifecycle action's outcome and return the process exit code.
///
/// On success it emits the `result` word (a JSON envelope under `--json`, a
/// human line otherwise) and returns `0`. A [`LifecycleResult::NotInstalled`]
/// is a no-op (no supervisor action, no mutation) that names the clear
/// "service not installed" message on stderr and exits `1` per REQ-003's
/// behavior block. An error is surfaced through the reporter and returns `1`;
/// an already-installed `install` therefore exits 1 with its typed message
/// (REQ-001).
fn render_lifecycle(
    result: std::result::Result<LifecycleResult, ServiceError>,
    json: bool,
    reporter: &mut impl Reporter,
) -> i32 {
    match result {
        Ok(LifecycleResult::NotInstalled) => {
            // No-op: not a spurious supervisor error, but the behavior block
            // (REQ-003) signals exit 1 with this exact stderr message.
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

/// Build the `status --json` envelope (REQ-003 `<done-when>`): the six fields,
/// with the recovered counters rendered as JSON `null` when absent (DEC-012).
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

/// Render the human-readable `status` summary: one line per field, with
/// `unknown` standing in for an absent recovered value.
fn render_status_human(status: &ServiceStatus, reporter: &mut impl Reporter) {
    reporter.line(&format!("installed:              {}", status.installed));
    reporter.line(&format!("running:                {}", status.running));
    reporter.line(&format!(
        "last fired at:          {}",
        status.last_fired_at.as_deref().unwrap_or("unknown")
    ));
    reporter.line(&format!(
        "last exit code:         {}",
        opt_to_string(status.last_exit_code)
    ));
    reporter.line(&format!(
        "subscriptions:          {}",
        opt_to_string(status.subscriptions_count)
    ));
    reporter.line(&format!(
        "re-applies since start: {}",
        opt_to_string(status.re_applies_since_start)
    ));
}

/// Render an optional numeric field as its value or the literal `unknown`.
fn opt_to_string<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |v| format!("{v}"))
}

/// Read the root manifest and, if it declares the ignored `[watcher]
/// debounce_ms` key, surface the typed warning (REQ-006 / DEC-002).
///
/// Best-effort: a repository that cannot be discovered or a manifest that
/// cannot be read is not this warning's concern (the foreground start path
/// surfaces real discovery errors), so a lookup miss is silently skipped.
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

/// The shutdown future for the foreground watcher (DEC-011): resolve on Ctrl-C
/// (SIGINT) on every platform, or — on POSIX — on SIGTERM, whichever arrives
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

    #[test]
    fn status_envelope_carries_the_six_fields_with_null_for_absent_counters() {
        // REQ-003 / DEC-012: the JSON object names all six fields, and an
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
        // REQ-003 behavior block: a lifecycle action on a not-installed service
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
        // REQ-001: install on an already-installed service exits 1 with the
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
