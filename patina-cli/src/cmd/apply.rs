//! `patina apply` command logic (REQ-017).
//!
//! This module owns the decision tree REQ-017 specifies — TTY prompt vs
//! non-TTY preview, `--yes`, `--json`, `--pager` — and maps the engine's
//! [`ApplyResult`] onto the process exit code. The engine semantics
//! (planning, journaling, executors, hooks, rollback) live in
//! `patina_core`; this module is presentation and control flow only, all
//! output routed through the [`Reporter`].
//!
//! ## Exit codes (REQ-017 / CHK-028..030)
//!
//! | Outcome                                   | Code |
//! |-------------------------------------------|------|
//! | Applied, previewed, or user-confirmed     | 0    |
//! | `pre_apply` `must_succeed` hook failed    | 2    |
//! | `post_apply` `must_succeed` hook → rollback | 3  |
//! | User declined the prompt                  | 5    |

use crate::cli::ApplyArgs;
use crate::cli::Pager;
use crate::exit_code::ExitCode;
use crate::output::diff;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use patina_core::ApplyRequest;
use patina_core::ApplyResult;
#[cfg(windows)]
use patina_core::DEV_MODE_REGISTRY_PATH;
use patina_core::ForceDeploy;
use patina_core::GateDecision;
use patina_core::HostDevModeProbe;
use patina_core::LockPolicy;
use patina_core::ResolvedPlan;
use patina_core::current_timestamp;
use patina_core::decide_symlink_gate;
use patina_core::execute_plan;
use patina_core::plan_apply;
use patina_core::plan_is_full_noop;

/// Whether the invoking process is attached to an interactive terminal.
/// Injected so the TTY decision is unit-testable without a real tty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tty {
    /// stdin is a terminal; `patina apply` (no `--yes`) prompts.
    Interactive,
    /// stdin is not a terminal; `patina apply` (no `--yes`) previews.
    NonInteractive,
}

/// A reader for the confirmation prompt line. Injected so the prompt path
/// is testable; production reads one line from stdin.
pub trait PromptReader {
    /// Read one response line. `None` on EOF.
    fn read_line(&mut self) -> Option<String>;
}

/// Production prompt reader: one line from stdin.
pub struct StdinReader;

impl PromptReader for StdinReader {
    fn read_line(&mut self) -> Option<String> {
        let mut buf = String::new();
        match std::io::stdin().read_line(&mut buf) {
            Ok(n) if n > 0 => Some(buf),
            // EOF (Ok(0)) or a read error: no response.
            _ => None,
        }
    }
}

/// Run `patina apply`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error when planning or execution fails at the engine level
/// (a real IO / discovery / parse failure). A failed `must_succeed` hook
/// or a declined prompt is *not* an error: it maps to a non-zero exit
/// code via the returned `i32`.
pub async fn run(
    args: &ApplyArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let request = build_request(args)?;
    let timestamp = current_timestamp();
    let resolved = plan_apply(&request, timestamp).context("failed to compute the apply plan")?;

    if args.json {
        return run_json(&resolved, &request, args.yes, reporter).await;
    }

    // Full no-op probe (REQ-009): a fully-satisfied repo with nothing to reap
    // shows no diff and no confirmation prompt, and reads no stdin. The engine
    // re-checks this under the held lock; this probe only governs the
    // prompt-skip. `execute_plan` then writes nothing (REQ-007) and reports
    // the deterministic up-to-date line (REQ-008).
    let is_full_noop =
        plan_is_full_noop(&resolved).context("failed to determine apply plan state")?;

    // The diff render and prompt belong to the human review path only; a full
    // no-op skips both and proceeds straight to the (no-op) execute below.
    if !is_full_noop {
        let rendered = render_diff(&resolved, args.pager, reporter)?;
        reporter.diff(&rendered);
    }

    match confirm_apply(is_full_noop, args.yes, tty, reader, reporter) {
        Confirmation::Proceed => {}
        Confirmation::PreviewOnly => return Ok(ExitCode::Success.code()),
        Confirmation::Declined => return Ok(ExitCode::UserDeclined.code()),
    }

    // Windows-only Developer Mode gate (REQ-007): if the plan creates
    // symbolic links and Developer Mode is off (and we are not elevated),
    // drive the one-time UAC elevation flow before mutating. On macOS /
    // Linux this is always `Proceed`.
    if let Some(exit) = drive_dev_mode_gate(&resolved, reporter)? {
        return Ok(exit);
    }

    let result = execute_plan(&resolved, &request, LockPolicy::Blocking)
        .await
        .context("apply execution failed")?;
    report_result(&result, reporter);
    Ok(exit_code_for(&result))
}

