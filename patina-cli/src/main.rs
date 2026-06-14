//! Patina CLI entry point.
//!
//! Parses the clap-derived command surface ([`cli`]) and dispatches to a
//! subcommand in [`cmd`]. User-facing output is routed exclusively through
//! the [`output::reporter::Reporter`] layer; logs go through `tracing`.
//! The process exit code is owned by the subcommand (the exit-code
//! contract). Each command returns an `anyhow::Result<i32>`: an `Ok(code)`
//! is a terminal state under the command's own control, an `Err` is an
//! engine failure. Both funnel through [`cli::resolve_exit_code`], the
//! single place the exit-code mapping lives, before `main` hands the
//! result to [`std::process::exit`].

mod cli;
mod cmd;
mod exit_code;
mod output;

use clap::Parser;
use cli::Cli;
use cli::Command;
use cli::resolve_exit_code;
use cmd::apply::StdinReader;
use cmd::apply::Tty;
use output::reporter::StreamReporter;
use std::io::IsTerminal;

/// Classify the current stdin as an interactive terminal or not, so prompt
/// flows fall through to plan-only / non-interactive behavior off a TTY.
fn detect_tty() -> Tty {
    if std::io::stdin().is_terminal() {
        Tty::Interactive
    } else {
        Tty::NonInteractive
    }
}

#[tokio::main]
async fn main() -> ! {
    let cli = Cli::parse();
    let mut reporter = StreamReporter::new();
    let outcome = match cli.command {
        Command::Init(args) => cmd::init::run(&args, &mut reporter).await,
        Command::Add(args) => {
            let mut reader = StdinReader;
            cmd::add::run(&args, detect_tty(), &mut reader, &mut reporter).await
        }
        Command::Remove(args) => {
            let mut reader = StdinReader;
            cmd::remove::run(&args, detect_tty(), &mut reader, &mut reporter).await
        }
        Command::Promote(args) => {
            let mut reader = StdinReader;
            cmd::promote::run(&args, detect_tty(), &mut reader, &mut reporter).await
        }
        Command::Apply(args) => {
            let mut reader = StdinReader;
            cmd::apply::run(&args, detect_tty(), &mut reader, &mut reporter).await
        }
        Command::Status(args) => cmd::status::run(&args, &mut reporter).await,
        Command::Doctor(args) => {
            let mut reader = StdinReader;
            cmd::doctor::run(&args, detect_tty(), &mut reader, &mut reporter)
        }
        Command::Rollback(args) => {
            let mut reader = StdinReader;
            cmd::rollback::run(&args, detect_tty(), &mut reader, &mut reporter).await
        }
        Command::Watch(args) => cmd::watch::run(&args, &mut reporter).await,
        // `debug` reports its own terminal state as an exit code already.
        Command::Debug(command) => Ok(cmd::debug::run(&command, &mut reporter)),
    };
    std::process::exit(resolve_exit_code(outcome, &mut reporter));
}
