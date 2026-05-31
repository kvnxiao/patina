//! `patina watch` command logic (REQ-004 / REQ-006).
//!
//! In this slice the command supports `--foreground` only: it runs the
//! watcher loop inline ([`patina_core::run_foreground`]), attached to the
//! invoking shell, until Ctrl-C (SIGINT) or — on POSIX — SIGTERM shuts it
//! down (DEC-011). The engine semantics (state-dir resolution, log stack,
//! subscription set, debounce loop) live in `patina_core::watch`; this module
//! is control flow and the shutdown-signal wiring only.
//!
//! Before starting, the command surfaces the forward-compatible-but-ignored
//! `[watcher] debounce_ms` warning (REQ-006 / DEC-002) through the reporter.
//! The background-service install lands in a later task; without
//! `--foreground` the command reports that and exits non-zero.

use crate::cli::WatchArgs;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;

/// Run `patina watch`. Returns the process exit code.
///
/// With `--foreground`, runs the watcher loop until a shutdown signal arrives
/// and returns `0` on a clean exit. Without `--foreground`, reports that the
/// background-service install is not yet wired and returns a non-zero code.
///
/// # Errors
///
/// Returns an error when the repository root cannot be resolved or the
/// foreground watcher fails to start or run (state-directory resolution, log
/// appender, journal read, or watcher arming).
pub async fn run(args: &WatchArgs, reporter: &mut impl Reporter) -> Result<i32> {
    emit_debounce_warning(reporter);

    if !args.foreground {
        // The per-OS background-service install is a later task; only the
        // inline `--foreground` mode is wired in this slice.
        reporter.warn(
            "patina watch currently supports only --foreground; the background \
             service install is not yet available",
        );
        return Ok(ExitCode::Generic.code());
    }

    patina_core::run_foreground(shutdown_signal())
        .await
        .context("foreground watcher failed")?;
    Ok(ExitCode::Success.code())
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
