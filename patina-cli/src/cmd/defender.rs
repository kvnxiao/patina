//! `patina defender` command logic (Windows-only).
//!
//! Weakening antivirus is a deliberate act, so this command never mutates
//! silently: it derives the exact exclusion set from the current plan, previews
//! every add and removal, asks for consent, and only then launches the elevated
//! helper behind one UAC prompt. The engine owns the derivation, diff, and
//! validation ([`patina_core`]); this module is presentation and control flow.
//!
//! ## Exit codes
//!
//! | Outcome                                      | Code |
//! |----------------------------------------------|------|
//! | Applied, previewed, cleared, or up to date   | 0    |
//! | Defender rejected the write (Tamper/managed) | 1    |
//! | The helper could not apply the request       | 1    |
//! | The helper never reported an outcome         | 1    |
//! | User declined the prompt or UAC consent      | 5    |
//!
//! `Blocked`, `Failed`, and `Unconfirmed` share exit 1. The error message is
//! the only thing separating them. An unprivileged CLI process cannot read
//! Defender's exclusion list, so the helper does the verifying: once it enacts
//! the request, it re-reads the list and reports what it found. A `Blocked`
//! message is therefore an observed rejection. `Unconfirmed` means the verdict
//! never arrived before the deadline; the exclusions may still have changed.

use crate::cli::DefenderArgs;
use crate::cli::DefenderCommand;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use crate::output::style::Styles;
use crate::output::style::paint;
use crate::output::table::emit_aligned;
use crate::output::table::row;
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
use patina_core::ExclusionClassifier;
use patina_core::ExclusionKind;
use patina_core::ExclusionState;
use patina_core::HostDefenderProbe;
use patina_core::ResolvedPlan;
use patina_core::current_timestamp;
use patina_core::defender_ledger_path;
use patina_core::defender_request_path;
use patina_core::defender_result_path;
use patina_core::derive_exclusions;
use patina_core::launch_defender_helper;
use patina_core::plan_apply;
use patina_core::plan_defender;
use patina_core::resolve_state_dir;
use patina_core::serialize_request;
use std::collections::BTreeSet;

/// The caveat printed whenever a rendered state was inferred from the ledger
/// rather than read from Defender. A reader cannot then mistake the one for
/// the other.
///
/// It states the remedy, not only the constraint. Neither `status` nor a
/// preview raises a UAC prompt, because both are read-only by contract. A
/// reader told only that administrator is required would wait for a prompt
/// that never comes. Elevating is the user's move, and the note has to say so.
const LEDGER_SOURCE_NOTE: &str =
    "  (showing what Patina recorded; re-run elevated to compare against Defender's list)";

/// The human label for an exclusion state.
///
/// Every state gets its own wording. `Recorded` and `Unrecorded` are inferred
/// from the ledger, so they are worded as inferences: `recorded` never reads
/// as `present`.
fn state_label(state: ExclusionState) -> &'static str {
    match state {
        ExclusionState::Owned => "present",
        ExclusionState::Unmanaged => "present, not recorded by patina",
        ExclusionState::Absent => "missing",
        ExclusionState::Recorded => "recorded",
        ExclusionState::Unrecorded => "not recorded",
    }
}

/// The stable machine token for an exclusion state, for `--json`.
///
/// Separate from [`state_label`] so the human wording can be reworded without
/// breaking a consumer.
fn state_token(state: ExclusionState) -> &'static str {
    match state {
        ExclusionState::Owned => "owned",
        ExclusionState::Unmanaged => "unmanaged",
        ExclusionState::Absent => "absent",
        ExclusionState::Recorded => "recorded",
        ExclusionState::Unrecorded => "unrecorded",
    }
}

