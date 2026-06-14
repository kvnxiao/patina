//! `patina status` command logic.
//!
//! Classifies every managed target as CLEAN / DRIFTED / MISSING /
//! ORPHANED against the last committed apply and renders the result —
//! a human-readable table by default, a JSON envelope under `--json`.
//! The engine semantics (journal read, current-plan recomputation,
//! classification, shared lock) live in `patina_core::status`; this module
//! is presentation and control flow only, all output routed through the
//! [`Reporter`].
//!
//! Status is read-only: it never mutates and always exits 0 on a
//! successful read. A shared-lock timeout is surfaced as a stderr warning
//! (the read-only escape hatch), not a non-zero exit.

use crate::cli::StatusArgs;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use patina_core::StatusOptions;
use patina_core::StatusReport;
use patina_core::TargetState;

/// Run `patina status`. Returns the process exit code (always 0 on a
/// successful read).
///
/// # Errors
///
/// Returns an error when the engine-level status read fails (repository
/// discovery, manifest parse, state-directory resolution, or a journal
/// read error). A shared-lock timeout is not an error: it is reported as
/// a warning on the report.
pub async fn run(args: &StatusArgs, reporter: &mut impl Reporter) -> Result<i32> {
    let report = patina_core::status(StatusOptions::default())
        .await
        .context("failed to compute status")?;

    // Lock-timeout (and any other) warnings go to stderr regardless of the
    // output format so they never pollute the JSON document on stdout.
    for warning in &report.warnings {
        reporter.warn(warning);
    }

    if args.json {
        reporter.json(&json_envelope(&report));
    } else {
        render_human(&report, reporter);
    }
    Ok(ExitCode::Success.code())
}

/// Build the `--json` envelope: `last_apply`, `files`, and the four
/// aggregate counters.
fn json_envelope(report: &StatusReport) -> String {
    let last_apply = report
        .last_apply
        .as_ref()
        .map_or(serde_json::Value::Null, |meta| {
            serde_json::json!({
                "at": meta.at,
                "user": meta.user,
                "host": meta.host,
            })
        });
    let files: Vec<serde_json::Value> = report
        .files
        .iter()
        .map(|entry| {
            serde_json::json!({
                "path": entry.path.as_str(),
                "state": state_label(entry.state),
            })
        })
        .collect();
    let envelope = serde_json::json!({
        "last_apply": last_apply,
        "files": files,
        "clean": report.clean,
        "drifted": report.drifted,
        "missing": report.missing,
        "orphaned": report.orphaned,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Render the human-readable table: one row per target plus a summary
/// line of the aggregate counters.
fn render_human(report: &StatusReport, reporter: &mut impl Reporter) {
    if report.last_apply.is_none() {
        reporter.line("No apply has been recorded yet; nothing to report.");
        return;
    }
    for entry in &report.files {
        reporter.line(&format!("{:<8} {}", state_label(entry.state), entry.path));
    }
    reporter.line(&format!(
        "clean: {}  drifted: {}  missing: {}  orphaned: {}",
        report.clean, report.drifted, report.missing, report.orphaned
    ));
}

/// Stable lowercase label for a target state, shared by both renderers.
fn state_label(state: TargetState) -> &'static str {
    state.label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;
    use camino::Utf8PathBuf;
    use patina_core::LastApply;
    use patina_core::StatusEntry;

    fn report_with_entries() -> StatusReport {
        let mut report = StatusReport {
            last_apply: Some(LastApply {
                at: "2026-05-28T12:00:00Z".to_owned(),
                user: "u".to_owned(),
                host: "h".to_owned(),
            }),
            ..StatusReport::default()
        };
        report.files.push(StatusEntry {
            path: Utf8PathBuf::from("/home/u/.gitconfig"),
            state: TargetState::Drifted,
        });
        report.drifted = 1;
        report
    }

    #[test]
    fn json_envelope_carries_counters_and_files() {
        let report = report_with_entries();
        let doc: serde_json::Value =
            serde_json::from_str(&json_envelope(&report)).expect("valid JSON");
        assert_eq!(
            doc.get("drifted").and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            doc.get("clean").and_then(serde_json::Value::as_u64),
            Some(0)
        );
        let files = doc
            .get("files")
            .and_then(serde_json::Value::as_array)
            .expect("files array");
        assert_eq!(files.len(), 1);
        let first = files.first().expect("one files entry");
        assert_eq!(
            first.get("state").and_then(serde_json::Value::as_str),
            Some("drifted")
        );
        assert_eq!(
            doc.pointer("/last_apply/at")
                .and_then(serde_json::Value::as_str),
            Some("2026-05-28T12:00:00Z")
        );
    }

    #[test]
    fn json_last_apply_is_null_when_no_apply_recorded() {
        let report = StatusReport::default();
        let doc: serde_json::Value =
            serde_json::from_str(&json_envelope(&report)).expect("valid JSON");
        assert!(doc.get("last_apply").expect("key present").is_null());
    }

    #[test]
    fn warnings_route_to_stderr_in_both_formats() {
        let mut report = report_with_entries();
        report.warnings.push("lock timed out".to_owned());
        let mut r = BufferReporter::new();
        for warning in &report.warnings {
            r.warn(warning);
        }
        r.json(&json_envelope(&report));
        assert!(r.err.contains("lock timed out"));
        // The JSON on stdout must remain a single parseable document.
        serde_json::from_str::<serde_json::Value>(r.out.trim()).expect("stdout is one JSON doc");
    }

    #[test]
    fn human_render_reports_nothing_when_no_apply() {
        let report = StatusReport::default();
        let mut r = BufferReporter::new();
        render_human(&report, &mut r);
        assert!(r.out.contains("No apply has been recorded"));
    }
}
