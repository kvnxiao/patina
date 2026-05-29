//! The clap-derived command-line surface for `patina` (REQ-017).
//!
//! `apply`, `status`, and `rollback` land so far; the `debug` family is
//! wired by its own task. The derive surface is kept thin — parsing only —
//! so the command logic lives in [`crate::cmd`] and stays unit-testable
//! without going through clap.

use camino::Utf8PathBuf;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;

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
    /// Materialize the declared configuration at its targets.
    Apply(ApplyArgs),

    /// Report drift between the repository and the materialized targets.
    Status(StatusArgs),

    /// Reverse the most recent successful apply via the journal and backups.
    Rollback(RollbackArgs),

    /// Debugging utilities. Hidden from the top-level help summary but
    /// documented; `journal` decodes a binary plan file post-mortem.
    #[command(hide = true, subcommand)]
    Debug(DebugCommand),
}

/// Subcommands under the `patina debug` namespace.
#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// Decode a `<ts>.plan` journal file into a human-readable view.
    Journal(DebugJournalArgs),
}

/// Flags for `patina debug journal`.
#[derive(Debug, Args)]
pub struct DebugJournalArgs {
    /// Path to the `<ts>.plan` file to decode.
    #[arg(value_name = "path")]
    pub path: Utf8PathBuf,
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
