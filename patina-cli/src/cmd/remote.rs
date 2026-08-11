//! `patina remote` command logic.
//!
//! Four verbs over the remote-source subsystem: `list` (offline, read-only),
//! `check` (`ls-remote` only, refreshes the pending-update notice), `update`
//! (the producer verb — fetches, runs the update gate, rewrites the lockfile),
//! and `prune` (sweeps unreferenced checkouts).
//!
//! The engine owns the semantics: [`patina_core::remote::update`] computes a
//! proposal per remote and [`patina_core::remote::gate`] decides. This module
//! is control flow, lock acquisition, prompting, and output — all of it through
//! the [`Reporter`].
//!
//! Locking follows the read/write split the rest of the CLI uses. `update` and
//! `prune` mutate (the working-tree lockfile, the cache) and take the exclusive
//! lock; `list` and `check` take the shared lock with the read-only escape
//! hatch, since `check` writes only the per-machine notice files it alone owns
//! — a shell hook must never contend with a running apply.
//!
//! See `docs/REMOTE_SOURCES.md` "Commands", "The update gate", and
//! "Shell integration".

use crate::cli::RemoteArgs;
use crate::cli::RemoteCommand;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use patina_core::LockKind;
use patina_core::SHARED_TIMEOUT;
use patina_core::acquire_lock;
use patina_core::exclusive_timeout;
use patina_core::remote::cache;
use patina_core::remote::gate::GateConcern;
use patina_core::remote::gate::GateOutcome;
use patina_core::remote::notice;
use patina_core::remote::update;
use patina_core::remote::update::Proposal;
use patina_core::remote::update::RemoteInventory;
use patina_core::remote::update::RemoteView;

/// Run `patina remote`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error when repository / state-directory resolution, module
/// enumeration, the lockfile read, or lock acquisition fails. A remote that is
/// individually unreachable or held back by the gate is not an error: it is
/// reported and the run continues, so one bad remote never blocks the rest.
pub fn run(
    args: &RemoteArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    // `check --hook` decides whether to do anything at all before paying for
    // repository discovery, so a shell hook on a machine with no remotes costs
    // one stat.
    let inventory = update::inventory().context("failed to enumerate the declared remotes")?;
    let lock_path = inventory.state_dir.join("lock");

    match &args.command {
        RemoteCommand::List => {
            let _guard = shared_lock(&lock_path, reporter);
            Ok(run_list(&inventory, args.json, reporter))
        }
        RemoteCommand::Check { hook } => {
            let _guard = shared_lock(&lock_path, reporter);
            Ok(run_check(&inventory, *hook, args.json, reporter))
        }
        RemoteCommand::Update { name, now, yes } => {
            let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
                .context("failed to acquire the exclusive lock for `patina remote update`")?;
            let mut inventory = inventory;
            run_update(
                &mut inventory,
                &UpdateFlags {
                    name: name.clone(),
                    bypass_age: *now,
                    yes: *yes,
                    json: args.json,
                },
                tty,
                reader,
                reporter,
            )
        }
        RemoteCommand::Prune => {
            let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
                .context("failed to acquire the exclusive lock for `patina remote prune`")?;
            run_prune(&inventory, args.json, reporter)
        }
    }
}

/// Acquire the shared lock, warning and proceeding on a timeout — the read-only
/// escape hatch `patina status` and `patina doctor` also use.
fn shared_lock(
    lock_path: &camino::Utf8Path,
    reporter: &mut impl Reporter,
) -> Option<patina_core::LockGuard> {
    match acquire_lock(lock_path, LockKind::Shared, SHARED_TIMEOUT) {
        Ok(guard) => Some(guard),
        Err(error) => {
            reporter.warn(&format!("proceeding without the shared lock: {error}"));
            None
        }
    }
}

/// `patina remote list` — the pins as recorded, with the pending state the last
/// `check` observed.
fn run_list(inventory: &RemoteInventory, json: bool, reporter: &mut impl Reporter) -> i32 {
    let pending = notice::read_pending(&inventory.state_dir);
    if json {
        let rows: Vec<serde_json::Value> = inventory
            .remotes
            .iter()
            .map(|view| {
                serde_json::json!({
                    "module": view.module,
                    "url": view.spec.url,
                    "ref": view.spec.git_ref,
                    "rev": view.pin.as_ref().map(|pin| pin.rev.clone()),
                    "updated_at": view.pin.as_ref().map(|pin| pin.updated_at.clone()),
                    "pending": pending.contains(&view.module),
                })
            })
            .collect();
        reporter.json(&document(&serde_json::json!({ "remotes": rows })));
        return ExitCode::Success.code();
    }

    if inventory.remotes.is_empty() {
        reporter.line("No remote-backed modules are declared.");
        return ExitCode::Success.code();
    }
    for view in &inventory.remotes {
        let git_ref = view.spec.git_ref.as_deref().unwrap_or("(default branch)");
        let rev = view
            .pin
            .as_ref()
            .map_or("(unpinned)", |pin| pin.rev.as_str());
        let state = if pending.contains(&view.module) {
            "  update pending"
        } else {
            ""
        };
        reporter.line(&format!(
            "{}  {}  {}  {}{}",
            view.module, view.spec.url, git_ref, rev, state
        ));
    }
    ExitCode::Success.code()
}

