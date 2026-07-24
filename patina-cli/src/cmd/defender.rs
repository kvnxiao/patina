//! `patina defender` command logic (Windows-only).
//!
//! Weakening antivirus is a deliberate act, so this command never mutates
//! silently: it derives the exact exclusion set from the current plan, previews
//! every add and removal, asks for consent, and only then launches the elevated
//! helper behind one UAC prompt. The engine owns the derivation, diff, and
//! validation ([`patina_core`]); this module is presentation and control flow,
//! all output routed through the [`Reporter`].
//!
//! ## Exit codes
//!
//! | Outcome                                      | Code |
//! |----------------------------------------------|------|
//! | Applied, previewed, cleared, or up to date   | 0    |
//! | Defender rejected the write (Tamper/managed) | 1    |
//! | User declined the prompt or UAC consent      | 5    |

use crate::cli::DefenderArgs;
use crate::cli::DefenderCommand;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use camino::Utf8Path;
use patina_core::ApplyRequest;
use patina_core::DefenderDiff;
use patina_core::DefenderLedger;
use patina_core::DefenderOutcome;
use patina_core::DefenderProbe;
use patina_core::Exclusion;
use patina_core::HostDefenderProbe;
use patina_core::ResolvedPlan;
use patina_core::current_timestamp;
use patina_core::defender_ledger_path;
use patina_core::defender_request_path;
use patina_core::derive_exclusions;
use patina_core::launch_defender_helper;
use patina_core::plan_apply;
use patina_core::plan_defender;
use patina_core::resolve_state_dir;
use patina_core::serialize_request;
use std::collections::BTreeSet;

/// Which reconcile a run performs — they differ only in the desired set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Reconcile to the plan's exclusion set (add missing, reap stale).
    Apply,
    /// Reconcile to the empty set (remove every patina-owned exclusion).
    Clear,
}

/// Run `patina defender`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error when planning, the unprivileged read, the request write, or
/// the elevated helper fails at the engine level. A declined prompt or declined
/// UAC consent is not an error; it maps to a non-zero exit via the returned
/// `i32`.
pub fn run(
    args: &DefenderArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    match &args.command {
        DefenderCommand::Apply { yes, json } => {
            run_reconcile(Action::Apply, *yes, *json, tty, reader, reporter)
        }
        DefenderCommand::Clear { yes, json } => {
            run_reconcile(Action::Clear, *yes, *json, tty, reader, reporter)
        }
        DefenderCommand::Status { json } => run_status(*json, reporter),
    }
}

/// The confirmation decision for the human reconcile path (mirrors the `apply`
/// idiom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    /// Mutate: `--yes` or an interactive `y`.
    Proceed,
    /// Non-TTY without `--yes`: previewed only, exit 0 with no writes.
    PreviewOnly,
    /// Interactive prompt answered with anything other than `y`/`Y`.
    Declined,
}

/// Reconcile the live Defender exclusions to `action`'s desired set.
fn run_reconcile(
    action: Action,
    yes: bool,
    json: bool,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    // `apply` derives its desired set from the plan, so it needs a resolvable
    // repository. `clear` reconciles to the empty set — it must stay usable as
    // the reversibility escape hatch even when the repository is broken or
    // gone, so it resolves the state directory directly and never plans.
    let (state_dir, repo_root, desired) = match action {
        Action::Apply => {
            let resolved = plan_apply(&ApplyRequest::default(), current_timestamp())
                .context("failed to compute the apply plan")?;
            let desired = derive_exclusions(&resolved);
            (resolved.state_dir, Some(resolved.repo_root), desired)
        }
        Action::Clear => {
            let state_dir = resolve_state_dir().context("failed to resolve the state directory")?;
            (state_dir, None, BTreeSet::new())
        }
    };

    let ledger_path = defender_ledger_path(&state_dir);
    let ledger = load_ledger(&ledger_path)?;
    let current = HostDefenderProbe::default()
        .read_exclusions()
        .context("failed to read current Defender exclusions")?;
    let diff = plan_defender(&desired, &current, &ledger.to_set());

    if json {
        return run_reconcile_json(
            &state_dir,
            repo_root.as_deref(),
            &desired,
            &diff,
            yes,
            &ledger_path,
            reporter,
        );
    }

    render_preview(repo_root.as_deref(), &desired, &diff, reporter);

    if diff.is_empty() {
        // Nothing to enact; converge the ledger to the desired set and report.
        save_ledger(&ledger_path, &DefenderLedger::from_set(&desired))?;
        reporter.line("Defender exclusions already up to date. No changes.");
        return Ok(ExitCode::Success.code());
    }

    match confirm(yes, tty, reader, reporter) {
        Confirmation::Proceed => {}
        Confirmation::PreviewOnly => return Ok(ExitCode::Success.code()),
        Confirmation::Declined => return Ok(ExitCode::UserDeclined.code()),
    }

    enact(&state_dir, &desired, &diff, &ledger_path, reporter)
}

