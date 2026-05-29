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
use patina_core::ForceDeploy;
use patina_core::ResolvedPlan;
use patina_core::execute_plan;
use patina_core::plan_apply;

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

    // Human path: render the diff first, then decide whether to mutate.
    let rendered = render_diff(&resolved, args.pager, reporter)?;
    reporter.diff(&rendered);

    let should_apply = match (args.yes, tty) {
        (true, _) => true,
        (false, Tty::NonInteractive) => {
            // Non-TTY without --yes: preview only, exit 0 (CHK-028).
            return Ok(ExitCode::Success.code());
        }
        (false, Tty::Interactive) => {
            reporter.prompt("Apply? [y/N] ");
            let answer = reader.read_line().unwrap_or_default();
            matches!(answer.trim(), "y" | "Y")
        }
    };

    if !should_apply {
        // User declined the prompt.
        return Ok(ExitCode::UserDeclined.code());
    }

    let result = execute_plan(&resolved, &request)
        .await
        .context("apply execution failed")?;
    report_result(&result, reporter);
    Ok(exit_code_for(&result))
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

    let result = execute_plan(resolved, request)
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
        FileMode::Symlink => "symlink",
        FileMode::SymlinkDir => "symlink-dir",
        FileMode::Copy => "copy",
        FileMode::CopyTree => "copy-tree",
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
        ApplyResult::Applied { warnings } => {
            for warning in warnings {
                reporter.warn(warning);
            }
            reporter.line("Applied.");
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

/// A monotonic UTC timestamp keying this run's journal and backup files,
/// formatted `YYYYMMDDTHHMMSSZ` (matches the journal fixtures). The
/// timestamp keys the journal filename only; it never appears in user
/// output, so determinism of stdout is preserved.
fn current_timestamp() -> String {
    jiff::Timestamp::now()
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(exit_code_for(&ApplyResult::Applied { warnings: vec![] }), 0);
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

    #[test]
    fn timestamp_is_compact_utc() {
        let ts = current_timestamp();
        // YYYYMMDDTHHMMSSZ is 16 chars; ends in Z, has the T separator.
        assert_eq!(ts.len(), 16, "timestamp {ts} should be 16 chars");
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.as_bytes().get(8), Some(&b'T'));
    }
}
