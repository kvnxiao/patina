//! `patina status` command logic.
//!
//! Classifies every managed target as CLEAN / DRIFTED / MISSING / ORPHANED
//! against the last committed apply. Renders the result as a human-readable
//! table by default, and as a JSON envelope under `--json`. The journal read,
//! the current-plan recomputation, the classification, and the shared lock all
//! live in `patina_core::status`; this module is presentation and control flow.
//!
//! `status` does not write, and exits 0 on any successful read. A shared-lock
//! timeout reaches the user as a stderr warning (the read-only escape hatch)
//! and leaves the exit code alone.

use crate::cli::StatusArgs;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use crate::output::style::Styles;
use crate::output::style::paint;
use crate::output::table::emit_aligned;
use crate::output::table::row;
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
/// read error).
pub async fn run(args: &StatusArgs, reporter: &mut impl Reporter) -> Result<i32> {
    let report = patina_core::status(StatusOptions::default())
        .await
        .context("failed to compute status")?;

    // Every warning, lock timeout included, goes to stderr in both output
    // formats, so stdout stays a single parseable JSON document.
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

/// Build the `--json` envelope: `last_apply`, `files`, the `clean` /
/// `drifted` / `missing` / `orphaned` counters, and `remotes_pending`.
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
        "remotes_pending": report.remotes_pending,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Render the human-readable table: one row per target, then a summary line of
/// the counters, then the pending-remote reminder.
fn render_human(report: &StatusReport, reporter: &mut impl Reporter) {
    if report.last_apply.is_none() {
        reporter.line("No apply has been recorded yet; nothing to report.");
        render_remotes_pending(report, reporter);
        return;
    }
    let styles = reporter.styles();
    let table: String = report
        .files
        .iter()
        .map(|entry| {
            row(&[
                paint(state_style(entry.state, &styles), state_label(entry.state)).as_str(),
                entry.path.as_str(),
            ])
        })
        .collect();
    emit_aligned(&table, reporter);
    reporter.line(&render_summary(report, &styles));
    render_remotes_pending(report, reporter);
}

/// The clean / drifted / missing / orphaned counters on one line, each
/// non-zero counter painted in its state's color.
///
/// A zero counter stays plain. Painting it would spend the state's color on
/// the absence of that state, and a clean repository has to read at a glance.
fn render_summary(report: &StatusReport, styles: &Styles) -> String {
    [
        (TargetState::Clean, report.clean),
        (TargetState::Drifted, report.drifted),
        (TargetState::Missing, report.missing),
        (TargetState::Orphaned, report.orphaned),
    ]
    .map(|(state, count)| {
        let counter = format!("{}: {count}", state_label(state));
        if count == 0 {
            counter
        } else {
            paint(state_style(state, styles), &counter)
        }
    })
    .join("  ")
}

/// The palette role for a target state.
fn state_style(state: TargetState, styles: &Styles) -> anstyle::Style {
    match state {
        TargetState::Clean => styles.status.clean,
        TargetState::Drifted => styles.status.drifted,
        TargetState::Missing => styles.status.missing,
        TargetState::Orphaned => styles.status.orphaned,
    }
}

/// Report the remotes whose upstream has moved past their pin, as of the last
/// `patina remote check`. The wording is the notice subsystem's own, so status
/// and the shell notice can never phrase the same fact two ways.
fn render_remotes_pending(report: &StatusReport, reporter: &mut impl Reporter) {
    if report.remotes_pending.is_empty() {
        return;
    }
    let names: Vec<&str> = report.remotes_pending.iter().map(String::as_str).collect();
    reporter.line(patina_core::remote::notice::pending_updates_message(&names).trim_end());
}

/// Stable lowercase label for a target state, shared by both renderers.
fn state_label(state: TargetState) -> &'static str {
    state.label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;
    use crate::output::reporter::assert_color_is_additive;
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

    /// A report with one target in each state.
    fn report_of_every_state() -> StatusReport {
        let mut report = report_with_entries();
        for (path, state) in [
            ("/home/u/.zshrc", TargetState::Clean),
            ("/home/u/.config/nvim/init.lua", TargetState::Missing),
            ("/home/u/.oldrc", TargetState::Orphaned),
        ] {
            report.files.push(StatusEntry {
                path: Utf8PathBuf::from(path),
                state,
            });
        }
        report.clean = 1;
        report.missing = 1;
        report.orphaned = 1;
        report
    }

    /// A stripped render shows the state word and nothing else, so each row
    /// must lead with its own label.
    #[test]
    fn every_state_leads_its_row_with_its_own_label() {
        let mut r = BufferReporter::new();
        render_human(&report_of_every_state(), &mut r);

        let rows: Vec<&str> = r.out.lines().take(4).collect();
        for (row, label) in rows.iter().zip(["drifted", "clean", "missing", "orphaned"]) {
            assert!(row.starts_with(label), "{row:?} must lead with {label}");
        }
    }

    /// A non-zero counter is painted so a clean repository reads at a glance; a
    /// zero counter stays plain so the color marks a state that is present, not
    /// one that is merely named.
    #[test]
    fn only_a_non_zero_counter_is_painted() {
        let report = StatusReport {
            clean: 2,
            ..StatusReport::default()
        };
        let summary = render_summary(&report, &Styles::colored());

        let clean = paint(Styles::colored().status.clean, "clean: 2");
        assert!(
            summary.contains(&clean),
            "a non-zero counter must be painted whole: {summary:?}"
        );
        assert!(
            summary.contains("drifted: 0")
                && !summary.contains(&paint(Styles::colored().status.drifted, "drifted: 0")),
            "a zero counter must stay plain: {summary:?}"
        );
    }

    #[test]
    fn human_render_color_strips_back_to_the_plain_table() {
        let report = report_of_every_state();
        assert_color_is_additive(|reporter| render_human(&report, reporter));
    }

    #[test]
    fn pending_remotes_reach_stdout_in_both_renderers() {
        let mut report = report_with_entries();
        report.remotes_pending = vec!["humanizer".to_owned(), "prompts".to_owned()];

        let mut r = BufferReporter::new();
        render_human(&report, &mut r);
        assert!(
            r.out.contains("humanizer") && r.out.contains("prompts"),
            "the human render must name every pending remote: {}",
            r.out
        );

        let doc: serde_json::Value =
            serde_json::from_str(&json_envelope(&report)).expect("valid JSON");
        assert_eq!(
            doc.get("remotes_pending")
                .and_then(serde_json::Value::as_array)
                .map(|names| names.iter().filter_map(serde_json::Value::as_str).collect()),
            Some(vec!["humanizer", "prompts"]),
            "the envelope must carry the whole pending set, in order: {doc}"
        );
    }

    #[test]
    fn pending_remotes_are_reported_before_the_first_apply() {
        // The pre-apply early return is a separate path through `render_human`,
        // and the reminder matters most on a fresh machine with a pending pin.
        let report = StatusReport {
            remotes_pending: vec!["humanizer".to_owned()],
            ..StatusReport::default()
        };
        let mut r = BufferReporter::new();
        render_human(&report, &mut r);
        assert!(
            r.out.contains("No apply has been recorded") && r.out.contains("humanizer"),
            "a repository with no apply must still report its pending remotes: {}",
            r.out
        );
    }

    #[test]
    fn an_empty_pending_set_adds_no_line() {
        // Counting lines rather than matching the notice wording: the wording
        // belongs to the notice subsystem and may change, while the
        // one-line-or-nothing shape is this renderer's own.
        let mut quiet = BufferReporter::new();
        render_human(&report_with_entries(), &mut quiet);

        let mut report = report_with_entries();
        report.remotes_pending = vec!["humanizer".to_owned()];
        let mut noisy = BufferReporter::new();
        render_human(&report, &mut noisy);

        assert_eq!(
            noisy.out.lines().count(),
            quiet.out.lines().count() + 1,
            "the pending set must add exactly one line, and none when it is empty:\n{}\n---\n{}",
            quiet.out,
            noisy.out
        );
    }
}