/// `patina remote check` — `ls-remote` every remote and refresh the notice.
fn run_check(
    inventory: &RemoteInventory,
    hook: bool,
    json: bool,
    reporter: &mut impl Reporter,
) -> i32 {
    let now = patina_core::current_epoch_seconds();
    if hook && !notice::hook_check_due(notice::last_check_epoch(&inventory.state_dir), now) {
        // Inside the throttle window: no network, no output. The notice already
        // on disk stays as it is, so the shell still prints it.
        return ExitCode::Success.code();
    }

    let mut behind: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for view in &inventory.remotes {
        match update::check_upstream(view) {
            Ok(result) if result.has_update() => behind.push(result.module),
            Ok(_) => {}
            Err(error) => failures.push(format!("{}: {error}", view.module)),
        }
    }

    // A repository behind its own origin may already carry pins another machine
    // gated and committed. Pulling those is the right move, so that message wins
    // over "run the gate here".
    let repo_behind = patina_core::remote::git::repo_differs_from_origin(&inventory.repo_root);
    let message = if repo_behind {
        Some(notice::repo_behind_message())
    } else if behind.is_empty() {
        None
    } else {
        let names: Vec<&str> = behind.iter().map(String::as_str).collect();
        Some(notice::pending_updates_message(&names))
    };

    // A write failure here must not fail a shell hook, so it is reported and the
    // command still succeeds.
    for error in [
        notice::write_notice(&inventory.state_dir, message.as_deref()).err(),
        notice::write_pending(&inventory.state_dir, &behind).err(),
        notice::record_check(&inventory.state_dir, now).err(),
    ]
    .into_iter()
    .flatten()
    {
        if !hook {
            reporter.warn(&format!(
                "failed to update the remote notice state: {error}"
            ));
        }
    }

    if hook {
        // Fully silent on success: the shell prints the notice file itself.
        return ExitCode::Success.code();
    }

    if json {
        reporter.json(&document(&serde_json::json!({
            "pending": behind,
            "repo_behind": repo_behind,
            "failed": failures,
        })));
        return ExitCode::Success.code();
    }

    for failure in &failures {
        reporter.warn(&format!("could not check remote {failure}"));
    }
    match message {
        Some(text) => reporter.line(text.trim_end()),
        None => reporter.line("Every remote is at its pinned rev."),
    }
    ExitCode::Success.code()
}

/// The parsed `patina remote update` flags, grouped so the runner does not take
/// four positional booleans.
struct UpdateFlags {
    name: Option<String>,
    bypass_age: bool,
    yes: bool,
    json: bool,
}

/// `patina remote update` — fetch, gate, and bump pins in the working-tree
/// lockfile.
fn run_update(
    inventory: &mut RemoteInventory,
    flags: &UpdateFlags,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    if flags.bypass_age {
        reporter.warn(
            "`--now` bypasses the age gate for this run: pins may move to commits \
             younger than their min_age",
        );
    }

    let views: Vec<RemoteView> = if let Some(name) = &flags.name {
        let Some(view) = inventory.find(name) else {
            reporter.warn(&format!(
                "no remote-backed module named `{name}` is declared in this repository"
            ));
            return Ok(ExitCode::Generic.code());
        };
        vec![view.clone()]
    } else {
        inventory.remotes.clone()
    };

    if views.is_empty() {
        reporter.line("No remote-backed modules are declared.");
        return Ok(ExitCode::Success.code());
    }

    let now_epoch = patina_core::current_epoch_seconds();
    let now_rfc3339 = patina_core::current_rfc3339();
    let mut rows = Vec::new();
    let mut rejected = false;

    for view in &views {
        let proposal = match update::propose(inventory, view, now_epoch, flags.bypass_age) {
            Ok(proposal) => proposal,
            Err(error) => {
                // One unreachable remote must not stop the others.
                reporter.warn(&format!("could not update remote {}: {error}", view.module));
                rows.push(row(&view.module, "failed", None));
                rejected = true;
                continue;
            }
        };
        let action = settle(
            inventory,
            view,
            &proposal,
            &now_rfc3339,
            flags,
            tty,
            reader,
            reporter,
        )?;
        if action == Action::Rejected {
            rejected = true;
        }
        rows.push(row(
            &view.module,
            action.label(),
            Some(&proposal.candidate_rev),
        ));
    }

    if flags.json {
        reporter.json(&document(&serde_json::json!({ "remotes": rows })));
    }
    Ok(if rejected {
        ExitCode::Generic.code()
    } else {
        ExitCode::Success.code()
    })
}

