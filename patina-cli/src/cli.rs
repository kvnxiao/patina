//! The clap-derived command-line surface for `patina` (REQ-017).
//!
//! `apply`, `status`, and `rollback` land so far; the `debug` family is
//! wired by its own task. The derive surface is kept thin — parsing only —
//! so the command logic lives in [`crate::cmd`] and stays unit-testable
//! without going through clap.

use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use camino::Utf8PathBuf;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;

/// Resolve a command's outcome to a process exit code (REQ-022).
///
/// This is the single funnel every subcommand terminates through, so the
/// exit-code contract lives in one place. A subcommand returns
/// `Ok(code)` when it reached a terminal state under its own control (a
/// successful apply, an aborted-by-hook apply, a declined prompt); the
/// code is returned verbatim. An `Err` is an engine-level failure: its
/// error chain is rendered to the reporter's err stream and mapped to an
/// [`ExitCode`] via [`ExitCode::from_error_chain`] — the lock timeout
/// becomes `4`, every other failure `1`.
///
/// The returned `i32` is what [`crate::main`] hands to
/// [`std::process::exit`].
#[must_use = "the returned code is the process's terminal exit status"]
pub fn resolve_exit_code(outcome: anyhow::Result<i32>, reporter: &mut impl Reporter) -> i32 {
    match outcome {
        Ok(code) => code,
        Err(error) => {
            // Render the full context chain so the underlying cause (e.g.
            // the offending TOML line) reaches the user, not just the
            // outermost `anyhow` context line.
            for cause in error.chain() {
                reporter.warn(&cause.to_string());
            }
            ExitCode::from_error_chain(&error).code()
        }
    }
}

/// `patina` — a cross-platform dotfile manager.
#[derive(Debug, Parser)]
#[command(name = "patina", version, about)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scaffold a root `patina.toml` and persist the default-repo pointer.
    Init(InitArgs),

    /// Bring an existing dotfile under management: move it into a module and
    /// write a `[[file]]` entry.
    Add(AddArgs),

    /// Unmanage a target: remove its `[[file]]` entry and replace the target
    /// with a regular file holding the last-applied content. `--purge`
    /// deletes the target outright.
    Remove(RemoveArgs),

    /// Promote a drifted copy-mode target: copy its current bytes back into
    /// its repository source, then re-apply so the journal records the new
    /// content. Refuses on template-rendered and symbolic-link targets.
    Promote(PromoteArgs),

    /// Materialize the declared configuration at its targets.
    Apply(ApplyArgs),

    /// Report drift between the repository and the materialized targets.
    Status(StatusArgs),

    /// Reverse the most recent successful apply via the journal and backups.
    Rollback(RollbackArgs),

    /// Inspect the environment for known problems (UNC repository paths,
    /// missing Windows Developer Mode, OS-too-old, missing default repo).
    /// Read-only by default; `--fix` interactively remediates fixable
    /// findings.
    Doctor(DoctorArgs),

    /// Watch the repository and re-apply on source changes. `--foreground`
    /// runs the watcher inline in the current terminal (REQ-004); the
    /// `install` / `uninstall` / `start` / `stop` / `restart` / `status`
    /// subcommands manage the per-OS background service (REQ-001 / REQ-003).
    Watch(WatchArgs),

    /// Debugging utilities. Hidden from the top-level help summary but
    /// documented; `journal` decodes a binary plan file post-mortem.
    #[command(hide = true, subcommand)]
    Debug(DebugCommand),
}

/// Flags for `patina watch` (REQ-001 / REQ-003 / REQ-004 / REQ-006).
///
/// `--foreground` runs the watcher loop inline, attached to the invoking
/// shell, and exits cleanly on Ctrl-C / SIGTERM (REQ-004). The lifecycle
/// subcommands (`install` / `uninstall` / `start` / `stop` / `restart` /
/// `status`) manage the per-OS background service (REQ-001 / REQ-003). With
/// neither, the command reports that a mode must be chosen.
#[derive(Debug, Args, Default)]
pub struct WatchArgs {
    /// The background-service lifecycle subcommand. Mutually exclusive with
    /// `--foreground`; omit both to see the usage hint.
    #[command(subcommand)]
    pub command: Option<WatchCommand>,