/// Write the request file, launch the elevated helper, and map the verified
/// outcome to the ledger update and exit code.
fn enact(
    state_dir: &Utf8Path,
    desired: &BTreeSet<Exclusion>,
    diff: &DefenderDiff,
    ledger_path: &Utf8Path,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    match write_and_launch(state_dir, diff)? {
        DefenderOutcome::Applied => {
            // Only after the helper's re-read confirms the change do we record
            // the new patina-owned set.
            save_ledger(ledger_path, &DefenderLedger::from_set(desired))?;
            reporter.line("Applied Defender exclusion changes.");
            Ok(ExitCode::Success.code())
        }
        DefenderOutcome::Declined => {
            reporter.warn("Defender exclusions were not changed (elevation declined).");
            Ok(ExitCode::UserDeclined.code())
        }
        DefenderOutcome::Blocked => Err(blocked_error()),
    }
}

/// JSON reconcile path: preview without `--yes`, otherwise enact and report.
fn run_reconcile_json(
    state_dir: &Utf8Path,
    repo_root: Option<&Utf8Path>,
    desired: &BTreeSet<Exclusion>,
    diff: &DefenderDiff,
    yes: bool,
    ledger_path: &Utf8Path,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    if !yes {
        reporter.json(&reconcile_json(repo_root, diff, "previewed"));
        return Ok(ExitCode::Success.code());
    }
    if diff.is_empty() {
        save_ledger(ledger_path, &DefenderLedger::from_set(desired))?;
        reporter.json(&reconcile_json(repo_root, diff, "up_to_date"));
        return Ok(ExitCode::Success.code());
    }

    match write_and_launch(state_dir, diff)? {
        DefenderOutcome::Applied => {
            save_ledger(ledger_path, &DefenderLedger::from_set(desired))?;
            reporter.json(&reconcile_json(repo_root, diff, "applied"));
            Ok(ExitCode::Success.code())
        }
        DefenderOutcome::Declined => {
            reporter.json(&reconcile_json(repo_root, diff, "declined"));
            Ok(ExitCode::UserDeclined.code())
        }
        DefenderOutcome::Blocked => {
            reporter.json(&reconcile_json(repo_root, diff, "blocked"));
            Ok(ExitCode::Generic.code())
        }
    }
}

/// Write the request file and launch the elevated helper, returning the
/// re-read-verified outcome. Shared by the human and JSON enact paths.
fn write_and_launch(state_dir: &Utf8Path, diff: &DefenderDiff) -> Result<DefenderOutcome> {
    let request_path = defender_request_path(state_dir);
    write_request(&request_path, diff)?;
    launch_defender_helper(&request_path, diff).context("failed to launch the Defender helper")
}

/// Report current Defender exclusions against the desired set (read-only).
fn run_status(json: bool, reporter: &mut impl Reporter) -> Result<i32> {
    let resolved = plan_apply(&ApplyRequest::default(), current_timestamp())
        .context("failed to compute the apply plan")?;
    let desired = derive_exclusions(&resolved);
    let ledger = load_ledger(&defender_ledger_path(&resolved.state_dir))?;

    match HostDefenderProbe::default().read_exclusions() {
        Ok(current) => {
            let diff = plan_defender(&desired, &current, &ledger.to_set());
            if json {
                reporter.json(&status_json(&resolved, &desired, &diff));
            } else {
                render_status(&resolved, &desired, &diff, reporter);
            }
            Ok(ExitCode::Success.code())
        }
        Err(err) => {
            // Graceful degrade, mirroring `doctor`'s shared-lock downgrade: the
            // read is a best effort, so a restricted read warns and reports the
            // desired set rather than hard-failing.
            reporter.warn(&format!(
                "could not read current Defender exclusions: {err}; showing the desired set only"
            ));
            if json {
                reporter.json(&status_desired_only_json(&resolved, &desired));
            } else {
                render_status_desired_only(&resolved, &desired, reporter);
            }
            Ok(ExitCode::Success.code())
        }
    }
}