/// What happened to one remote's proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Already at the upstream tip.
    UpToDate,
    /// The pin moved.
    Updated,
    /// The gate held it back, or the user declined.
    Held,
    /// A hard reject.
    Rejected,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::Updated => "updated",
            Self::Held => "held",
            Self::Rejected => "rejected",
        }
    }
}

/// Act on one proposal: bump the pin, prompt first, or report why not.
#[expect(
    clippy::too_many_arguments,
    reason = "settling one proposal needs the inventory it writes into, the view and \
              proposal it is about, the timestamp to stamp, the invocation flags, and the \
              three output/input seams (tty, reader, reporter). Grouping them behind a \
              struct would move the same fields without removing any."
)]
fn settle(
    inventory: &mut RemoteInventory,
    view: &RemoteView,
    proposal: &Proposal,
    now_rfc3339: &str,
    flags: &UpdateFlags,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<Action> {
    match &proposal.outcome {
        GateOutcome::AlreadyPinned => {
            if !flags.json {
                reporter.line(&format!(
                    "{}: already at {}",
                    proposal.module, proposal.candidate_rev
                ));
            }
            Ok(Action::UpToDate)
        }
        GateOutcome::Allowed => {
            bump(inventory, view, proposal, now_rfc3339, flags.json, reporter)?;
            Ok(Action::Updated)
        }
        GateOutcome::RejectedFuture { .. } => {
            reporter.warn(&format!(
                "{}: refusing {} — its committer time is more than an hour ahead of this \
                 machine's clock",
                proposal.module, proposal.candidate_rev
            ));
            Ok(Action::Rejected)
        }
        GateOutcome::Cooldown { eligible_at } => {
            if !flags.json {
                reporter.line(&format!(
                    "{}: holding {} until {} (min_age not yet met); the pin is unchanged",
                    proposal.module,
                    proposal.candidate_rev,
                    format_epoch(*eligible_at)
                ));
            }
            Ok(Action::Held)
        }
        GateOutcome::NeedsConfirmation(concerns) => {
            if confirm(&proposal.module, concerns, flags.yes, tty, reader, reporter) {
                bump(inventory, view, proposal, now_rfc3339, flags.json, reporter)?;
                Ok(Action::Updated)
            } else {
                if !flags.json {
                    reporter.line(&format!(
                        "{}: pin unchanged at {}",
                        proposal.module,
                        proposal.current_rev.as_deref().unwrap_or("(unpinned)")
                    ));
                }
                Ok(Action::Held)
            }
        }
        // `GateOutcome` is `#[non_exhaustive]`. A verdict this binary does not
        // recognize must not be treated as permission to move a pin.
        _ => {
            reporter.warn(&format!(
                "{}: the update gate returned a verdict this patina does not recognize; \
                 the pin is unchanged",
                proposal.module
            ));
            Ok(Action::Held)
        }
    }
}

/// Write one accepted proposal into the lockfile and report it.
fn bump(
    inventory: &mut RemoteInventory,
    view: &RemoteView,
    proposal: &Proposal,
    now_rfc3339: &str,
    json: bool,
    reporter: &mut impl Reporter,
) -> Result<()> {
    update::accept(inventory, view, proposal, now_rfc3339)
        .map_err(patina_core::EngineError::from)
        .context("failed to write patina.lock")?;
    if !json {
        reporter.line(&format!(
            "{}: {} -> {}",
            proposal.module,
            proposal.current_rev.as_deref().unwrap_or("(unpinned)"),
            proposal.candidate_rev
        ));
    }
    Ok(())
}

/// Ask whether to accept a flagged pin bump.
///
/// `--yes` accepts without asking. A non-interactive shell cannot be asked, so
/// it declines: leaving the pin where it is, is the safe answer for a check
/// that exists to catch a rewritten or backdated upstream.
fn confirm(
    module: &str,
    concerns: &[GateConcern],
    yes: bool,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> bool {
    for concern in concerns {
        reporter.warn(&format!("{module}: {}", concern.describe()));
    }
    if yes {
        return true;
    }
    if tty == Tty::NonInteractive {
        reporter.warn(&format!(
            "{module}: cannot confirm in a non-interactive shell; re-run with --yes to accept"
        ));
        return false;
    }
    reporter.confirm(&format!("Bump the pin for {module} anyway?"));
    matches!(reader.read_line().unwrap_or_default().trim(), "y" | "Y")
}

/// `patina remote prune` — the reachability sweep, run by hand.
fn run_prune(inventory: &RemoteInventory, json: bool, reporter: &mut impl Reporter) -> Result<i32> {
    let removed = cache::prune(&inventory.state_dir)
        .map_err(patina_core::EngineError::from)
        .context("failed to prune the remote checkout cache")?;
    if json {
        let paths: Vec<&str> = removed.iter().map(|path| path.as_str()).collect();
        reporter.json(&document(&serde_json::json!({ "removed": paths })));
    } else if removed.is_empty() {
        reporter.line("No unreferenced remote checkouts to remove.");
    } else {
        for path in &removed {
            reporter.line(&format!("removed {path}"));
        }
    }
    Ok(ExitCode::Success.code())
}

/// One `remotes` row of the `remote update` JSON envelope.
fn row(module: &str, action: &str, rev: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "module": module, "action": action, "rev": rev })
}

