//! The clap-derived command-line surface for `patina`.
//!
//! This module only parses. The command logic lives in [`crate::cmd`], where
//! the unit tests call it directly without clap.

use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use camino::Utf8PathBuf;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;

/// Resolve a command's outcome to a process exit code.
///
/// Every subcommand terminates here, so the exit-code contract has one site. A
/// subcommand that reaches a terminal state under its own control (a successful
/// apply, an apply a hook aborted, a declined prompt) returns `Ok(code)`, and
/// that code becomes the process status verbatim.
///
/// An `Err` is an engine-level failure. Each cause in the chain is printed to
/// the reporter's err stream, then [`ExitCode::from_error_chain`] picks the
/// code: a lock timeout is `4`, every other failure `1`.
///
/// [`crate::main`] passes the returned `i32` to [`std::process::exit`].
#[must_use = "the returned exit code is the process's terminal status"]
pub fn resolve_exit_code(outcome: anyhow::Result<i32>, reporter: &mut impl Reporter) -> i32 {
    match outcome {
        Ok(code) => code,
        Err(error) => {
            // The outermost error is only the context wrapper. The root cause,
            // the offending TOML line, is at the end of the chain, so every
            // cause is printed.
            for cause in error.chain() {
                reporter.error(&cause.to_string());
            }
            ExitCode::from_error_chain(&error).code()
        }
    }
}

/// `patina`, a cross-platform dotfile manager.
#[derive(Debug, Parser)]
#[command(name = "patina", version, about, disable_help_subcommand = true)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,

    /// When to colorize output. Global: accepted before or after the
    /// subcommand.
    #[arg(long, value_enum, default_value = "auto", global = true)]
    pub color: ColorChoiceArg,
}

/// The `--color` flag: when to emit ANSI styling.
///
/// [`ColorChoiceArg::choice`] resolves it to an [`anstream::ColorChoice`]. The
/// reporter's auto-stream reads that policy to decide whether it styles the
/// destination stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoiceArg {
    /// Color when the stream is a terminal; strip to plain text when piped,
    /// redirected, or `NO_COLOR` is set. The default.
    Auto,
    /// Always emit color, even when the stream is not a terminal.
    Always,
    /// Never emit color.
    Never,
}

impl ColorChoiceArg {
    /// Map to the `anstream` policy the reporter's auto-stream consumes.
    /// `Auto` defers the per-stream terminal / `NO_COLOR` decision to
    /// `anstream`; `Always` / `Never` are unconditional.
    #[must_use = "the returned policy drives whether output is styled"]
    pub fn choice(self) -> anstream::ColorChoice {
        match self {
            ColorChoiceArg::Auto => anstream::ColorChoice::Auto,
            ColorChoiceArg::Always => anstream::ColorChoice::Always,
            ColorChoiceArg::Never => anstream::ColorChoice::Never,
        }
    }
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scaffold a root `patina.toml` and persist the default-repo pointer.
    Init(InitArgs),

    /// Bring an existing dotfile under management: copy it into a module and
    /// write a `[[file]]` or `[[directory]]` entry by source kind.
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

    /// Manage remote git sources. `list` reports each remote's pin, and `check`
    /// compares upstream tips against the lock without downloading objects.
    /// `update` bumps pins through the update gate, and `prune` removes cached
    /// checkouts no journal record references.
    Remote(RemoteArgs),

    /// Watch the repository and re-apply on source changes. `--foreground`
    /// runs the watcher inline in the current terminal; the
    /// `install` / `uninstall` / `start` / `stop` / `restart` / `status`
    /// subcommands manage the per-OS background service.
    Watch(WatchArgs),

    /// Manage Windows Defender path exclusions for the repository and its
    /// deployed targets. Windows-only: absent from `--help` on macOS/Linux.
    /// `apply` reconciles (adds missing, reaps patina-owned stale), `status`
    /// reads current vs desired unprivileged, `clear` removes all
    /// patina-owned. Weakening antivirus is deliberate: previewed, consented,
    /// and gated behind one UAC prompt.
    #[cfg(windows)]
    Defender(DefenderArgs),

    /// Debugging utilities, hidden from the top-level help summary. `journal`
    /// decodes a binary plan file post-mortem, and `drift-cache` decodes the
    /// watcher's drift cache.
    #[command(hide = true, subcommand, disable_help_subcommand = true)]
    Debug(DebugCommand),
}