    /// Run the watcher inline in the current terminal instead of installing a
    /// background service. Ctrl-C (SIGINT) or SIGTERM shuts it down cleanly.
    #[arg(long)]
    pub foreground: bool,

    /// Emit a JSON envelope instead of human output. Global, so it is accepted
    /// both before and after a lifecycle subcommand (`patina watch status
    /// --json`).
    #[arg(long, global = true)]
    pub json: bool,
}

/// Background-service lifecycle subcommands under `patina watch` (REQ-001 /
/// REQ-003). Each operates on the per-OS service registration through the
/// `patina_core::watch::service` backend; all but `status` acquire the
/// exclusive advisory lock, `status` the shared lock (SPEC-0001 REQ-023).
#[derive(Debug, Subcommand, Clone)]
pub enum WatchCommand {
    /// Register the watcher as a per-user background service that launches at
    /// login. Exits 1 if already installed.
    Install,

    /// Stop the running watcher and remove the service registration.
    Uninstall {
        /// Proceed without prompting. Mutating: acquires the exclusive lock.
        #[arg(long)]
        yes: bool,
    },

    /// Ask the platform supervisor to start the installed service.
    Start,

    /// Ask the platform supervisor to stop the running service without
    /// removing its registration.
    Stop,

    /// Stop then start the installed service.
    Restart,

    /// Report the service's installed / running state, last-exit code, and the
    /// watcher's recovered subscription / re-apply counters. Read-only.
    Status,
}

/// Subcommands under the `patina debug` namespace.
#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// Decode a `<ts>.plan` journal file into a human-readable view.
    Journal(DebugJournalArgs),

    /// Decode a `drift.cache` file into a human-readable view.
    DriftCache(DebugDriftCacheArgs),
}

/// Flags for `patina debug journal`.
#[derive(Debug, Args)]
pub struct DebugJournalArgs {
    /// Path to the `<ts>.plan` file to decode.
    #[arg(value_name = "path")]
    pub path: Utf8PathBuf,
}

/// Flags for `patina debug drift-cache`.
#[derive(Debug, Args)]
pub struct DebugDriftCacheArgs {
    /// Path to the `drift.cache` file to decode.
    #[arg(value_name = "path")]
    pub path: Utf8PathBuf,
}

/// Flags for `patina init`.
#[derive(Debug, Args, Default)]
pub struct InitArgs {
    /// Target directory to initialize. Defaults to the current working
    /// directory when omitted. Created if it does not yet exist.
    #[arg(value_name = "path")]
    pub path: Option<Utf8PathBuf>,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,

    /// Proceed without prompting. `init` is a mutating command (REQ-009);
    /// this is accepted for parity with the other mutating subcommands.
    #[arg(long)]
    pub yes: bool,
}

/// Flags for `patina add` (REQ-002).
///
/// The three mode flags (`--symlink` / `--copy` / `--template`) form a
/// mutually-exclusive clap group: declaring two produces a usage error
/// (exit 2). In v1.0 `add` exposes only these three modes — not
/// `symlink-dir` / `copy-tree` (a v1.0 non-goal).
#[derive(Debug, Args, Default)]
#[command(group = clap::ArgGroup::new("mode").multiple(false))]
#[expect(
    clippy::struct_excessive_bools,
    reason = "this is a clap-derived flag struct: each bool is an independent CLI flag (the three mode flags plus --json / --yes), not a state machine that would be better modelled as an enum. The mode flags are unified at use-site into the AddMode enum."
)]
pub struct AddArgs {
    /// The dotfile to bring under management. Absolute or HOME-relative
    /// (a leading `~` is expanded).
    #[arg(value_name = "path")]
    pub path: Utf8PathBuf,

    /// The module subdirectory to file the entry under. Prompted for in a
    /// TTY when omitted; required in a non-TTY shell.
    #[arg(long, value_name = "name")]
    pub module: Option<String>,

