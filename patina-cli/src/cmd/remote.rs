//! `patina remote` command logic.
//!
//! Four verbs over the remote-source subsystem: `list` (offline, read-only),
//! `check` (`ls-remote` only, refreshes the pending-update notice), `update`
//! (the producer verb that fetches, runs the update gate, and rewrites the
//! lockfile), and `prune` (sweeps unreferenced checkouts).
//!
//! The engine owns the semantics: [`patina_core::remote::update`] computes a
//! proposal per remote and [`patina_core::remote::gate`] decides. This module
//! is control flow, lock acquisition, prompting, and output, all of it through
//! the [`Reporter`].
//!
//! Locking follows the read/write split the rest of the CLI uses. `update` and
//! `prune` mutate (the working-tree lockfile, the cache) and take the exclusive
//! lock; `list` and `check` take the shared lock with the read-only escape
//! hatch, since `check` writes only the per-machine notice files it alone owns.
//! A shell hook must never contend with a running apply.
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
/// Returns an error when repository / state-directory resolution,
/// root-manifest parsing, the lockfile read, or lock acquisition fails. A
/// remote that is individually unreachable or held back by the gate is not an
/// error: it is reported and the run continues, so one bad remote never blocks
/// the rest.
pub fn run(
    args: &RemoteArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let state_dir =
        patina_core::resolve_state_dir().context("failed to resolve the state directory")?;
    let lock_path = state_dir.join("lock");

    match &args.command {
        RemoteCommand::List => {
            let _guard = shared_lock(&lock_path, reporter);
            let inventory = declared_remotes()?;
            Ok(run_list(&inventory, args.json, reporter))
        }
        RemoteCommand::Check { hook } => {
            let _guard = shared_lock(&lock_path, reporter);
            let inventory = declared_remotes()?;
            Ok(run_check(&inventory, *hook, args.json, reporter))
        }
        RemoteCommand::Update { name, now, yes } => {
            let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
                .context("failed to acquire the exclusive lock for `patina remote update`")?;
            // Read the lockfile only once the exclusive lock is held: this is a
            // read-modify-write of a file another `patina remote update` may be
            // rewriting, so a read taken before the lock could silently drop a
            // concurrent bump.
            let mut inventory = declared_remotes()?;
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
            let inventory = declared_remotes()?;
            run_prune(&inventory, args.json, reporter)
        }
    }
}

/// Enumerate the root manifest's declared remotes and their pins.
fn declared_remotes() -> Result<RemoteInventory> {
    update::inventory().context("failed to enumerate the declared remotes")
}

/// Acquire the shared lock, warning and proceeding on a timeout: the read-only
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

/// Report the pins as recorded, with the pending state the last
/// `check` observed.
fn run_list(inventory: &RemoteInventory, json: bool, reporter: &mut impl Reporter) -> i32 {
    let pending = notice::read_pending(&inventory.state_dir);
    if json {
        let rows: Vec<serde_json::Value> = inventory
            .remotes
            .iter()
            .map(|view| {
                serde_json::json!({
                    "name": view.name,
                    "url": view.spec.url,
                    "ref": view.spec.git_ref,
                    "rev": view.pin.as_ref().map(|pin| pin.rev.clone()),
                    "updated_at": view.pin.as_ref().map(|pin| pin.updated_at.clone()),
                    "pending": pending.contains(&view.name),
                })
            })
            .collect();
        reporter.json(&document(&serde_json::json!({ "remotes": rows })));
        return ExitCode::Success.code();
    }

    if inventory.remotes.is_empty() {
        reporter.line("No remotes are declared.");
        return ExitCode::Success.code();
    }
    for view in &inventory.remotes {
        let git_ref = view.spec.git_ref.as_deref().unwrap_or("(default branch)");
        let rev = view
            .pin
            .as_ref()
            .map_or("(unpinned)", |pin| pin.rev.as_str());
        let state = if pending.contains(&view.name) {
            "  update pending"
        } else {
            ""
        };
        reporter.line(&format!(
            "{}  {}  {}  {}{}",
            view.name, view.spec.url, git_ref, rev, state
        ));
    }
    ExitCode::Success.code()
}