/// Flags for `patina watch`.
///
/// `--foreground` runs the watcher loop inline, attached to the invoking
/// shell, and exits cleanly on Ctrl-C / SIGTERM. The lifecycle
/// subcommands (`install` / `uninstall` / `start` / `stop` / `restart` /
/// `status`) manage the per-OS background service. With
/// neither, the command reports that a mode must be chosen.
#[derive(Debug, Args, Default)]
#[command(disable_help_subcommand = true)]
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

/// Background-service lifecycle subcommands under `patina watch`.
///
/// Each operates on the per-OS service registration through the
/// `patina_core::watch::service` backend. `status` is read-only and takes the
/// shared lock. When the shared lock times out, `status` warns and proceeds
/// without it. Every other subcommand takes the exclusive lock, and a timeout
/// on that one is exit `4`.
#[derive(Debug, Subcommand, Clone)]
pub enum WatchCommand {
    /// Register the watcher as a per-user background service that launches at
    /// login. When the service is already installed, exits 1.
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

/// Flags for `patina remote`.
///
/// The verbs split by what they write. `update` and `prune` mutate the
/// working-tree lockfile and the per-machine cache, so both take the exclusive
/// lock. `list` and `check` take the shared lock: `list` does not write, and
/// `check` writes only the per-machine notice files it alone owns.
#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct RemoteArgs {
    /// The remote-source subcommand to run.
    #[command(subcommand)]
    pub command: RemoteCommand,

    /// Emit a JSON envelope instead of human output. Global, so it is accepted
    /// both before and after the subcommand (`patina remote list --json`).
    #[arg(long, global = true)]
    pub json: bool,
}

/// Subcommands under `patina remote`.
#[derive(Debug, Subcommand, Clone)]
pub enum RemoteCommand {
    /// Report each remote's URL, ref, pinned rev, and pending-update state.
    /// Read-only and offline: the pending state is read from the last
    /// `patina remote check`.
    List,

    /// Compare upstream tips against the lock with `git ls-remote` only, and
    /// refresh the pending-update notice. Does not download objects. Every pin
    /// is left as recorded.
    Check {
        /// Run as a shell hook: self-throttle to at most one real check per
        /// day, and stay silent on success.
        #[arg(long)]
        hook: bool,
    },

    /// Fetch upstream, run the update gate, and bump `rev` / `updated_at` in
    /// the working-tree lockfile for you to review and commit. Does not write
    /// to any target.
    Update {
        /// The remote to update. Every remote when omitted.
        #[arg(value_name = "name")]
        name: Option<String>,

        /// Bypass the age gate for this run only, with a visible warning. Every
        /// other gate check still applies.
        #[arg(long)]
        now: bool,

        /// Accept every gate confirmation automatically. Required to bump a pin
        /// the gate flags in a non-interactive shell.
        #[arg(long)]
        yes: bool,
    },

    /// Remove cached checkouts unreferenced by any journal record.
    Prune,
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
    /// directory. A missing directory is created.
    #[arg(value_name = "path")]
    pub path: Option<Utf8PathBuf>,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,

    /// Proceed without prompting. `init` is a mutating command; this is
    /// accepted for parity with the other mutating subcommands.
    #[arg(long)]
    pub yes: bool,
}

/// Flags for `patina add`.
///
/// The mode flags (`--symlink` / `--copy` / `--template` / `--symlink-tree`)
/// form a mutually-exclusive clap group: declaring more than one is a usage
/// error (exit 2). Which flags are legal depends on the source kind.
/// `--symlink` and `--copy` apply to either a file or a directory source;
/// `--template` is file-only; `--symlink-tree` is directory-only. clap cannot
/// see the source's on-disk kind, so `cmd::add` checks the flag against the
/// kind at use-site and raises a typed error.
#[derive(Debug, Args, Default)]
#[command(group = clap::ArgGroup::new("mode").multiple(false))]
#[expect(
    clippy::struct_excessive_bools,
    reason = "this is a clap-derived flag struct: each bool is an independent CLI flag (the mode flags, plus --json, --yes, and --force), not a state machine that would be better modelled as an enum. The mode flags are unified at use-site into the AddMode enum."
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

    /// File the entry as a byte copy (a recursive copy for a directory
    /// source).
    #[arg(long, group = "mode")]
    pub copy: bool,

    /// File the entry as a rendered template (file source only).
    #[arg(long, group = "mode")]
    pub template: bool,

    /// File a directory source as a per-leaf symbolic-link tree (directory
    /// source only).
    #[arg(long = "symlink-tree", group = "mode")]
    pub symlink_tree: bool,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,

    /// Proceed without prompting. `add` is a mutating command.
    #[arg(long)]
    pub yes: bool,

    /// Add the path even when a tree-mode entry's `ignore` list already
    /// excludes it. Separate from `--yes`: that flag only skips prompts, while
    /// this one overrides a validation refusal.
    #[arg(long)]
    pub force: bool,
}