/// The interactive confirmation, prompting only on an interactive TTY.
fn confirm(
    yes: bool,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Confirmation {
    match (yes, tty) {
        (true, _) => Confirmation::Proceed,
        (false, Tty::NonInteractive) => Confirmation::PreviewOnly,
        (false, Tty::Interactive) => {
            reporter.confirm("Modify Windows Defender exclusions?");
            let answer = reader.read_line().unwrap_or_default();
            if matches!(answer.trim(), "y" | "Y") {
                Confirmation::Proceed
            } else {
                Confirmation::Declined
            }
        }
    }
}

/// Render the add / remove / unchanged preview for a reconcile. `repo_root` is
/// `None` for `clear`, which reconciles to the empty set without planning a
/// repository.
fn render_preview(
    repo_root: Option<&Utf8Path>,
    desired: &BTreeSet<Exclusion>,
    diff: &DefenderDiff,
    reporter: &mut impl Reporter,
) {
    match repo_root {
        Some(repo_root) => reporter.line(&format!("Defender exclusions for {repo_root}:")),
        None => reporter.line("Patina-owned Defender exclusions:"),
    }
    let add_keys: BTreeSet<String> = diff.to_add.iter().map(Exclusion::key).collect();
    for exclusion in &diff.to_add {
        reporter.line(&format!(
            "  + {} ({})",
            exclusion.path,
            exclusion.kind.label()
        ));
    }
    for exclusion in &diff.to_remove {
        reporter.line(&format!(
            "  - {} ({})",
            exclusion.path,
            exclusion.kind.label()
        ));
    }
    for exclusion in desired {
        if !add_keys.contains(&exclusion.key()) {
            reporter.line(&format!(
                "    {} ({}) [unchanged]",
                exclusion.path,
                exclusion.kind.label()
            ));
        }
    }
    if diff.is_empty() && desired.is_empty() {
        reporter.line("  (no patina-owned exclusions)");
    }
}

/// Render the status view: each desired exclusion as present / missing, plus
/// any patina-owned stale entries.
fn render_status(
    resolved: &ResolvedPlan,
    desired: &BTreeSet<Exclusion>,
    diff: &DefenderDiff,
    reporter: &mut impl Reporter,
) {
    reporter.line(&format!("Defender exclusions for {}:", resolved.repo_root));
    // A desired exclusion in `to_add` is not currently present; everything else
    // in the desired set is present.
    let missing_keys: BTreeSet<String> = diff.to_add.iter().map(Exclusion::key).collect();
    for exclusion in desired {
        let state = if missing_keys.contains(&exclusion.key()) {
            "missing"
        } else {
            "present"
        };
        reporter.line(&format!(
            "  {} ({}) [{state}]",
            exclusion.path,
            exclusion.kind.label()
        ));
    }
    for exclusion in &diff.to_remove {
        reporter.line(&format!(
            "  {} ({}) [stale — patina-owned, no longer managed]",
            exclusion.path,
            exclusion.kind.label()
        ));
    }
}

/// Render the status view when the live read failed: the desired set only.
fn render_status_desired_only(
    resolved: &ResolvedPlan,
    desired: &BTreeSet<Exclusion>,
    reporter: &mut impl Reporter,
) {
    reporter.line(&format!(
        "Desired Defender exclusions for {} (current state unavailable):",
        resolved.repo_root
    ));
    for exclusion in desired {
        reporter.line(&format!(
            "  {} ({})",
            exclusion.path,
            exclusion.kind.label()
        ));
    }
}

/// The reconcile JSON envelope: `repo_root`, `to_add`, `to_remove`, `result`.
///
/// `repo_root` is `null` for `clear`, which does not plan a repository.
fn reconcile_json(repo_root: Option<&Utf8Path>, diff: &DefenderDiff, result: &str) -> String {
    let envelope = serde_json::json!({
        "repo_root": repo_root.map(Utf8Path::as_str),
        "to_add": exclusions_json(&diff.to_add),
        "to_remove": exclusions_json(&diff.to_remove),
        "result": result,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// The status JSON envelope: each desired exclusion with its `present` state,
/// plus the stale patina-owned entries.
fn status_json(
    resolved: &ResolvedPlan,
    desired: &BTreeSet<Exclusion>,
    diff: &DefenderDiff,
) -> String {
    let missing_keys: BTreeSet<String> = diff.to_add.iter().map(Exclusion::key).collect();
    let exclusions: Vec<serde_json::Value> = desired
        .iter()
        .map(|exclusion| {
            serde_json::json!({
                "path": exclusion.path.as_str(),
                "kind": exclusion.kind.label(),
                "present": !missing_keys.contains(&exclusion.key()),
            })
        })
        .collect();
    let envelope = serde_json::json!({
        "repo_root": resolved.repo_root.as_str(),
        "exclusions": exclusions,
        "stale": exclusions_json(&diff.to_remove),
        "current_readable": true,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// The degraded status JSON envelope when the live read failed.
fn status_desired_only_json(resolved: &ResolvedPlan, desired: &BTreeSet<Exclusion>) -> String {
    let exclusions: Vec<serde_json::Value> = desired
        .iter()
        .map(|exclusion| {
            serde_json::json!({
                "path": exclusion.path.as_str(),
                "kind": exclusion.kind.label(),
            })
        })
        .collect();
    let envelope = serde_json::json!({
        "repo_root": resolved.repo_root.as_str(),
        "exclusions": exclusions,
        "current_readable": false,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// A JSON array of `{path, kind}` objects for a list of exclusions.
fn exclusions_json(exclusions: &[Exclusion]) -> Vec<serde_json::Value> {
    exclusions
        .iter()
        .map(|exclusion| {
            serde_json::json!({
                "path": exclusion.path.as_str(),
                "kind": exclusion.kind.label(),
            })
        })
        .collect()
}

/// The typed error for a blocked write — Defender returned success but the
/// re-read shows the exclusions did not change. Names the likely cause and an
/// actionable next step.
fn blocked_error() -> anyhow::Error {
    anyhow!(
        "Defender rejected the exclusion change; the write did not take. This \
         usually means Tamper Protection is enabled or Defender is managed by \
         policy (Intune / GPO). Check `Get-MpComputerStatus` (IsTamperProtected \
         and AMRunningMode); apply the exclusions through your management tool, \
         or consider a Windows 11 Dev Drive in Defender performance mode as a \
         lower-risk alternative to path exclusions."
    )
}

/// Write the request file the elevated helper reads.
fn write_request(request_path: &Utf8Path, diff: &DefenderDiff) -> Result<()> {
    fs_err::write(request_path.as_std_path(), serialize_request(diff))
        .with_context(|| format!("failed to write the Defender request file `{request_path}`"))
}

/// Load the patina-owned exclusion ledger, treating an absent file as empty.
fn load_ledger(path: &Utf8Path) -> Result<DefenderLedger> {
    match fs_err::read_to_string(path.as_std_path()) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("failed to parse the Defender ledger `{path}`")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(DefenderLedger::default()),
        Err(err) => {
            Err(anyhow::Error::new(err)
                .context(format!("failed to read the Defender ledger `{path}`")))
        }
    }
}

/// Persist the ledger deterministically (sorted, pretty JSON with a trailing
/// newline).
fn save_ledger(path: &Utf8Path, ledger: &DefenderLedger) -> Result<()> {
    let mut json =
        serde_json::to_string_pretty(ledger).context("failed to serialize the Defender ledger")?;
    json.push('\n');
    fs_err::write(path.as_std_path(), json)
        .with_context(|| format!("failed to write the Defender ledger `{path}`"))
}
