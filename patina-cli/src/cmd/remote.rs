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
//!
//! A shell hook must never contend with a running apply, and the shared lock is
//! therefore held across the inventory read alone, never across the network. A
//! `git` call has no timeout of its own, so holding the lock over one would let
//! an unreachable server keep an apply waiting until its own lock wait expires.
//!
//! See `docs/REMOTE_SOURCES.md` "Commands", "The update gate", and
//! "Shell integration".

use crate::cli::RemoteArgs;
use crate::cli::RemoteCommand;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::cmd::shared_lock;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use crate::output::style::Styles;
use crate::output::style::paint;
use anyhow::Context;
use anyhow::Result;
use patina_core::LockKind;
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
use std::io::Write;
use tabwriter::TabWriter;

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
            let inventory = read_inventory(&lock_path, false, reporter)?;
            Ok(run_list(
                &inventory,
                args.json,
                &Styles::colored(),
                reporter,
            ))
        }
        RemoteCommand::Check { hook } => {
            let inventory = read_inventory(&lock_path, *hook, reporter)?;
            Ok(run_check(&inventory, *hook, args.json, reporter))
        }
        RemoteCommand::Update { name, now, yes } => run_update_locked(
            &lock_path,
            &UpdateFlags {
                name: name.clone(),
                bypass_age: *now,
                yes: *yes,
                json: args.json,
            },
            tty,
            reader,
            reporter,
        ),
        RemoteCommand::Prune => {
            let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
                .context("failed to acquire the exclusive lock for `patina remote prune`")?;
            let inventory = declared_remotes()?;
            run_prune(&inventory, args.json, reporter)
        }
    }
}

/// Run `patina remote update` over every declared remote, with the default
/// flags and the exclusive lock. The producer pass `patina apply --update`
/// drives, so the two spell one operation rather than the apply synthesizing a
/// command line.
///
/// # Errors
///
/// Returns an error under the same conditions as [`run`].
pub fn run_update_all(
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let state_dir =
        patina_core::resolve_state_dir().context("failed to resolve the state directory")?;
    run_update_locked(
        &state_dir.join("lock"),
        &UpdateFlags {
            name: None,
            bypass_age: false,
            yes: false,
            json: false,
        },
        tty,
        reader,
        reporter,
    )
}

/// Acquire the exclusive lock, read the inventory under it, and run the update
/// pass.
fn run_update_locked(
    lock_path: &camino::Utf8Path,
    flags: &UpdateFlags,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let _guard = acquire_lock(lock_path, LockKind::Exclusive, exclusive_timeout())
        .context("failed to acquire the exclusive lock for `patina remote update`")?;
    // Read the lockfile only once the exclusive lock is held: this is a
    // read-modify-write of a file another `patina remote update` may be
    // rewriting, so a read taken before the lock could silently drop a
    // concurrent bump.
    let mut inventory = declared_remotes()?;
    run_update(&mut inventory, flags, tty, reader, reporter)
}

/// Enumerate the root manifest's declared remotes and their pins.
fn declared_remotes() -> Result<RemoteInventory> {
    update::inventory().context("failed to enumerate the declared remotes")
}

/// Read the inventory under the shared lock, releasing it before returning.
///
/// The lock covers the read of the manifest and the lockfile, the files a
/// concurrent `apply` or `remote update` rewrites, and nothing after it. The
/// caller may then spend as long as it likes on the network without an apply
/// waiting behind it.
fn read_inventory(
    lock_path: &camino::Utf8Path,
    quiet: bool,
    reporter: &mut impl Reporter,
) -> Result<RemoteInventory> {
    let guard = shared_lock(lock_path, quiet, reporter);
    let inventory = declared_remotes();
    drop(guard);
    inventory
}