/// The reconcile mode a run performs. `Apply` and `Clear` differ only in the
/// desired set.
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
    // repository. `clear` reconciles to the empty set and has to stay usable
    // as the reversibility escape hatch even when the repository is broken or
    // gone; it therefore resolves the state directory directly and never
    // plans.
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

    let ledger = load_ledger(&defender_ledger_path(&state_dir))?;
    let current = HostDefenderProbe::default()
        .read_exclusions()
        .context("failed to read current Defender exclusions")?;
    let recorded = ledger.to_set();
    let diff = plan_defender(&desired, &current, &recorded);
    let classifier = ExclusionClassifier::new(&current, &recorded);
    let reconcile = Reconcile {
        state_dir: &state_dir,
        repo_root: repo_root.as_deref(),
        desired: &desired,
        diff: &diff,
        classifier: &classifier,
    };

    if json {
        return run_reconcile_json(&reconcile, yes, reporter);
    }

    render_preview(&reconcile, reporter);

    if diff.is_empty() {
        // An empty diff enacts nothing against Defender. Converging the ledger
        // can still claim exclusions Patina did not previously own, so the line
        // reports that count instead of "no changes".
        let adopted = reconcile.adoptable();
        reconcile.record_ledger()?;
        reporter.line(&if adopted == 0 {
            "Defender exclusions already up to date. No changes.".to_owned()
        } else {
            let paths = if adopted == 1 { "path" } else { "paths" };
            format!(
                "Defender exclusions already up to date. Recorded {adopted} already-excluded \
                 {paths} as patina-owned."
            )
        });
        return Ok(ExitCode::Success.code());
    }

    match confirm(yes, tty, reader, reporter) {
        Confirmation::Proceed => {}
        Confirmation::PreviewOnly => return Ok(ExitCode::Success.code()),
        Confirmation::Declined => return Ok(ExitCode::UserDeclined.code()),
    }

    enact(&reconcile, reporter)
}

/// Everything a reconcile's preview and enact need, gathered so the human and
/// JSON paths take one context instead of two long parallel argument lists.
struct Reconcile<'a> {
    /// The resolved state directory holding the request, result, and ledger
    /// files.
    state_dir: &'a Utf8Path,
    /// The repository the desired set was derived from; `None` for `clear`,
    /// which reconciles to the empty set without planning one.
    repo_root: Option<&'a Utf8Path>,
    /// The exclusion set this run reconciles to.
    desired: &'a BTreeSet<Exclusion>,
    /// The add/remove work the run would perform.
    diff: &'a DefenderDiff,
    /// Classifies each desired exclusion against the same live-list reading and
    /// ledger the diff was computed from, and so also says whether that reading
    /// saw Defender's list at all.
    classifier: &'a ExclusionClassifier,
}

impl Reconcile<'_> {
    /// Converge the ledger to this run's desired set, recording what Patina now
    /// owns.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger cannot be serialized or written.
    fn record_ledger(&self) -> Result<()> {
        save_ledger(
            &defender_ledger_path(self.state_dir),
            &DefenderLedger::from_set(self.desired),
        )
    }

    /// How many desired exclusions Defender already has that the ledger does
    /// not, and that [`Reconcile::record_ledger`] will therefore claim.
    ///
    /// Always zero when the live list was withheld, since an unmanaged entry
    /// cannot be told from an owned one without it.
    fn adoptable(&self) -> usize {
        self.desired
            .iter()
            .filter(|exclusion| self.classifier.classify(exclusion) == ExclusionState::Unmanaged)
            .count()
    }
}

/// Write the request file, launch the elevated helper, and map the verified
/// outcome to the ledger update and exit code.
fn enact(reconcile: &Reconcile<'_>, reporter: &mut impl Reporter) -> Result<i32> {
    // The helper runs with its window hidden and its PowerShell work takes
    // seconds, so the terminal would otherwise sit silent until the verdict,
    // with only the UAC dialog to explain the pause.
    reporter.line("Elevating to apply and verify the change; this takes a few seconds.");
    match write_and_launch(reconcile.state_dir, reconcile.diff)? {
        DefenderOutcome::Applied => {
            // The new patina-owned set is recorded only after the helper's
            // re-read confirms the change.
            reconcile.record_ledger()?;
            reporter.line("Applied Defender exclusion changes.");
            Ok(ExitCode::Success.code())
        }
        DefenderOutcome::Declined => {
            reporter.warn("Defender exclusions were not changed (elevation declined).");
            Ok(ExitCode::UserDeclined.code())
        }
        DefenderOutcome::Blocked { detail } => Err(blocked_error(&detail)),
        DefenderOutcome::Failed { detail } => Err(failed_error(&detail)),
        DefenderOutcome::Unconfirmed => Err(unconfirmed_error()),
    }
}