/// Serialize a JSON envelope, falling back to an empty object so a
/// serialization failure cannot abort a command whose real work is done.
fn document(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned())
}

/// Render Unix seconds as an RFC 3339 UTC instant for the cooldown message.
fn format_epoch(epoch: i64) -> String {
    jiff::Timestamp::from_second(epoch).map_or_else(
        |_| epoch.to_string(),
        |ts| ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;

    /// A reader that answers with a scripted line, then EOF.
    struct ScriptedReader(Option<String>);

    impl PromptReader for ScriptedReader {
        fn read_line(&mut self) -> Option<String> {
            self.0.take()
        }
    }

    fn concerns() -> Vec<GateConcern> {
        vec![GateConcern::HistoryRewritten]
    }

    #[test]
    fn yes_accepts_without_reading_stdin() {
        let mut reader = ScriptedReader(None);
        let mut reporter = BufferReporter::new();
        assert!(confirm(
            "humanizer",
            &concerns(),
            true,
            Tty::Interactive,
            &mut reader,
            &mut reporter
        ));
        assert!(
            reporter.err.contains("history was rewritten"),
            "the concern must still be reported under --yes: {}",
            reporter.err
        );
    }

    #[test]
    fn a_non_interactive_shell_declines_and_says_how_to_proceed() {
        let mut reader = ScriptedReader(Some("y\n".to_owned()));
        let mut reporter = BufferReporter::new();
        assert!(
            !confirm(
                "humanizer",
                &concerns(),
                false,
                Tty::NonInteractive,
                &mut reader,
                &mut reporter
            ),
            "a flagged bump must not be accepted without a confirmation"
        );
        assert!(
            reporter.err.contains("--yes"),
            "the message must name the flag that would accept it: {}",
            reporter.err
        );
    }

    #[test]
    fn an_interactive_y_accepts_and_anything_else_declines() {
        for (answer, expected) in [("y\n", true), ("Y\n", true), ("n\n", false), ("", false)] {
            let mut reader = ScriptedReader(Some(answer.to_owned()));
            let mut reporter = BufferReporter::new();
            assert_eq!(
                confirm(
                    "humanizer",
                    &concerns(),
                    false,
                    Tty::Interactive,
                    &mut reader,
                    &mut reporter
                ),
                expected,
                "answer {answer:?} must map to {expected}"
            );
            assert!(
                reporter.err.contains("Bump the pin for humanizer anyway?"),
                "the interactive path must prompt: {}",
                reporter.err
            );
        }
    }

    #[test]
    fn a_cooldown_instant_renders_as_an_rfc_3339_utc_timestamp() {
        assert_eq!(format_epoch(1_786_456_800), "2026-08-11T14:00:00Z");
    }

    #[test]
    fn action_labels_are_the_json_contract() {
        // The `action` field is what a script branches on, so the labels are
        // part of the envelope contract rather than prose.
        assert_eq!(Action::UpToDate.label(), "up_to_date");
        assert_eq!(Action::Updated.label(), "updated");
        assert_eq!(Action::Held.label(), "held");
        assert_eq!(Action::Rejected.label(), "rejected");
    }
}