/// Run `ls-remote` against every remote and refresh the notice.
fn run_check(
    inventory: &RemoteInventory,
    hook: bool,
    json: bool,
    reporter: &mut impl Reporter,
) -> i32 {
    let now = patina_core::current_epoch_seconds();
    if hook && !notice::hook_check_due(notice::last_check_epoch(&inventory.state_dir), now) {
        // The notice already on disk stays as it is, so the shell still prints it.
        return ExitCode::Success.code();
    }

    let mut behind: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for view in &inventory.remotes {
        match update::check_upstream(view) {
            Ok(result) if result.has_update() => behind.push(result.name),
            Ok(_) => {}
            Err(error) => failures.push(format!("{}: {error}", view.name)),
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

    // A remote that could not be reached is a failed check, not a clean one, so
    // a script can tell "nothing pending" from "checked nothing". Matches the
    // exit `patina remote update` returns for the same failure.
    let exit = if failures.is_empty() {
        ExitCode::Success
    } else {
        ExitCode::Generic
    };

    if json {
        reporter.json(&document(&serde_json::json!({
            "pending": behind,
            "repo_behind": repo_behind,
            "failed": failures,
        })));
        return exit.code();
    }

    for failure in &failures {
        reporter.warn(&format!("could not check remote {failure}"));
    }
    match message {
        Some(text) => reporter.line(text.trim_end()),
        None => reporter.line("Every remote is at its pinned rev."),
    }
    exit.code()
}

/// The parsed `patina remote update` flags, grouped so the runner does not take
/// four positional booleans.
struct UpdateFlags {
    name: Option<String>,
    bypass_age: bool,
    yes: bool,
    json: bool,
}

/// Fetch, gate, and bump pins in the working-tree
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
                "no remote named `{name}` is declared in this repository's root patina.toml"
            ));
            return Ok(ExitCode::Generic.code());
        };
        vec![view.clone()]
    } else {
        inventory.remotes.clone()
    };

    if views.is_empty() {
        reporter.line("No remotes are declared.");
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
                reporter.warn(&format!("could not update remote {}: {error}", view.name));
                rows.push(row(&view.name, "failed", None));
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
            &view.name,
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
                    proposal.name, proposal.candidate_rev
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
                "{}: refusing {} because its committer time is more than an hour ahead of this \
                 machine's clock",
                proposal.name, proposal.candidate_rev
            ));
            Ok(Action::Rejected)
        }
        GateOutcome::Cooldown { eligible_at } => {
            if !flags.json {
                reporter.line(&format!(
                    "{}: holding {} until {} (min_age not yet met); the pin is unchanged",
                    proposal.name,
                    proposal.candidate_rev,
                    format_epoch(*eligible_at)
                ));
            }
            Ok(Action::Held)
        }
        GateOutcome::NeedsConfirmation(concerns) => {
            if confirm(&proposal.name, concerns, flags.yes, tty, reader, reporter) {
                bump(inventory, view, proposal, now_rfc3339, flags.json, reporter)?;
                Ok(Action::Updated)
            } else {
                if !flags.json {
                    reporter.line(&format!(
                        "{}: pin unchanged at {}",
                        proposal.name,
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
                proposal.name
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
            proposal.name,
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
    name: &str,
    concerns: &[GateConcern],
    yes: bool,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> bool {
    for concern in concerns {
        reporter.warn(&format!("{name}: {}", concern.describe()));
    }
    if yes {
        return true;
    }
    if tty == Tty::NonInteractive {
        reporter.warn(&format!(
            "{name}: cannot confirm in a non-interactive shell; re-run with --yes to accept"
        ));
        return false;
    }
    reporter.confirm(&format!("Bump the pin for {name} anyway?"));
    matches!(reader.read_line().unwrap_or_default().trim(), "y" | "Y")
}

/// Run the cache sweep by hand: whole trees for remotes the root manifest no
/// longer declares, then unreferenced checkouts of the ones it does.
fn run_prune(inventory: &RemoteInventory, json: bool, reporter: &mut impl Reporter) -> Result<i32> {
    let state_dir = &inventory.state_dir;
    let declared = inventory
        .remotes
        .iter()
        .map(|view| view.name.as_str())
        .collect();
    let mut removed = cache::prune_undeclared(state_dir, &declared)
        .map_err(patina_core::EngineError::from)
        .context("failed to prune the cache of undeclared remotes")?;
    removed.extend(
        cache::prune(state_dir)
            .map_err(patina_core::EngineError::from)
            .context("failed to prune the remote checkout cache")?,
    );
    removed.sort();
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
fn row(name: &str, action: &str, rev: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "name": name, "action": action, "rev": rev })
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

    /// An inventory holding one remote, optionally pinned, over a state
    /// directory that does not exist. Nothing here touches the network or the
    /// filesystem: the renderers read only what they are handed.
    fn inventory(rev: Option<&str>) -> RemoteInventory {
        let pin = rev.map(|rev| patina_core::remote::lockfile::LockEntry {
            url: "https://example.invalid/r".to_owned(),
            git_ref: Some("main".to_owned()),
            rev: rev.to_owned(),
            updated_at: "2026-08-11T14:00:00Z".to_owned(),
        });
        RemoteInventory {
            repo_root: camino::Utf8PathBuf::from("/repo"),
            state_dir: camino::Utf8PathBuf::from("/state/does-not-exist"),
            global_min_age: None,
            lockfile: patina_core::remote::lockfile::Lockfile::default(),
            remotes: vec![RemoteView {
                name: "humanizer".to_owned(),
                spec: patina_core::RemoteSpec {
                    name: "humanizer".to_owned(),
                    url: "https://example.invalid/r".to_owned(),
                    git_ref: Some("main".to_owned()),
                    min_age: None,
                },
                pin,
            }],
        }
    }

    fn proposal(outcome: GateOutcome, current: Option<&str>) -> Proposal {
        Proposal {
            name: "humanizer".to_owned(),
            candidate_rev: "b".repeat(40),
            candidate_epoch: 1_786_456_800,
            current_rev: current.map(str::to_owned),
            outcome,
        }
    }

    #[test]
    fn list_json_reports_the_declared_remote_and_its_pin() {
        let mut reporter = BufferReporter::new();
        let rev = "a".repeat(40);
        assert_eq!(run_list(&inventory(Some(&rev)), true, &mut reporter), 0);
        let doc: serde_json::Value =
            serde_json::from_str(reporter.out.trim()).expect("one JSON document");
        assert_eq!(
            doc.pointer("/remotes/0/name")
                .and_then(serde_json::Value::as_str),
            Some("humanizer")
        );
        assert_eq!(
            doc.pointer("/remotes/0/rev")
                .and_then(serde_json::Value::as_str),
            Some(rev.as_str())
        );
        assert_eq!(
            doc.pointer("/remotes/0/pending")
                .and_then(serde_json::Value::as_bool),
            Some(false),
            "no pending file exists, so nothing is pending"
        );
    }

    #[test]
    fn list_human_marks_an_unpinned_remote() {
        let mut reporter = BufferReporter::new();
        assert_eq!(run_list(&inventory(None), false, &mut reporter), 0);
        assert!(
            reporter.out.contains("humanizer") && reporter.out.contains("(unpinned)"),
            "an unpinned remote must be shown as such: {}",
            reporter.out
        );
    }

    #[test]
    fn list_human_says_so_when_nothing_is_declared() {
        let mut empty = inventory(None);
        empty.remotes.clear();
        let mut reporter = BufferReporter::new();
        assert_eq!(run_list(&empty, false, &mut reporter), 0);
        assert!(reporter.out.contains("No remotes are declared"));
    }

    #[test]
    fn an_already_pinned_proposal_reports_up_to_date_and_writes_nothing() {
        let mut inv = inventory(None);
        let view = only_view(&inv);
        let mut reader = ScriptedReader(None);
        let mut reporter = BufferReporter::new();
        let action = settle(
            &mut inv,
            &view,
            &proposal(GateOutcome::AlreadyPinned, None),
            "2026-08-11T14:00:00Z",
            &flags(false),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter,
        )
        .expect("settling an up-to-date remote cannot fail");
        assert_eq!(action, Action::UpToDate);
        assert!(reporter.out.contains("already at"));
        assert!(
            inv.lockfile.is_empty(),
            "an up-to-date remote must not write a pin"
        );
    }

    #[test]
    fn a_cooldown_proposal_reports_when_it_becomes_eligible() {
        let mut inv = inventory(None);
        let view = only_view(&inv);
        let mut reader = ScriptedReader(None);
        let mut reporter = BufferReporter::new();
        let action = settle(
            &mut inv,
            &view,
            &proposal(
                GateOutcome::Cooldown {
                    eligible_at: 1_786_456_800,
                },
                Some(&"a".repeat(40)),
            ),
            "2026-08-11T14:00:00Z",
            &flags(false),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter,
        )
        .expect("settling a held remote cannot fail");
        assert_eq!(action, Action::Held);
        assert!(
            reporter.out.contains("2026-08-11T14:00:00Z") && reporter.out.contains("min_age"),
            "the cooldown must name the eligibility instant: {}",
            reporter.out
        );
        assert!(
            inv.lockfile.is_empty(),
            "a held remote must not write a pin"
        );
    }

    #[test]
    fn a_future_dated_proposal_is_rejected_and_writes_nothing() {
        let mut inv = inventory(None);
        let view = only_view(&inv);
        let mut reader = ScriptedReader(None);
        let mut reporter = BufferReporter::new();
        let action = settle(
            &mut inv,
            &view,
            &proposal(
                GateOutcome::RejectedFuture {
                    candidate_epoch: 1,
                    now_epoch: 0,
                },
                None,
            ),
            "2026-08-11T14:00:00Z",
            &flags(false),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter,
        )
        .expect("settling a rejected remote cannot fail");
        assert_eq!(action, Action::Rejected);
        assert!(
            reporter.err.contains("ahead of this machine's clock"),
            "the reject must say why: {}",
            reporter.err
        );
        assert!(inv.lockfile.is_empty());
    }

    #[test]
    fn a_flagged_proposal_declined_in_a_non_tty_leaves_the_pin_alone() {
        let mut inv = inventory(None);
        let view = only_view(&inv);
        let mut reader = ScriptedReader(None);
        let mut reporter = BufferReporter::new();
        let action = settle(
            &mut inv,
            &view,
            &proposal(
                GateOutcome::NeedsConfirmation(concerns()),
                Some(&"a".repeat(40)),
            ),
            "2026-08-11T14:00:00Z",
            &flags(false),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter,
        )
        .expect("settling a declined remote cannot fail");
        assert_eq!(action, Action::Held);
        assert!(reporter.out.contains("pin unchanged at"));
        assert!(inv.lockfile.is_empty());
    }

    fn flags(json: bool) -> UpdateFlags {
        UpdateFlags {
            name: None,
            bypass_age: false,
            yes: false,
            json,
        }
    }

    fn only_view(inventory: &RemoteInventory) -> RemoteView {
        inventory
            .find("humanizer")
            .expect("the fixture declares one remote")
            .clone()
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
}