    /// File the entry as a symbolic link.
    #[arg(long, group = "mode")]
    pub symlink: bool,

    /// File the entry as a byte copy.
    #[arg(long, group = "mode")]
    pub copy: bool,

    /// File the entry as a rendered template.
    #[arg(long, group = "mode")]
    pub template: bool,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,

    /// Proceed without prompting. `add` is a mutating command (REQ-009).
    #[arg(long)]
    pub yes: bool,
}

/// Flags for `patina remove` (REQ-003).
#[derive(Debug, Args, Default)]
pub struct RemoveArgs {
    /// The managed target to unmanage. Absolute or HOME-relative (a leading
    /// `~` is expanded).
    #[arg(value_name = "path")]
    pub path: Utf8PathBuf,

    /// Delete the target from disk entirely instead of replacing it with a
    /// regular file holding the last-applied content.
    #[arg(long)]
    pub purge: bool,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,

    /// Proceed without prompting. `remove` is a mutating command (REQ-009).
    #[arg(long)]
    pub yes: bool,
}

/// Flags for `patina promote` (REQ-004).
#[derive(Debug, Args, Default)]
pub struct PromoteArgs {
    /// The drifted copy-mode target to promote. Absolute or HOME-relative (a
    /// leading `~` is expanded).
    #[arg(value_name = "target")]
    pub target: Utf8PathBuf,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,

    /// Proceed without prompting. `promote` is a mutating command (REQ-009).
    #[arg(long)]
    pub yes: bool,
}

/// Flags for `patina rollback`.
#[derive(Debug, Args, Default)]
pub struct RollbackArgs {
    /// Roll back unconditionally with no prompt, regardless of TTY state.
    #[arg(long)]
    pub yes: bool,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,
}

/// Flags for `patina doctor` (REQ-005, REQ-006).
///
/// The read-only path (no `--fix`) acquires only the shared lock and emits
/// findings; `--fix` (wired in T-011) acquires the exclusive lock and
/// interactively remediates fixable findings, with `--yes` auto-accepting
/// every prompt.
#[derive(Debug, Args, Default)]
pub struct DoctorArgs {
    /// Interactively remediate fixable findings instead of only reporting
    /// them. Mutating: acquires the exclusive lock (T-011).
    #[arg(long)]
    pub fix: bool,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,

    /// With `--fix`, accept every remediation prompt automatically. Required
    /// to run `--fix` in a non-TTY shell (T-011).
    #[arg(long)]
    pub yes: bool,
}

/// Flags for `patina status`.
#[derive(Debug, Args, Default)]
pub struct StatusArgs {
    /// Emit a JSON envelope instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
}

/// Flags for `patina apply`.
#[derive(Debug, Args, Default)]
pub struct ApplyArgs {
    /// Apply unconditionally with no prompt, regardless of TTY state.
    #[arg(long)]
    pub yes: bool,

    /// Override every hook in this invocation to `must_succeed = false`.
    #[arg(long)]
    pub force_deploy: bool,

    /// Emit a JSON envelope instead of human output. Without `--yes` this
    /// is a preview (no mutation); pair with `--yes` to apply.
    #[arg(long)]
    pub json: bool,

    /// Pipe the rendered diff through an external pager if it resolves on
    /// PATH; fall back to the embedded renderer with a warning otherwise.
    #[arg(long, value_enum)]
    pub pager: Option<Pager>,

    /// CLI variable override, repeatable: `-v key=value`.
    #[arg(short = 'v', value_name = "key=value")]
    pub var: Vec<String>,
}

/// External pager tools `--pager` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Pager {
    /// `delta` — a syntax-highlighting diff pager.
    Delta,
    /// `difft` — difftastic, a structural diff tool.
    Difft,
}

impl Pager {
    /// The binary name this pager resolves to on PATH.
    #[must_use = "the binary name drives PATH resolution"]
    pub fn binary(self) -> &'static str {
        match self {
            Pager::Delta => "delta",
            Pager::Difft => "difft",
        }
    }
}
