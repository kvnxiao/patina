//! `patina apply` command logic.
//!
//! Owns the decision tree (TTY prompt vs non-TTY preview, `--yes`, `--json`,
//! `--pager`) and maps the engine's [`ApplyResult`] onto the process exit
//! code. Planning, journaling, the executors, the hooks, and rollback live in
//! `patina_core`; presentation and control flow live here.
//!
//! ## Exit codes
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
use crate::output::style::paint;
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
use patina_core::Orphan;
use patina_core::ResolvedPlan;
use patina_core::current_timestamp;
use patina_core::decide_symlink_gate;
use patina_core::execute_plan;
use patina_core::plan_apply;
use patina_core::plan_is_full_noop;
use patina_core::plan_orphans;
use patina_core::remote::lockfile::Lockfile;
use patina_core::remote::lockfile::lockfile_path;

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
    let mutating = rewrites_the_lockfile(args, tty);
    if args.update {
        if args.json {
            reporter.warn(
                "`--update` is ignored with `--json`; run `patina remote update` first, then \
                 `patina apply --json`",
            );
        } else if mutating {
            run_remote_updates(tty, reader, reporter);
        } else {
            reporter.warn(
                "`--update` skipped: a non-interactive apply without `--yes` is a preview and \
                 must not bump pins; run `patina remote update` or add `--yes`",
            );
        }
    }
    let timestamp = current_timestamp();
    let resolved = plan_apply(&request, timestamp).context("failed to compute the apply plan")?;
    prune_stale_pins(&resolved, mutating, reporter)?;

    if args.json {
        return run_json(&resolved, &request, args.yes, reporter).await;
    }

    // The engine re-checks this under the held lock. This probe governs only
    // whether the human path renders a diff and prompts.
    let is_full_noop =
        plan_is_full_noop(&resolved).context("failed to determine apply plan state")?;

    if !is_full_noop {
        // Orphans the reap phase would delete this run are not plan
        // operations, so pass them to the renderer explicitly; the engine
        // re-derives the same set under the held lock when it executes.
        let orphans = plan_orphans(&resolved).context("failed to determine the reap set")?;
        let rendered = render_diff(&resolved, &orphans, args.pager, reporter)?;
        reporter.out_block(&rendered);
    }

    match confirm_apply(is_full_noop, args.yes, tty, reader, reporter) {
        Confirmation::Proceed => {}
        Confirmation::PreviewOnly => return Ok(ExitCode::Success.code()),
        Confirmation::Declined => return Ok(ExitCode::UserDeclined.code()),
    }

    if let Some(exit) = drive_dev_mode_gate(&resolved, reporter)? {
        return Ok(exit);
    }

    let result = execute_plan(&resolved, &request, LockPolicy::Blocking)
        .await
        .context("apply execution failed")?;
    report_result(&result, reporter);
    Ok(exit_code_for(&result))
}

/// Whether this invocation may rewrite the working-tree `patina.lock`.
///
/// Both the `--update` producer pass and the stale-pin sweep write that file,
/// so both are held back on a run that owes the caller zero writes: a
/// non-interactive apply without `--yes` is a preview, and a `--json` run owns
/// stdout as one machine-readable document.
fn rewrites_the_lockfile(args: &ApplyArgs, tty: Tty) -> bool {
    !args.json && (args.yes || tty == Tty::Interactive)
}

/// Drop `patina.lock` pins the root manifest no longer declares.
///
/// Like `--update`, the rewrite lands before the consent prompt: it edits the
/// repository, not a managed target, and a declined diff leaves it correctly
/// pruned either way. A preview writes nothing and names the stale pins
/// instead.
///
/// The common case, nothing stale, is decided from an unlocked read and
/// costs no lock. A mutating pass then redoes the read-modify-write under the
/// exclusive lock: `Lockfile::save` rewrites the whole file, so a snapshot
/// taken outside the lock would silently revert whatever a concurrent
/// `patina remote update` bumped in between.
fn prune_stale_pins(
    resolved: &patina_core::ResolvedPlan,
    mutating: bool,
    reporter: &mut impl Reporter,
) -> Result<()> {
    let path = lockfile_path(&resolved.repo_root);
    let Some(stale) = stale_pins(&path, resolved, reporter) else {
        return Ok(());
    };
    if stale.is_empty() {
        return Ok(());
    }

    if !mutating {
        reporter.warn(&format!(
            "patina.lock pins {}, which no [[remote]] table declares; an apply that may \
             write will drop the entries",
            stale.join(", ")
        ));
        return Ok(());
    }

    let _guard = patina_core::acquire_lock(
        &resolved.state_dir.join("lock"),
        patina_core::LockKind::Exclusive,
        patina_core::exclusive_timeout(),
    )
    .context("failed to acquire the exclusive lock to prune stale patina.lock pins")?;
    let Ok(mut lockfile) = Lockfile::load(&path) else {
        return Ok(());
    };
    let stale = names_of(&lockfile.retain_declared(&resolved.remote_names));
    if stale.is_empty() {
        return Ok(());
    }
    lockfile
        .save(&path)
        .map_err(patina_core::EngineError::from)
        .context("failed to write patina.lock")?;
    reporter.warn(&format!(
        "dropped patina.lock pins no [[remote]] table declares: {}",
        stale.join(", ")
    ));
    Ok(())
}