/// Report the pins as recorded, with the pending state the last
/// `check` observed.
fn run_list(
    inventory: &RemoteInventory,
    json: bool,
    styles: &Styles,
    reporter: &mut impl Reporter,
) -> i32 {
    let pending = notice::read_pending(&inventory.state_dir);
    if json {
        let rows: Vec<serde_json::Value> = inventory
            .remotes
            .iter()
            .map(|view| {
                serde_json::json!({
                    "name": view.name().as_str(),
                    "url": view.spec.url,
                    "ref": view.spec.git_ref,
                    "rev": view.pin.as_ref().map(|pin| pin.rev.clone()),
                    "updated_at": view.pin.as_ref().map(|pin| pin.updated_at.clone()),
                    "pending": notice::is_pending(&pending, view.name()),
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

    let mut table = format!(
        "{}\t{}\t{}\t{}\n",
        paint(styles.header, "NAME"),
        paint(styles.header, "REF"),
        paint(styles.header, "REV"),
        paint(styles.header, "URL"),
    );
    for view in &inventory.remotes {
        table.push_str(&list_row(
            view,
            notice::is_pending(&pending, view.name()),
            styles,
        ));
    }
    for line in align(&table).lines() {
        reporter.line(line);
    }
    ExitCode::Success.code()
}

/// One tab-separated listing row. The URL is the trailing cell, which elastic
/// tabstops leave unpadded, so a pending tag can follow it without widening a
/// column for every other remote.
fn list_row(view: &RemoteView, pending: bool, styles: &Styles) -> String {
    let git_ref = match &view.spec.git_ref {
        Some(declared) => declared.clone(),
        None => paint(styles.remote.implicit_ref, "(default branch)"),
    };
    let rev = match &view.pin {
        Some(pin) => paint(styles.remote.rev, &pin.rev),
        None => paint(styles.remote.attention, "(unpinned)"),
    };
    let tag = if pending {
        format!("  {}", paint(styles.remote.attention, "(update pending)"))
    } else {
        String::new()
    };
    format!(
        "{}\t{git_ref}\t{rev}\t{}{tag}\n",
        paint(styles.remote.name, view.name().as_str()),
        view.spec.url,
    )
}

/// Align tab-separated cells into columns.
///
/// ANSI mode measures a cell by printable width, so a painted cell pads exactly
/// as its stripped form does; that is what keeps piped, `--color never`, and
/// `NO_COLOR` output aligned identically to a terminal's. Writing to a `Vec`
/// cannot fail, so the unaligned fallback is unreachable and exists only
/// because a print path must not carry a panic.
fn align(table: &str) -> String {
    let mut aligned: Vec<u8> = Vec::new();
    let mut writer = TabWriter::new(&mut aligned)
        .minwidth(0)
        .padding(2)
        .ansi(true);
    if writer.write_all(table.as_bytes()).is_err() || writer.flush().is_err() {
        return table.to_owned();
    }
    drop(writer);
    String::from_utf8(aligned).unwrap_or_else(|_| table.to_owned())
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
            Err(error) => failures.push(format!("{}: {error}", view.name())),
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
        notice::publish(&inventory.state_dir, &behind, message.as_deref()).err(),
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

    let selected: Vec<usize> = if let Some(name) = &flags.name {
        let Some(index) = inventory
            .remotes
            .iter()
            .position(|view| view.name().matches(name))
        else {
            reporter.warn(&format!(
                "no remote named `{name}` is declared in this repository's root patina.toml"
            ));
            return Ok(ExitCode::Generic.code());
        };
        vec![index]
    } else {
        (0..inventory.remotes.len()).collect()
    };

    if selected.is_empty() {
        reporter.line("No remotes are declared.");
        return Ok(ExitCode::Success.code());
    }

    let now_epoch = patina_core::current_epoch_seconds();
    let now_rfc3339 = patina_core::current_rfc3339();
    let mut outcomes: Vec<Outcome> = Vec::new();

    for index in selected {
        let Some(view) = inventory.remotes.get(index) else {
            continue;
        };
        let proposal = match update::propose(inventory, view, now_epoch, flags.bypass_age) {
            Ok(proposal) => proposal,
            Err(error) => {
                // One unreachable remote must not stop the others.
                reporter.warn(&format!("could not update remote {}: {error}", view.name()));
                outcomes.push(Outcome {
                    name: view.name().clone(),
                    action: Action::Failed,
                    rev: None,
                });
                continue;
            }
        };
        let action = settle(
            &mut inventory.lockfile,
            &inventory.repo_root,
            view,
            &proposal,
            &now_rfc3339,
            flags,
            tty,
            reader,
            reporter,
        )?;
        outcomes.push(Outcome {
            name: view.name().clone(),
            action,
            rev: Some(proposal.candidate_rev),
        });
    }

    reconcile_notice(&inventory.state_dir, &outcomes, reporter);

    if flags.json {
        let rows: Vec<serde_json::Value> = outcomes.iter().map(Outcome::row).collect();
        reporter.json(&document(&serde_json::json!({ "remotes": rows })));
    }
    Ok(exit_for(&outcomes).code())
}

/// What one remote's update pass amounted to, for the JSON envelope, the exit
/// code, and the notice reconciliation.
struct Outcome {
    name: patina_core::RemoteName,
    action: Action,
    rev: Option<String>,
}

impl Outcome {
    /// One `remotes` row of the `remote update` JSON envelope.
    fn row(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name.as_str(),
            "action": self.action.label(),
            "rev": self.rev,
        })
    }
}

/// Settle the notice state for every remote this run bumped or found already
/// at its tip, so the shell stops announcing an update the user just acted on.
///
/// A failure to rewrite notification state must not fail a command whose real
/// work (the lockfile bump) is already durable, so it is reported and
/// swallowed.
fn reconcile_notice(
    state_dir: &camino::Utf8Path,
    outcomes: &[Outcome],
    reporter: &mut impl Reporter,
) {
    let names: Vec<&patina_core::RemoteName> = outcomes
        .iter()
        .filter(|outcome| matches!(outcome.action, Action::Updated | Action::UpToDate))
        .map(|outcome| &outcome.name)
        .collect();
    if names.is_empty() {
        return;
    }
    if let Err(error) = notice::settle(state_dir, &names) {
        reporter.warn(&format!(
            "failed to update the remote notice state: {error}"
        ));
    }
}

/// The exit code for a whole `remote update` run.
///
/// A failure outranks a decline: a run that both failed to reach one remote and
/// was told no about another reports the failure, the more actionable of the
/// two. A pin the gate held on its own is a success: the run did what the gate
/// said, and a script that treated a cooldown as an error would fail daily.
fn exit_for(outcomes: &[Outcome]) -> ExitCode {
    if outcomes
        .iter()
        .any(|outcome| matches!(outcome.action, Action::Failed | Action::Rejected))
    {
        ExitCode::Generic
    } else if outcomes
        .iter()
        .any(|outcome| outcome.action == Action::Declined)
    {
        ExitCode::UserDeclined
    } else {
        ExitCode::Success
    }
}

/// What happened to one remote's proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Already at the upstream tip.
    UpToDate,
    /// The pin moved.
    Updated,
    /// The gate held it back on its own: a cooldown, or a verdict this binary
    /// does not recognize. Nobody was asked, so nothing was refused.
    Held,
    /// The user was asked and said no. Distinct from [`Action::Held`] because
    /// a declined prompt is what exit code 5 means across every command.
    Declined,
    /// A hard reject.
    Rejected,
    /// The remote could not even be proposed (unreachable, missing ref).
    Failed,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::Updated => "updated",
            Self::Held => "held",
            Self::Declined => "declined",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
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
    lockfile: &mut patina_core::remote::lockfile::Lockfile,
    repo_root: &camino::Utf8Path,
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
            bump(
                lockfile,
                repo_root,
                view,
                proposal,
                now_rfc3339,
                flags.json,
                reporter,
            )?;
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
                    patina_core::clock::epoch_to_rfc3339(*eligible_at)
                ));
            }
            Ok(Action::Held)
        }
        GateOutcome::NeedsConfirmation(concerns) => {
            let answer = confirm(&proposal.name, concerns, flags.yes, tty, reader, reporter);
            if answer == Confirmed::Yes {
                bump(
                    lockfile,
                    repo_root,
                    view,
                    proposal,
                    now_rfc3339,
                    flags.json,
                    reporter,
                )?;
                return Ok(Action::Updated);
            }
            if !flags.json {
                reporter.line(&format!(
                    "{}: pin unchanged at {}",
                    proposal.name,
                    proposal.current_rev.as_deref().unwrap_or("(unpinned)")
                ));
            }
            Ok(if answer == Confirmed::No {
                Action::Declined
            } else {
                Action::Held
            })
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
    lockfile: &mut patina_core::remote::lockfile::Lockfile,
    repo_root: &camino::Utf8Path,
    view: &RemoteView,
    proposal: &Proposal,
    now_rfc3339: &str,
    json: bool,
    reporter: &mut impl Reporter,
) -> Result<()> {
    update::accept(lockfile, repo_root, view, proposal, now_rfc3339)
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
) -> Confirmed {
    for concern in concerns {
        reporter.warn(&format!("{name}: {}", concern.describe()));
    }
    if yes {
        return Confirmed::Yes;
    }
    if tty == Tty::NonInteractive {
        reporter.warn(&format!(
            "{name}: cannot confirm in a non-interactive shell; re-run with --yes to accept"
        ));
        return Confirmed::Unasked;
    }
    reporter.confirm(&format!("Bump the pin for {name} anyway?"));
    if matches!(reader.read_line().unwrap_or_default().trim(), "y" | "Y") {
        Confirmed::Yes
    } else {
        Confirmed::No
    }
}