/// Flags for `patina remove`.
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

    /// Proceed without prompting. `remove` is a mutating command.
    #[arg(long)]
    pub yes: bool,
}

/// Flags for `patina promote`.
#[derive(Debug, Args, Default)]
pub struct PromoteArgs {
    /// The drifted copy-mode target to promote. Absolute or HOME-relative (a
    /// leading `~` is expanded).
    #[arg(value_name = "target")]
    pub target: Utf8PathBuf,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,

    /// Proceed without prompting. `promote` is a mutating command.
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

/// Flags for `patina doctor`.
///
/// The read-only path (no `--fix`) takes only the shared lock and emits
/// findings. `--fix` takes the exclusive lock and interactively remediates
/// fixable findings. `--yes` auto-accepts every prompt.
#[derive(Debug, Args, Default)]
pub struct DoctorArgs {
    /// Interactively remediate fixable findings instead of only reporting
    /// them. Mutating: acquires the exclusive lock.
    #[arg(long)]
    pub fix: bool,

    /// Emit a JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,

    /// With `--fix`, accept every remediation prompt automatically. Required
    /// to run `--fix` in a non-TTY shell.
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "this is a clap-derived flag struct: each bool is an independent CLI flag (--yes / --force-deploy / --update / --json), not a state machine that would read better as an enum."
)]
pub struct ApplyArgs {
    /// Apply unconditionally with no prompt, regardless of TTY state.
    #[arg(long)]
    pub yes: bool,

    /// Override every hook in this invocation to `must_succeed = false`.
    #[arg(long)]
    pub force_deploy: bool,

    /// Run `patina remote update` for every remote before applying, so one run
    /// covers both the pin bump and the consent diff for its new bytes. When a
    /// remote is unreachable, the run warns and applies the pins already
    /// committed. This
    /// flag is ignored with `--json`, and skipped on a non-interactive preview
    /// without `--yes`. The apply's own `--yes` does not accept the update
    /// gate: a flagged bump is still held or prompted.
    #[arg(long)]
    pub update: bool,

    /// Emit a JSON envelope instead of human output. Without `--yes` this
    /// is a preview (no mutation); pair with `--yes` to apply.
    #[arg(long)]
    pub json: bool,

    /// CLI variable override, repeatable: `-v key=value`.
    #[arg(short = 'v', value_name = "key=value")]
    pub var: Vec<String>,
}

/// Flags for `patina defender` (Windows-only).
#[cfg(windows)]
#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct DefenderArgs {
    /// The Defender exclusion subcommand to run.
    #[command(subcommand)]
    pub command: DefenderCommand,
}

/// Subcommands under `patina defender` (Windows-only).
#[cfg(windows)]
#[derive(Debug, Subcommand)]
pub enum DefenderCommand {
    /// Reconcile Defender exclusions: add every desired exclusion that is
    /// missing and reap the patina-owned exclusions the current plan no
    /// longer manages. Previewed and consented; launches the elevated helper
    /// behind one UAC prompt.
    Apply {
        /// Proceed without prompting. Required to reconcile in a
        /// non-interactive shell.
        #[arg(long)]
        yes: bool,

        /// Emit a JSON envelope instead of human output.
        #[arg(long)]
        json: bool,
    },

    /// Report the current Defender exclusions against the desired set. A
    /// read-only, unprivileged `Get-MpPreference`; no elevation.
    Status {
        /// Emit a JSON envelope instead of human output.
        #[arg(long)]
        json: bool,
    },

    /// Remove every patina-owned Defender exclusion. An exclusion the user
    /// added is left untouched. Previewed and consented; launches the elevated
    /// helper behind one UAC prompt.
    Clear {
        /// Proceed without prompting. Required to clear in a non-interactive
        /// shell.
        #[arg(long)]
        yes: bool,

        /// Emit a JSON envelope instead of human output.
        #[arg(long)]
        json: bool,
    },
}