/// The names of the pins the root manifest no longer declares, from an
/// unlocked read, or `None` when the lockfile does not parse.
///
/// Planning has already succeeded, so no entry needed the lockfile this run.
/// Refusing to apply over one that will not parse would break the guarantee
/// that a stray `patina.lock` costs a repository nothing until an entry
/// actually selects a remote. The engine's own read raises the parse error
/// when one does.
fn stale_pins(
    path: &camino::Utf8Path,
    resolved: &patina_core::ResolvedPlan,
    reporter: &mut impl Reporter,
) -> Option<Vec<String>> {
    match Lockfile::load(path) {
        Ok(mut lockfile) => Some(names_of(&lockfile.retain_declared(&resolved.remote_names))),
        Err(error) => {
            reporter.warn(&format!("leaving patina.lock alone: {error}"));
            None
        }
    }
}

/// Remote names as written, for a message that lists them.
fn names_of(remotes: &[patina_core::RemoteName]) -> Vec<String> {
    remotes.iter().map(ToString::to_string).collect()
}

/// Run `patina remote update` over every remote before the apply proper.
///
/// Failures here never fail the apply: an unreachable remote degrades to a
/// plain apply against the committed pins, with a warning. Whatever pins the
/// pass did bump are already written, so the apply that follows sees them.
///
/// The gate is not auto-accepted: `yes` is `false` regardless of the apply's
/// own `--yes`. A rewritten-history or backdated bump is held, or prompted on
/// a TTY, so `apply --yes` never silently accepts a supply-chain concern the
/// gate exists to surface. `patina remote update --yes` remains the explicit
/// way to accept one.
fn run_remote_updates(tty: Tty, reader: &mut impl PromptReader, reporter: &mut impl Reporter) {
    match crate::cmd::remote::run_update_all(tty, reader, reporter) {
        Ok(code) if code == ExitCode::Success.code() => {}
        Ok(_) => reporter.warn(
            "some remotes could not be updated; applying the pins already committed \
             in patina.lock",
        ),
        Err(error) => reporter.warn(&format!(
            "remote update failed ({error}); applying the pins already committed in patina.lock"
        )),
    }
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

/// Decide whether to proceed with the apply, prompting only on the
/// interactive review path.
///
/// A full no-op (`is_full_noop`) short-circuits ahead of the diff-and-prompt
/// branch, so a fully-satisfied repo neither sees the prompt nor has a line
/// read from `reader`.
fn confirm_apply(
    is_full_noop: bool,
    yes: bool,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Confirmation {
    if is_full_noop {
        // Safe either way: a no-op writes nothing whatever the user answers.
        return Confirmation::Proceed;
    }
    match (yes, tty) {
        (true, _) => Confirmation::Proceed,
        // Non-TTY without --yes: preview only, exit 0.
        (false, Tty::NonInteractive) => Confirmation::PreviewOnly,
        (false, Tty::Interactive) => {
            reporter.confirm("Apply?");
            let answer = reader.read_line().unwrap_or_default();
            if matches!(answer.trim(), "y" | "Y") {
                Confirmation::Proceed
            } else {
                Confirmation::Declined
            }
        }
    }
}

/// Drive the Windows Developer Mode symlink-elevation gate.
///
/// Returns `Ok(None)` when the apply may proceed (no symlink in the plan,
/// not on Windows, Developer Mode already on, or the helper just enabled
/// it). Returns `Ok(Some(code))` to short-circuit the command with a
/// terminal exit code: `5` when the user declines the UAC consent dialog.
/// Returns `Err` when the helper ran but the flag still reads off afterward
/// (exit 1).
///
/// On macOS / Linux [`decide_symlink_gate`] reports `Proceed` (the probe is
/// `NotWindows`), so this never reads the registry and never spawns the
/// helper.
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
            // stderr must name `Developer Mode` and `patina doctor --fix`.
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
/// reports `NotWindows`), so this arm is unreachable in practice. It exists
/// only so the cross-platform gate compiles without a `#[cfg]` at the call
/// site.
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
    // Before any mutation, so the envelope reports what this run would
    // remove. The engine re-derives the same set under the lock.
    let reaped = plan_orphans(resolved).context("failed to determine the reap set")?;

    if !yes {
        let document = json_envelope(resolved, &reaped, "previewed");
        reporter.json(&document);
        return Ok(ExitCode::Success.code());
    }

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
    let document = json_envelope(resolved, &reaped, result_field);
    reporter.json(&document);
    Ok(exit_code_for(&result))
}

/// Build the `--json` envelope: `repo_root`, `profile`, `plan`, `reaped`,
/// `result`.
///
/// Each plan row carries a `state` field: the target's
/// [`Disposition`](patina_core::Disposition) label (`create`, `update`, or
/// `unchanged`), through the canonical
/// [`Disposition::label`](patina_core::Disposition::label) mapping. It is a
/// pure function of the plan-time classification, so it inherits the
/// deterministic-stdout contract.
///
/// `reaped` lists what this run would remove: orphans of a prior apply the
/// current plan no longer manages. They are not plan operations, so they are
/// reported in their own array rather than as `plan` rows; the human diff
/// renders the same set as `remove` blocks. Each row is an object carrying the
/// `target` and the `reason` it fell out of the managed set
/// ([`OrphanReason::label`](patina_core::OrphanReason::label)), so a consumer
/// can tell a deletion caused by a new `ignore` pattern from one caused by a
/// dropped entry. `plan_orphans` sorted them by target, so the array is a
/// stable function of the reap set.
fn json_envelope(resolved: &ResolvedPlan, reaped: &[Orphan], result: &str) -> String {
    let plan: Vec<serde_json::Value> = resolved
        .operations
        .iter()
        .flat_map(|op| {
            op.targets
                .iter()
                .zip(&op.dispositions)
                .flat_map(move |(target, disposition)| plan_rows(op, target, disposition))
        })
        .collect();
    let reaped: Vec<serde_json::Value> = reaped
        .iter()
        .map(|orphan| {
            serde_json::json!({
                "target": orphan.target.as_str(),
                "reason": orphan.reason.label(),
            })
        })
        .collect();
    let envelope = serde_json::json!({
        "repo_root": resolved.repo_root.as_str(),
        "profile": resolved.profile,
        "plan": plan,
        "reaped": reaped,
        "result": result,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// The plan rows one `(operation, target)` pair contributes to the `--json`
/// plan array.
///
/// A single-target mode (empty `leaves`) yields one row carrying the target's
/// own disposition label. A tree mode yields one row per materialized leaf.
/// Each row carries that leaf's path, under the declared target, and its
/// per-leaf disposition label. That is the same per-leaf routing the human
/// diff renderer uses, so the two surfaces agree on what an entry expands to.
fn plan_rows(
    op: &patina_core::ResolvedOperation,
    target: &camino::Utf8Path,
    disposition: &patina_core::TargetDisposition,
) -> Vec<serde_json::Value> {
    if disposition.leaves.is_empty() {
        vec![serde_json::json!({
            "mode": mode_label(op.mode),
            "source": op.source.as_str(),
            "target": target.as_str(),
            "state": disposition.aggregate.label(),
        })]
    } else {
        disposition
            .leaves
            .iter()
            .map(|leaf| {
                serde_json::json!({
                    "mode": mode_label(op.mode),
                    "source": op.source.join(&leaf.relative).as_str(),
                    "target": target.join(&leaf.relative).as_str(),
                    "state": leaf.disposition.label(),
                })
            })
            .collect()
    }
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
    orphans: &[Orphan],
    pager: Option<Pager>,
    reporter: &mut impl Reporter,
) -> Result<String> {
    let rendered = diff::render(resolved, orphans).map_err(|e| anyhow!(e))?;
    if let Some(pager) = pager
        && patina_core::resolve_on_path(pager.binary()).is_none()
    {
        reporter.warn(&format!(
            "pager `{}` not found on PATH; falling back to the embedded diff",
            pager.binary()
        ));
    }
    // Patina never pipes to an external pager. The embedded renderer is
    // always the source of the rendered string, so stdout stays
    // deterministic.
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
            // Both lines are deterministic: no timestamp, PID, or state path.
            let outcome = if *up_to_date {
                "Already up to date. No changes to apply."
            } else {
                "Applied."
            };
            reporter.line(&paint(reporter.styles().success, outcome));
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

/// Exit code for an apply result.
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
    fn full_noop_interactive_skips_prompt_and_reads_no_stdin() {
        // A fully-satisfied plan on an interactive TTY must not prompt, and
        // must read no stdin.
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
    fn non_noop_interactive_does_prompt_and_reads_the_answer() {
        // Counterpart to the no-op test: a plan that is not a no-op must
        // prompt on an interactive TTY and read the answer. A `confirm_apply`
        // that skipped the prompt unconditionally fails here.
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
    fn yes_proceeds_without_prompting_on_any_tty() {
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
    fn non_tty_without_yes_previews_only() {
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