/// JSON reconcile path: preview without `--yes`, otherwise enact and report.
fn run_reconcile_json(
    reconcile: &Reconcile<'_>,
    yes: bool,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let mut report = |result: &str, detail: &str| {
        reporter.json(&reconcile_json(reconcile, result, detail));
    };

    if !yes {
        report("previewed", "");
        return Ok(ExitCode::Success.code());
    }
    if reconcile.diff.is_empty() {
        reconcile.record_ledger()?;
        report("up_to_date", "");
        return Ok(ExitCode::Success.code());
    }

    match write_and_launch(reconcile.state_dir, reconcile.diff)? {
        DefenderOutcome::Applied => {
            reconcile.record_ledger()?;
            report("applied", "");
            Ok(ExitCode::Success.code())
        }
        DefenderOutcome::Declined => {
            report("declined", "");
            Ok(ExitCode::UserDeclined.code())
        }
        // The failing results share an exit code, so `result` is what separates
        // them here. `detail` adds the helper's own words, the same text the
        // human path puts in its error message.
        DefenderOutcome::Blocked { detail } => {
            report("blocked", &detail);
            Ok(ExitCode::Generic.code())
        }
        DefenderOutcome::Failed { detail } => {
            report("failed", &detail);
            Ok(ExitCode::Generic.code())
        }
        DefenderOutcome::Unconfirmed => {
            report("unconfirmed", "");
            Ok(ExitCode::Generic.code())
        }
    }
}

/// Write the request file and launch the elevated helper, returning the outcome
/// the helper reported. Shared by the human and JSON enact paths.
fn write_and_launch(state_dir: &Utf8Path, diff: &DefenderDiff) -> Result<DefenderOutcome> {
    let request_path = defender_request_path(state_dir);
    write_request(&request_path, diff)?;
    launch_defender_helper(&request_path, &defender_result_path(state_dir))
        .context("failed to launch the Defender helper")
}