/// The confirmation decision for the human apply path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    /// Mutate: `--yes`, an interactive `y`, or a full no-op (which writes
    /// nothing regardless).
    Proceed,
    /// Non-TTY without `--yes`: the diff was previewed; exit 0 with no writes.
    PreviewOnly,
    /// Interactive prompt answered with anything other than `y`/`Y`.
    Declined,
}

/// Decide whether to proceed with the apply, prompting the user only on the
/// interactive review path (REQ-009).
///
/// A full no-op (`is_full_noop`) proceeds **without** touching the reporter's
/// prompt or reading a line from `reader`: the short-circuit precedes the
/// diff-and-prompt branch, so a fully-satisfied repo is never asked to
/// confirm and `execute_plan` then writes nothing. This is the core REQ-009
/// observable — the interactive-TTY prompt-skip — and is exercised directly
/// by the unit tests below with a recording reader that counts stdin reads.
fn confirm_apply(
    is_full_noop: bool,
    yes: bool,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Confirmation {
    if is_full_noop {
        // No diff, no prompt, no stdin read: the no-op writes nothing whether
        // or not the user would have confirmed (REQ-009).
        return Confirmation::Proceed;
    }
    match (yes, tty) {
        (true, _) => Confirmation::Proceed,
        // Non-TTY without --yes: preview only, exit 0 (CHK-028).
        (false, Tty::NonInteractive) => Confirmation::PreviewOnly,
        (false, Tty::Interactive) => {
            reporter.prompt("Apply? [y/N] ");
            let answer = reader.read_line().unwrap_or_default();
            if matches!(answer.trim(), "y" | "Y") {
                Confirmation::Proceed
            } else {
                Confirmation::Declined
            }
        }
    }
}

/// Drive the Windows Developer Mode symlink-elevation gate (REQ-007).
///
/// Returns `Ok(None)` when the apply may proceed (no symlink in the plan,
/// not on Windows, Developer Mode already on, or the helper just enabled
/// it). Returns `Ok(Some(code))` to short-circuit the command with a
/// terminal exit code: `5` when the user declines the UAC consent dialog,
/// honouring CHK-012's stderr-substring contract. Returns `Err` when the
/// helper ran but the flag still reads off afterward (exit 1, REQ-007).
///
/// On macOS / Linux [`decide_symlink_gate`] reports `Proceed` (the probe is
/// `NotWindows`), so this never reads the registry and never spawns the
/// helper — proving the cross-platform guarantee.
fn drive_dev_mode_gate(
    resolved: &ResolvedPlan,
    reporter: &mut impl Reporter,
) -> Result<Option<i32>> {
    match decide_symlink_gate(resolved, &HostDevModeProbe::default()) {
        GateDecision::Proceed => Ok(None),
        GateDecision::ProceedElevatedWarning => {
            reporter.warn(
                "Patina is running elevated; prefer enabling Developer Mode \
                 (`patina doctor --fix`) and running unelevated",
            );
            Ok(None)
        }
        GateDecision::RequireElevation => drive_elevation(reporter),
    }
}

/// Launch the one-time UAC helper and map its outcome to the command's
/// control flow. Split out so the `#[cfg(windows)]` launch is isolated from
/// the cross-platform gate decision above.
#[cfg(windows)]
fn drive_elevation(reporter: &mut impl Reporter) -> Result<Option<i32>> {
    reporter.line(
        "Developer Mode is required to create symbolic links. \
         Requesting one-time elevation…",
    );
    match patina_core::launch_elevate_helper().context("failed to launch the elevation helper")? {
        patina_core::ElevationOutcome::EnabledNow => Ok(None),
        patina_core::ElevationOutcome::Declined => {
            // CHK-012: stderr must name `Developer Mode` and
            // `patina doctor --fix`; exit 5 (user declined).
            reporter.warn(
                "Developer Mode was not enabled (elevation declined). \
                 Run `patina doctor --fix` to enable it, then re-run \
                 `patina apply`.",
            );
            Ok(Some(ExitCode::UserDeclined.code()))
        }
        patina_core::ElevationOutcome::RanButStillDisabled => Err(anyhow!(
            "the elevation helper ran but Developer Mode is still disabled; \
             the registry value {DEV_MODE_REGISTRY_PATH} did not change to 1"
        )),
    }
}

/// Non-Windows builds never reach a `RequireElevation` verdict (the probe
/// reports `NotWindows`), so this arm is unreachable in practice; it exists
/// only so the cross-platform gate compiles without a `#[cfg]` at the call
/// site. The `_reporter` is unused here.
// The `Result` is never an `Err` on this platform, but the return type must
// match the Windows variant above (which genuinely is fallible) so the
// single call site compiles on every platform — hence the allow.
#[cfg(not(windows))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature parity with the fallible #[cfg(windows)] variant"
)]
fn drive_elevation(_reporter: &mut impl Reporter) -> Result<Option<i32>> {
    Ok(None)
}