/// What the confirmation step concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirmed {
    /// Accepted, by `--yes` or at the prompt.
    Yes,
    /// The user was asked and said no.
    No,
    /// There was no way to ask. Distinct from [`Confirmed::No`] because a
    /// non-interactive run that could not raise the prompt refused nothing, and
    /// exit code 5 means a prompt was declined.
    Unasked,
}

/// Run the cache sweep by hand: whole trees for remotes the root manifest no
/// longer declares, then unreferenced checkouts of the ones it does. The
/// currently pinned checkouts are kept regardless of journal reachability: a
/// pin bumped but not yet applied is the warm cache an offline apply depends
/// on.
fn run_prune(inventory: &RemoteInventory, json: bool, reporter: &mut impl Reporter) -> Result<i32> {
    let declared = inventory
        .remotes
        .iter()
        .map(patina_core::remote::update::RemoteView::name)
        .collect();
    let keep: Vec<(patina_core::RemoteName, String)> = inventory
        .remotes
        .iter()
        .filter_map(|view| {
            view.pin
                .as_ref()
                .map(|pin| (view.name().clone(), pin.rev.clone()))
        })
        .collect();
    let removed = cache::prune(&inventory.state_dir, &declared, Some(&keep))
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

/// Serialize a JSON envelope, falling back to an empty object so a
/// serialization failure cannot abort a command whose real work is done.
fn document(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;

    /// A validated remote name for the in-process fixtures.
    fn remote_name(spelling: &str) -> patina_core::RemoteName {
        patina_core::RemoteName::parse(spelling).expect("a legal remote name")
    }

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
                spec: patina_core::RemoteSpec {
                    name: remote_name("humanizer"),
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
            current_rev: current.map(str::to_owned),
            outcome,
        }
    }

    #[test]
    fn list_json_reports_the_declared_remote_and_its_pin() {
        let mut reporter = BufferReporter::new();
        let rev = "a".repeat(40);
        assert_eq!(
            run_list(
                &inventory(Some(&rev)),
                true,
                &Styles::plain(),
                &mut reporter
            ),
            0
        );
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
        assert_eq!(
            run_list(&inventory(None), false, &Styles::plain(), &mut reporter),
            0
        );
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
        assert_eq!(run_list(&empty, false, &Styles::plain(), &mut reporter), 0);
        assert!(reporter.out.contains("No remotes are declared"));
        assert!(
            !reporter.out.contains("NAME"),
            "an empty listing must print no table header: {}",
            reporter.out
        );
    }

    /// The second remote is wider in the name column and narrower in the rev
    /// column, so no single row drives every width.
    #[test]
    fn list_human_starts_each_column_where_its_header_starts() {
        let rev = "a".repeat(40);
        let mut inv = inventory(Some(&rev));
        inv.remotes.push(RemoteView {
            spec: patina_core::RemoteSpec {
                name: remote_name("a-much-longer-remote-name"),
                url: "https://example.invalid/other".to_owned(),
                git_ref: None,
                min_age: None,
            },
            pin: None,
        });

        let mut reporter = BufferReporter::new();
        assert_eq!(run_list(&inv, false, &Styles::plain(), &mut reporter), 0);
        let mut lines = reporter.out.lines();
        let header = lines.next().expect("a header row");
        let pinned = lines.next().expect("the pinned remote's row");
        let unpinned = lines.next().expect("the unpinned remote's row");
        assert_eq!(lines.next(), None, "one row per remote and nothing more");

        for (label, pinned_cell, unpinned_cell) in [
            ("REF", "main", "(default branch)"),
            ("REV", rev.as_str(), "(unpinned)"),
            (
                "URL",
                "https://example.invalid/r",
                "https://example.invalid/other",
            ),
        ] {
            let column = header.find(label).expect("every header label is printed");
            let starts_at = |line: &str, expected: &str| {
                line.get(column..)
                    .is_some_and(|rest| rest.starts_with(expected))
            };
            assert!(
                starts_at(pinned, pinned_cell),
                "{label} must start at column {column} in {pinned:?}"
            );
            assert!(
                starts_at(unpinned, unpinned_cell),
                "{label} must start at column {column} in {unpinned:?}"
            );
        }
    }

    /// Color must be purely additive over the plain table: the colored render
    /// has to carry escapes, and stripping them has to give back the plain
    /// render byte for byte. Padding painted along with its cell would break
    /// this, and with it the alignment of piped and `--color never` output.
    #[test]
    fn list_human_color_strips_back_to_the_plain_table() {
        let inv = inventory(Some(&"a".repeat(40)));

        let mut plain = BufferReporter::new();
        assert_eq!(run_list(&inv, false, &Styles::plain(), &mut plain), 0);
        let mut colored = BufferReporter::new();
        assert_eq!(run_list(&inv, false, &Styles::colored(), &mut colored), 0);

        assert!(
            colored.out.contains('\u{1b}'),
            "the colored render must carry escapes: {:?}",
            colored.out
        );
        assert_eq!(
            anstream::adapter::strip_str(&colored.out).to_string(),
            plain.out,
            "stripping color must leave the plain table untouched"
        );
    }

    /// A remote the last `check` found behind its upstream must say so on its
    /// own row, in text and not by color alone.
    #[test]
    fn list_row_tags_a_pending_update_after_the_url() {
        let view = only_view(&inventory(Some(&"a".repeat(40))));
        let plain = Styles::plain();
        assert!(
            list_row(&view, true, &plain)
                .ends_with("\thttps://example.invalid/r  (update pending)\n"),
            "the tag must follow the URL in the trailing cell: {:?}",
            list_row(&view, true, &plain)
        );
        assert!(
            list_row(&view, false, &plain).ends_with("\thttps://example.invalid/r\n"),
            "a remote at its pin must carry no tag"
        );
    }

    #[test]
    fn an_already_pinned_proposal_reports_up_to_date_and_writes_nothing() {
        let mut inv = inventory(None);
        let view = only_view(&inv);
        let mut reader = ScriptedReader(None);
        let mut reporter = BufferReporter::new();
        let action = settle(
            &mut inv.lockfile,
            &inv.repo_root.clone(),
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
            &mut inv.lockfile,
            &inv.repo_root.clone(),
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
            &mut inv.lockfile,
            &inv.repo_root.clone(),
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
            &mut inv.lockfile,
            &inv.repo_root.clone(),
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
        assert_eq!(
            action,
            Action::Held,
            "a shell that could not raise the prompt refused nothing"
        );
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
    fn a_declined_prompt_exits_five_and_a_gate_hold_exits_zero() {
        // Exit 5 is the repository-wide code for a declined prompt, so a
        // decline must not be folded into the gate's own holds, which are the
        // ordinary daily outcome of a cooldown.
        let outcome = |action| {
            vec![Outcome {
                name: remote_name("humanizer"),
                action,
                rev: None,
            }]
        };
        assert_eq!(exit_for(&outcome(Action::Declined)), ExitCode::UserDeclined);
        assert_eq!(exit_for(&outcome(Action::Held)), ExitCode::Success);
        assert_eq!(exit_for(&outcome(Action::UpToDate)), ExitCode::Success);
        assert_eq!(exit_for(&outcome(Action::Failed)), ExitCode::Generic);
        assert_eq!(exit_for(&outcome(Action::Rejected)), ExitCode::Generic);

        let mut mixed = outcome(Action::Declined);
        mixed.extend(outcome(Action::Failed));
        assert_eq!(
            exit_for(&mixed),
            ExitCode::Generic,
            "an unreachable remote is the more actionable of the two"
        );
    }

    #[test]
    fn yes_accepts_without_reading_stdin() {
        let mut reader = ScriptedReader(None);
        let mut reporter = BufferReporter::new();
        assert_eq!(
            confirm(
                "humanizer",
                &concerns(),
                true,
                Tty::Interactive,
                &mut reader,
                &mut reporter
            ),
            Confirmed::Yes
        );
        assert!(
            reporter.err.contains("history was rewritten"),
            "the concern must still be reported under --yes: {}",
            reporter.err
        );
    }

    #[test]
    fn a_non_interactive_shell_holds_and_says_how_to_proceed() {
        let mut reader = ScriptedReader(Some("y\n".to_owned()));
        let mut reporter = BufferReporter::new();
        assert_eq!(
            confirm(
                "humanizer",
                &concerns(),
                false,
                Tty::NonInteractive,
                &mut reader,
                &mut reporter
            ),
            Confirmed::Unasked,
            "a flagged bump must not be accepted without a confirmation, and a shell that \
             could not be asked has refused nothing"
        );
        assert!(
            reporter.err.contains("--yes"),
            "the message must name the flag that would accept it: {}",
            reporter.err
        );
    }

    #[test]
    fn an_interactive_y_accepts_and_anything_else_declines() {
        for (answer, expected) in [
            ("y\n", Confirmed::Yes),
            ("Y\n", Confirmed::Yes),
            ("n\n", Confirmed::No),
            ("", Confirmed::No),
        ] {
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
                "answer {answer:?} must map to {expected:?}"
            );
            assert!(
                reporter.err.contains("Bump the pin for humanizer anyway?"),
                "the interactive path must prompt: {}",
                reporter.err
            );
        }
    }

    #[test]
    fn a_failed_shared_lock_warns_normally_and_stays_silent_for_a_hook() {
        // A lock path whose parent directory does not exist fails acquisition,
        // exercising the same fallthrough a timeout takes. The hook contract is
        // zero stderr, so the quiet flag must gate the warning.
        let lock_path = camino::Utf8PathBuf::from("/does/not/exist/anywhere/lock");

        let mut reporter = BufferReporter::new();
        assert!(shared_lock(&lock_path, false, &mut reporter).is_none());
        assert!(
            reporter.err.contains("proceeding without the shared lock"),
            "the non-hook path must warn: {}",
            reporter.err
        );

        let mut reporter = BufferReporter::new();
        assert!(shared_lock(&lock_path, true, &mut reporter).is_none());
        assert!(
            reporter.err.is_empty(),
            "the hook path must stay silent, got: {}",
            reporter.err
        );
    }
}