/// Report current Defender exclusions against the desired set (read-only).
fn run_status(json: bool, reporter: &mut impl Reporter) -> Result<i32> {
    let resolved = plan_apply(&ApplyRequest::default(), current_timestamp())
        .context("failed to compute the apply plan")?;
    let desired = derive_exclusions(&resolved);
    let ledger = load_ledger(&defender_ledger_path(&resolved.state_dir))?;

    match HostDefenderProbe::default().read_exclusions() {
        Ok(current) => {
            let recorded = ledger.to_set();
            let diff = plan_defender(&desired, &current, &recorded);
            let classifier = ExclusionClassifier::new(&current, &recorded);
            if json {
                reporter.json(&status_json(&resolved, &desired, &diff, &classifier));
            } else {
                render_status(&resolved, &desired, &diff, &classifier, reporter);
            }
            Ok(ExitCode::Success.code())
        }
        Err(err) => {
            // The live read is best-effort, so a restricted one warns and
            // reports the desired set rather than hard-failing. Same downgrade
            // `doctor` takes on a shared-lock timeout.
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

/// The exclusion's path, painted in its kind's color.
///
/// The listing does not print `(file)` / `(folder)` text, so this color is the
/// only place the kind appears in human output. See
/// [`ExclusionStyles`](crate::output::style::ExclusionStyles) for what that
/// costs and where the kind is still readable as data.
fn path_by_kind(exclusion: &Exclusion, styles: &Styles) -> String {
    let style = match exclusion.kind {
        ExclusionKind::File => styles.exclusion.file,
        ExclusionKind::Folder => styles.exclusion.folder,
    };
    paint(style, exclusion.path.as_str())
}

/// The bracketed state tag for a classified exclusion.
fn state_tag(state: ExclusionState, styles: &Styles) -> String {
    let style = match state {
        ExclusionState::Owned | ExclusionState::Recorded => styles.exclusion.state_present,
        ExclusionState::Unmanaged => styles.exclusion.state_unmanaged,
        ExclusionState::Absent | ExclusionState::Unrecorded => styles.exclusion.state_absent,
    };
    paint(style, &format!("[{}]", state_label(state)))
}

/// A bracketed tag carrying arbitrary prose, in the not-in-place style.
///
/// Only the stale-entry line needs this. A stale entry is not a desired
/// exclusion, so its phrase is none of the exclusion states.
fn stale_tag(styles: &Styles) -> String {
    paint(
        styles.exclusion.state_absent,
        "[stale: patina-owned, no longer managed]",
    )
}

/// Render the add / remove / unchanged preview for a reconcile.
///
/// `repo_root` is `None` for `clear`: that verb reconciles to the empty set
/// and does not plan a repository.
fn render_preview(reconcile: &Reconcile<'_>, reporter: &mut impl Reporter) {
    let styles = &reporter.styles();
    let Reconcile {
        repo_root,
        desired,
        diff,
        classifier,
        ..
    } = *reconcile;

    match repo_root {
        Some(repo_root) => reporter.line(&format!("Defender exclusions for {repo_root}:")),
        None => reporter.line("Patina-owned Defender exclusions:"),
    }
    if !classifier.live_list_was_read() {
        reporter.line(LEDGER_SOURCE_NOTE);
    }
    let mut table = String::new();
    for exclusion in &diff.to_add {
        table.push_str(&listing_row("  + ", exclusion, None, styles));
    }
    for exclusion in &diff.to_remove {
        table.push_str(&listing_row("  - ", exclusion, None, styles));
    }
    // Everything the reconcile will not add, tagged with why. An exclusion
    // Defender already has but Patina does not own appears in this block.
    for exclusion in desired {
        let state = classifier.classify(exclusion);
        if !state.needs_add() {
            let tag = state_tag(state, styles);
            table.push_str(&listing_row("    ", exclusion, Some(&tag), styles));
        }
    }
    emit_aligned(&table, reporter);
    if diff.is_empty() && desired.is_empty() {
        reporter.line("  (no patina-owned exclusions)");
    }
}

/// One listing row: the add / remove / unchanged marker and the path in the
/// first cell, then the state tag in the next.
///
/// The marker shares the path's cell so it cannot widen the column.
fn listing_row(marker: &str, exclusion: &Exclusion, tag: Option<&str>, styles: &Styles) -> String {
    let path = format!("{marker}{}", path_by_kind(exclusion, styles));
    match tag {
        Some(tag) => row(&[path.as_str(), tag]),
        None => row(&[path.as_str()]),
    }
}

/// Render the status view: each desired exclusion as present / missing, plus
/// any patina-owned stale entries.
fn render_status(
    resolved: &ResolvedPlan,
    desired: &BTreeSet<Exclusion>,
    diff: &DefenderDiff,
    classifier: &ExclusionClassifier,
    reporter: &mut impl Reporter,
) {
    let styles = &reporter.styles();
    reporter.line(&format!("Defender exclusions for {}:", resolved.repo_root));
    if !classifier.live_list_was_read() {
        reporter.line(LEDGER_SOURCE_NOTE);
    }
    let mut table = String::new();
    for exclusion in desired {
        let tag = state_tag(classifier.classify(exclusion), styles);
        table.push_str(&listing_row("  ", exclusion, Some(&tag), styles));
    }
    let stale = stale_tag(styles);
    for exclusion in &diff.to_remove {
        table.push_str(&listing_row("  ", exclusion, Some(&stale), styles));
    }
    emit_aligned(&table, reporter);
}

/// Render the status view when the live read failed: the desired set only.
fn render_status_desired_only(
    resolved: &ResolvedPlan,
    desired: &BTreeSet<Exclusion>,
    reporter: &mut impl Reporter,
) {
    let styles = &reporter.styles();
    reporter.line(&format!(
        "Desired Defender exclusions for {} (current state unavailable):",
        resolved.repo_root
    ));
    let mut table = String::new();
    for exclusion in desired {
        table.push_str(&listing_row("  ", exclusion, None, styles));
    }
    emit_aligned(&table, reporter);
}

/// The reconcile JSON envelope: `repo_root`, `current_readable`, `to_add`,
/// `to_remove`, `result`, `detail`.
///
/// `repo_root` is `null` for `clear`: that verb does not plan a repository.
/// When Defender withheld the live list, `current_readable` is `false`. The
/// diff behind that envelope was computed against the ledger rather than
/// against Defender's list. [`status_json`]
/// emits the same field for the same reason. The three results that exit `1`
/// are separated by `result` itself. `detail` adds the helper's own words on
/// `blocked` and `failed`, the same text the human path puts in its error
/// message, and is an empty string on every other result rather than a missing
/// key.
fn reconcile_json(reconcile: &Reconcile<'_>, result: &str, detail: &str) -> String {
    let envelope = serde_json::json!({
        "repo_root": reconcile.repo_root.map(Utf8Path::as_str),
        "current_readable": reconcile.classifier.live_list_was_read(),
        "to_add": exclusions_json(&reconcile.diff.to_add),
        "to_remove": exclusions_json(&reconcile.diff.to_remove),
        "result": result,
        "detail": detail,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// The status JSON envelope: each desired exclusion with its `present` state,
/// plus the stale patina-owned entries.
fn status_json(
    resolved: &ResolvedPlan,
    desired: &BTreeSet<Exclusion>,
    diff: &DefenderDiff,
    classifier: &ExclusionClassifier,
) -> String {
    let exclusions: Vec<serde_json::Value> = desired
        .iter()
        .map(|exclusion| {
            serde_json::json!({
                "path": exclusion.path.as_str(),
                "kind": exclusion.kind.label(),
                "state": state_token(classifier.classify(exclusion)),
            })
        })
        .collect();
    let envelope = serde_json::json!({
        "repo_root": resolved.repo_root.as_str(),
        "exclusions": exclusions,
        "stale": exclusions_json(&diff.to_remove),
        "current_readable": classifier.live_list_was_read(),
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

/// The typed error for a blocked write: Defender returned success but the
/// helper's elevated re-read shows the exclusions did not change. The message
/// states the likely cause and a next step.
fn blocked_error(detail: &str) -> anyhow::Error {
    anyhow!(
        "Defender rejected the exclusion change; the write did not take \
         ({detail}). This usually means Tamper Protection is enabled or \
         Defender is managed by policy (Intune / GPO). Check \
         `Get-MpComputerStatus` (IsTamperProtected and AMRunningMode); apply \
         the exclusions through your management tool, or consider a Windows 11 \
         Dev Drive in Defender performance mode as a lower-risk alternative to \
         path exclusions."
    )
}

/// The typed error for a helper that never reached the point of applying: a
/// path it refused, an unreadable request file, PowerShell unavailable.
///
/// None of those is Defender declining the change, so this stays apart from
/// [`blocked_error`]. Sending the user to hunt for Tamper Protection over an
/// unrelated failure wastes their time.
fn failed_error(detail: &str) -> anyhow::Error {
    anyhow!("the elevated helper could not apply the Defender exclusions: {detail}")
}

/// The typed error for an apply whose verdict never arrived.
///
/// The message states only that the outcome is unknown. The helper did not
/// report before the deadline, so the exclusions may have been applied. The
/// reconcile is idempotent: a re-run is safe, and it writes the ledger entry
/// this outcome withheld.
fn unconfirmed_error() -> anyhow::Error {
    anyhow!(
        "the elevated helper did not report a result, so whether the Defender \
         exclusions changed is unknown. They may have been applied without \
         being recorded. Re-run `patina defender apply` (it is idempotent), or \
         check the live list with `Get-MpPreference` from an elevated shell."
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;
    use crate::output::reporter::assert_color_is_additive;
    use patina_core::CurrentExclusions;
    use patina_core::ExclusionKind;

    const REPO: &str = r"C:\Users\kevin\dotfiles";

    /// A reconcile derived from one live-list reading and one ledger, so the
    /// diff and the classifier cannot disagree the way hand-built parts could.
    ///
    /// The state directory is inert: every assertion in this module renders or
    /// serializes without reaching the filesystem.
    struct Fixture {
        desired: BTreeSet<Exclusion>,
        diff: DefenderDiff,
        classifier: ExclusionClassifier,
    }

    impl Fixture {
        fn new(
            desired: &[(&str, ExclusionKind)],
            current: &CurrentExclusions,
            ledger: &[(&str, ExclusionKind)],
        ) -> Self {
            let desired: BTreeSet<Exclusion> = desired
                .iter()
                .map(|(path, kind)| Exclusion::new(*path, *kind))
                .collect();
            let recorded: BTreeSet<Exclusion> = ledger
                .iter()
                .map(|(path, kind)| Exclusion::new(*path, *kind))
                .collect();
            Self {
                diff: plan_defender(&desired, current, &recorded),
                classifier: ExclusionClassifier::new(current, &recorded),
                desired,
            }
        }

        fn reconcile(&self) -> Reconcile<'_> {
            Reconcile {
                state_dir: Utf8Path::new(r"C:\state"),
                repo_root: Some(Utf8Path::new(REPO)),
                desired: &self.desired,
                diff: &self.diff,
                classifier: &self.classifier,
            }
        }

        /// Render the preview with the plain palette and return stdout.
        fn preview(&self) -> String {
            let mut reporter = BufferReporter::new();
            render_preview(&self.reconcile(), &mut reporter);
            reporter.out
        }
    }

    fn known(paths: &[&str]) -> CurrentExclusions {
        CurrentExclusions::Known(paths.iter().map(camino::Utf8PathBuf::from).collect())
    }

    const GITCONFIG: &str = r"C:\Users\kevin\.gitconfig";

    #[test]
    fn preview_from_defender_claims_presence_and_says_nothing_about_the_ledger() {
        let fixture = Fixture::new(
            &[
                (REPO, ExclusionKind::Folder),
                (GITCONFIG, ExclusionKind::File),
            ],
            &known(&[REPO]),
            &[(REPO, ExclusionKind::Folder)],
        );
        let out = fixture.preview();

        assert!(out.contains(&format!("  + {GITCONFIG}")));
        assert!(out.contains("[present]"));
        assert!(
            !out.contains(LEDGER_SOURCE_NOTE),
            "a list read from Defender needs no ledger caveat: {out}"
        );
    }

    #[test]
    fn preview_from_the_ledger_never_claims_an_exclusion_is_present() {
        // The regression guard for the whole feature: unprivileged, Patina has
        // not seen Defender's list, so it must not report `present`, only that
        // it recorded the exclusion itself.
        let fixture = Fixture::new(
            &[(REPO, ExclusionKind::Folder)],
            &CurrentExclusions::Unreadable,
            &[(REPO, ExclusionKind::Folder)],
        );
        let out = fixture.preview();

        assert!(out.contains(LEDGER_SOURCE_NOTE));
        assert!(out.contains("[recorded]"));
        assert!(
            !out.contains("[present]"),
            "an inferred state must not be rendered as an observed one: {out}"
        );
    }

    #[test]
    fn an_exclusion_defender_has_that_patina_does_not_record_is_called_out() {
        // Defender already excludes the path, so nothing will be added, but the
        // ledger does not own it. That determines whether `clear` can ever reap
        // it.
        let fixture = Fixture::new(&[(REPO, ExclusionKind::Folder)], &known(&[REPO]), &[]);
        let out = fixture.preview();

        assert!(
            out.contains("[present, not recorded by patina]"),
            "an unowned exclusion must say so: {out}"
        );
        assert!(
            fixture.diff.to_add.is_empty(),
            "it is already excluded, so nothing is added: {:?}",
            fixture.diff
        );
    }

    #[test]
    fn an_unowned_exclusion_does_not_render_the_same_as_an_owned_one() {
        // Both are present in Defender and neither is added, so without the
        // distinct state the listing would show them identically.
        let owned = Fixture::new(
            &[(REPO, ExclusionKind::Folder)],
            &known(&[REPO]),
            &[(REPO, ExclusionKind::Folder)],
        );
        let unowned = Fixture::new(&[(REPO, ExclusionKind::Folder)], &known(&[REPO]), &[]);
        assert_ne!(owned.preview(), unowned.preview());
    }

    #[test]
    fn adoptable_counts_only_the_exclusions_the_ledger_does_not_own() {
        // What the "already up to date" line reports. The repo folder is
        // already Patina's; the two files are excluded but unrecorded.
        let fixture = Fixture::new(
            &[
                (REPO, ExclusionKind::Folder),
                (GITCONFIG, ExclusionKind::File),
                (r"C:\Users\kevin\.zshrc", ExclusionKind::File),
            ],
            &known(&[REPO, GITCONFIG, r"C:\Users\kevin\.zshrc"]),
            &[(REPO, ExclusionKind::Folder)],
        );
        assert_eq!(fixture.reconcile().adoptable(), 2);
        assert!(
            fixture.diff.is_empty(),
            "adoption happens on the no-op path, so the diff must be empty: {:?}",
            fixture.diff
        );
    }

    #[test]
    fn nothing_is_adoptable_when_the_live_list_was_withheld() {
        // Unmanaged is undetectable unprivileged, so the count must not guess.
        let fixture = Fixture::new(
            &[(REPO, ExclusionKind::Folder)],
            &CurrentExclusions::Unreadable,
            &[],
        );
        assert_eq!(fixture.reconcile().adoptable(), 0);
    }

    #[test]
    fn reconcile_json_reports_whether_the_live_list_was_readable() {
        let readable = Fixture::new(&[], &known(&[]), &[]);
        let withheld = Fixture::new(&[], &CurrentExclusions::Unreadable, &[]);

        assert!(
            reconcile_json(&readable.reconcile(), "applied", "")
                .contains("\"current_readable\": true")
        );
        assert!(
            reconcile_json(&withheld.reconcile(), "applied", "")
                .contains("\"current_readable\": false")
        );
    }

    #[test]
    fn reconcile_json_sets_detail_on_a_blocked_result_and_empties_it_otherwise() {
        // `blocked`, `failed`, and `unconfirmed` all exit 1, so `result` is
        // what separates them in the envelope. `detail` adds the helper's own
        // words on the results that have a reason to report.
        let fixture = Fixture::new(&[], &known(&[]), &[]);
        let blocked = reconcile_json(
            &fixture.reconcile(),
            "blocked",
            "exclusions not applied (TamperProtected=True)",
        );
        assert!(blocked.contains("TamperProtected=True"), "{blocked}");
        assert!(
            reconcile_json(&fixture.reconcile(), "applied", "").contains("\"detail\": \"\""),
            "a result with nothing to explain carries an empty detail, not a missing key"
        );
    }

    #[test]
    fn only_a_rejected_write_is_reported_as_defender_refusing_it() {
        // `Blocked`, `Failed`, and `Unconfirmed` share a code, so the message is
        // the only thing separating them. Blaming Tamper Protection for an
        // unconfirmed outcome is the bug this split fixes.
        let blocked = blocked_error("TamperProtected=True").to_string();
        assert!(blocked.contains("Defender rejected the exclusion change"));
        assert!(blocked.contains("Tamper Protection"));
        assert!(blocked.contains("TamperProtected=True"));

        for other in [
            unconfirmed_error().to_string(),
            failed_error("refusing to exclude `C:\\`").to_string(),
        ] {
            assert!(
                !other.contains("rejected") && !other.contains("Tamper Protection"),
                "only a verified rejection may blame Defender: {other}"
            );
        }
    }

    #[test]
    fn an_unconfirmed_outcome_says_the_state_is_unknown_and_a_re_run_is_safe() {
        let message = unconfirmed_error().to_string();
        assert!(message.contains("unknown"));
        assert!(message.contains("patina defender apply"));
    }

    #[test]
    fn a_failed_outcome_carries_the_helpers_own_detail() {
        assert!(
            failed_error("refusing to exclude `C:\\`")
                .to_string()
                .contains("refusing to exclude `C:\\`")
        );
    }

    /// Every exclusion state, listed by hand. Adding a variant is caught by the
    /// wildcard-free matches in [`state_label`] and [`state_token`]; this array
    /// checks that the new wording and token do not collide with an existing
    /// one, so it has to be extended alongside them.
    const ALL_STATES: [ExclusionState; 5] = [
        ExclusionState::Owned,
        ExclusionState::Unmanaged,
        ExclusionState::Absent,
        ExclusionState::Recorded,
        ExclusionState::Unrecorded,
    ];

    #[test]
    fn every_state_has_its_own_label_and_token() {
        // Two states sharing a label would be indistinguishable in the listing;
        // sharing a token would be indistinguishable to a `--json` consumer.
        let labels: BTreeSet<&str> = ALL_STATES.into_iter().map(state_label).collect();
        let tokens: BTreeSet<&str> = ALL_STATES.into_iter().map(state_token).collect();
        assert_eq!(labels.len(), ALL_STATES.len(), "labels collide: {labels:?}");
        assert_eq!(tokens.len(), ALL_STATES.len(), "tokens collide: {tokens:?}");
    }

    #[test]
    fn the_three_readable_states_are_colored_apart() {
        // Under a readable live list, Owned, Unmanaged, and Absent are what
        // the user has to tell apart. Color is the only thing separating the
        // two present states beyond their wording.
        let styles = Styles::colored();
        let escapes: BTreeSet<String> = [
            ExclusionState::Owned,
            ExclusionState::Unmanaged,
            ExclusionState::Absent,
        ]
        .into_iter()
        .map(|state| state_tag(state, &styles).replace(state_label(state), "LABEL"))
        .collect();
        assert_eq!(escapes.len(), 3, "the three states must paint apart");
    }

    #[test]
    fn the_plain_palette_leaves_a_path_and_tag_byte_identical_to_unstyled() {
        // What every other assertion in this module relies on: a plain render
        // does not emit escapes, so a path or label reads back verbatim.
        let file = Exclusion::new(r"C:\a", ExclusionKind::File);
        assert_eq!(path_by_kind(&file, &Styles::plain()), r"C:\a");
        assert_eq!(
            state_tag(ExclusionState::Recorded, &Styles::plain()),
            "[recorded]"
        );
        assert_eq!(
            stale_tag(&Styles::plain()),
            "[stale: patina-owned, no longer managed]"
        );
    }

    #[test]
    fn a_file_path_and_a_folder_path_are_colored_differently() {
        // Color is the only place the kind appears in human output, so if the
        // two kinds painted alike the distinction would be gone entirely.
        let styles = Styles::colored();
        let same_path = r"C:\a";
        let file = path_by_kind(&Exclusion::new(same_path, ExclusionKind::File), &styles);
        let folder = path_by_kind(&Exclusion::new(same_path, ExclusionKind::Folder), &styles);

        assert!(file.contains('\u{1b}') && folder.contains('\u{1b}'));
        assert_ne!(
            file, folder,
            "the same path must paint differently by kind: that color is the only signal"
        );
    }

    #[test]
    fn a_painted_path_keeps_the_path_contiguous() {
        // The path is painted whole, so it stays greppable and copy-pasteable
        // out of a terminal. Painting per-component would break both.
        let styles = Styles::colored();
        let path = r"C:\Users\kevin\.gitconfig";
        let painted = path_by_kind(&Exclusion::new(path, ExclusionKind::File), &styles);
        assert!(painted.contains(path), "escapes must not split the path");
    }

    #[test]
    fn a_colored_preview_still_reads_as_the_plain_text_it_paints() {
        // Path and tag are each painted whole, so a substring search for either
        // survives the escapes. A regression painting the brackets separately
        // would slip an escape into the middle of `[recorded]` and break every
        // consumer that greps this output.
        let fixture = Fixture::new(
            &[(REPO, ExclusionKind::Folder)],
            &CurrentExclusions::Unreadable,
            &[(REPO, ExclusionKind::Folder)],
        );
        let mut reporter = BufferReporter::colored();
        render_preview(&fixture.reconcile(), &mut reporter);

        assert!(reporter.out.contains(REPO));
        assert!(reporter.out.contains("[recorded]"));
        assert_color_is_additive(|reporter| {
            render_preview(&fixture.reconcile(), reporter);
        });
    }

    /// Every desired exclusion must reach the listing with its state tag. A
    /// dropped row hides a Defender exclusion from the only listing that
    /// reports it.
    #[test]
    fn a_listing_carries_every_desired_exclusion_and_its_tag() {
        let short = r"C:\a";
        let fixture = Fixture::new(
            &[
                (short, ExclusionKind::File),
                (GITCONFIG, ExclusionKind::File),
            ],
            &known(&[short, GITCONFIG]),
            &[
                (short, ExclusionKind::File),
                (GITCONFIG, ExclusionKind::File),
            ],
        );
        let out = fixture.preview();
        let tagged: Vec<&str> = out
            .lines()
            .filter(|line| line.contains("[present]"))
            .collect();

        assert_eq!(tagged.len(), 2, "both exclusions must be listed: {out}");
        for path in [short, GITCONFIG] {
            assert!(
                tagged.iter().any(|line| line.contains(path)),
                "{path} must be listed with its tag: {out}"
            );
        }
    }
}