/// JSON path: build the envelope and (when `--yes`) mutate.
async fn run_json(
    resolved: &ResolvedPlan,
    request: &ApplyRequest,
    yes: bool,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    if !yes {
        // --json without --yes is a preview; never mutate.
        let document = json_envelope(resolved, "previewed");
        reporter.json(&document);
        return Ok(ExitCode::Success.code());
    }

    // Windows Developer Mode gate (REQ-007) applies to the JSON apply path
    // too: a symlink-bearing plan on a dev-mode-disabled host drives the
    // UAC flow before any mutation. No-op on macOS / Linux.
    if let Some(exit) = drive_dev_mode_gate(resolved, reporter)? {
        return Ok(exit);
    }

    let result = execute_plan(resolved, request, LockPolicy::Blocking)
        .await
        .context("apply execution failed")?;
    let result_field = match &result {
        ApplyResult::Applied { .. } => "applied",
        ApplyResult::RolledBack { .. } => "rolled_back",
        ApplyResult::Aborted { .. } => "aborted",
    };
    let document = json_envelope(resolved, result_field);
    reporter.json(&document);
    Ok(exit_code_for(&result))
}

/// Build the `--json` envelope: `repo_root`, `profile`, `plan`, `result`.
fn json_envelope(resolved: &ResolvedPlan, result: &str) -> String {
    let plan: Vec<serde_json::Value> = resolved
        .operations
        .iter()
        .flat_map(|op| {
            op.targets.iter().map(move |target| {
                serde_json::json!({
                    "mode": mode_label(op.mode),
                    "source": op.source.as_str(),
                    "target": target.as_str(),
                })
            })
        })
        .collect();
    let envelope = serde_json::json!({
        "repo_root": resolved.repo_root.as_str(),
        "profile": resolved.profile,
        "plan": plan,
        "result": result,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Stable lowercase label for a file mode in the JSON envelope.
fn mode_label(mode: patina_core::FileMode) -> &'static str {
    use patina_core::FileMode;
    match mode {
        FileMode::Symlink | FileMode::SymlinkDir => "symlink",
        FileMode::SymlinkTree => "symlink-tree",
        FileMode::Copy | FileMode::CopyTree => "copy",
        FileMode::TemplateRender => "template",
    }
}

/// Render the diff, honouring `--pager` with a PATH-resolution fallback.
fn render_diff(
    resolved: &ResolvedPlan,
    pager: Option<Pager>,
    reporter: &mut impl Reporter,
) -> Result<String> {
    let rendered = diff::render(resolved).map_err(|e| anyhow!(e))?;
    if let Some(pager) = pager
        && patina_core::resolve_on_path(pager.binary()).is_none()
    {
        reporter.warn(&format!(
            "pager `{}` not found on PATH; falling back to the embedded diff",
            pager.binary()
        ));
    }
    // Piping to a resolved external pager is deferred; the embedded
    // renderer is always the source of the rendered string so output
    // stays deterministic for the fallback path REQ-017 specifies.
    Ok(rendered)
}

/// Report a non-JSON apply result through the reporter.
fn report_result(result: &ApplyResult, reporter: &mut impl Reporter) {
    match result {
        ApplyResult::Applied {
            warnings,
            up_to_date,
        } => {
            for warning in warnings {
                reporter.warn(warning);
            }
            // REQ-008: a full no-op prints a deterministic up-to-date line
            // (no timestamp, PID, or state path) instead of "Applied.".
            if *up_to_date {
                reporter.line("Already up to date. No changes to apply.");
            } else {
                reporter.line("Applied.");
            }
        }
        ApplyResult::RolledBack { failed_hook } => {
            reporter.warn(&format!(
                "post_apply hook `{failed_hook}` failed; rolled back all file operations"
            ));
        }
        ApplyResult::Aborted { failed_hook } => {
            reporter.warn(&format!(
                "pre_apply hook `{failed_hook}` failed; aborted before any file operation"
            ));
        }
    }
}

/// Exit code for an apply result (REQ-022 table).
fn exit_code_for(result: &ApplyResult) -> i32 {
    match result {
        ApplyResult::Applied { .. } => ExitCode::Success,
        ApplyResult::Aborted { .. } => ExitCode::PreApplyAbort,
        ApplyResult::RolledBack { .. } => ExitCode::PostApplyRollback,
    }
    .code()
}

/// Build the engine [`ApplyRequest`] from the parsed flags.
fn build_request(args: &ApplyArgs) -> Result<ApplyRequest> {
    let force_deploy = if args.force_deploy {
        ForceDeploy::Yes
    } else {
        ForceDeploy::No
    };
    let mut cli_overrides = Vec::with_capacity(args.var.len());
    for raw in &args.var {
        cli_overrides.push(parse_override(raw)?);
    }
    Ok(ApplyRequest {
        force_deploy,
        cli_overrides,
    })
}

/// Parse a single `-v key=value` override.
fn parse_override(raw: &str) -> Result<(String, String)> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| anyhow!("invalid -v override `{raw}`; expected key=value"))?;
    if key.is_empty() {
        return Err(anyhow!(
            "invalid -v override `{raw}`; the key must not be empty"
        ));
    }
    Ok((key.to_owned(), value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;

    /// A prompt reader that records how many times `read_line` was called
    /// (and always answers EOF). Used to prove a path reads no stdin: the
    /// test asserts `reads == 0`. Recording rather than panicking keeps the
    /// reader free of the production-forbidden `panic!` while still failing
    /// the assertion on a single stray read.
    #[derive(Default)]
    struct RecordingReader {
        reads: usize,
    }

    impl PromptReader for RecordingReader {
        fn read_line(&mut self) -> Option<String> {
            self.reads += 1;
            None
        }
    }

    /// A scripted reader yielding a fixed sequence of answer lines; used for
    /// the non-no-op interactive path that genuinely prompts.
    struct ScriptedReader {
        lines: std::collections::VecDeque<String>,
    }

    impl PromptReader for ScriptedReader {
        fn read_line(&mut self) -> Option<String> {
            self.lines.pop_front()
        }
    }

    #[test]
    fn full_noop_interactive_skips_prompt_and_reads_no_stdin(/* CHK-013, REQ-009 */) {
        // The core REQ-009 observable: a fully-satisfied plan on an
        // interactive TTY must NOT present the confirmation prompt and must
        // read no stdin. `RecordingReader` counts every `read_line`, so the
        // `reads == 0` assertion fails on a single stray read; `BufferReporter.err`
        // captures any prompt text, so a single `prompt` invocation fails too.
        let mut reader = RecordingReader::default();
        let mut reporter = BufferReporter::new();
        let decision = confirm_apply(
            // is_full_noop
            true,
            // yes
            false,
            Tty::Interactive,
            &mut reader,
            &mut reporter,
        );
        assert_eq!(
            decision,
            Confirmation::Proceed,
            "a full no-op must proceed (it writes nothing) without prompting"
        );
        assert_eq!(
            reader.reads, 0,
            "a full no-op must read no stdin, but read_line was called {} time(s)",
            reader.reads
        );
        assert!(
            reporter.err.is_empty(),
            "a full no-op must emit no prompt text, got stderr: {}",
            reporter.err
        );
    }

    #[test]
    fn non_noop_interactive_does_prompt_and_reads_the_answer(/* guards the skip */) {
        // Counterpart to the no-op test: when the plan is NOT a no-op, the
        // interactive path MUST prompt and read the answer. This is the red
        // guard — if `confirm_apply` skipped the prompt unconditionally, the
        // `Apply?` text would be absent and the decline answer ignored.
        let mut reader = ScriptedReader {
            lines: std::collections::VecDeque::from(["n\n".to_owned()]),
        };
        let mut reporter = BufferReporter::new();
        let decision = confirm_apply(
            // is_full_noop
            false,
            // yes
            false,
            Tty::Interactive,
            &mut reader,
            &mut reporter,
        );
        assert_eq!(
            decision,
            Confirmation::Declined,
            "an interactive `n` answer must decline"
        );
        assert!(
            reporter.err.contains("Apply?"),
            "the interactive non-no-op path must emit the confirmation prompt, got: {}",
            reporter.err
        );
    }

    #[test]
    fn yes_proceeds_without_prompting_on_any_tty(/* CHK-028 sibling */) {
        // `--yes` proceeds without consulting the reader on either TTY kind.
        for tty in [Tty::Interactive, Tty::NonInteractive] {
            let mut reader = RecordingReader::default();
            let mut reporter = BufferReporter::new();
            let decision = confirm_apply(false, true, tty, &mut reader, &mut reporter);
            assert_eq!(decision, Confirmation::Proceed, "--yes proceeds on {tty:?}");
            assert_eq!(reader.reads, 0, "--yes must not read stdin on {tty:?}");
            assert!(reporter.err.is_empty(), "--yes must not prompt on {tty:?}");
        }
    }

    #[test]
    fn non_tty_without_yes_previews_only(/* CHK-028 */) {
        let mut reader = RecordingReader::default();
        let mut reporter = BufferReporter::new();
        let decision = confirm_apply(
            false,
            false,
            Tty::NonInteractive,
            &mut reader,
            &mut reporter,
        );
        assert_eq!(
            decision,
            Confirmation::PreviewOnly,
            "a non-TTY shell without --yes previews and exits 0"
        );
        assert_eq!(reader.reads, 0, "the preview path must not read stdin");
        assert!(reporter.err.is_empty(), "the preview path must not prompt");
    }

    #[test]
    fn override_parses_key_value() {
        assert_eq!(
            parse_override("email=a@b.com").expect("parse"),
            ("email".to_owned(), "a@b.com".to_owned())
        );
    }

    #[test]
    fn override_rejects_missing_equals() {
        parse_override("noeq").expect_err("missing `=` must be rejected");
    }

    #[test]
    fn override_rejects_empty_key() {
        parse_override("=value").expect_err("empty key must be rejected");
    }

    #[test]
    fn exit_codes_match_outcomes() {
        assert_eq!(
            exit_code_for(&ApplyResult::Applied {
                warnings: vec![],
                up_to_date: false,
            }),
            0
        );
        assert_eq!(
            exit_code_for(&ApplyResult::Aborted {
                failed_hook: "h".to_owned()
            }),
            2
        );
        assert_eq!(
            exit_code_for(&ApplyResult::RolledBack {
                failed_hook: "h".to_owned()
            }),
            3
        );
    }
}
