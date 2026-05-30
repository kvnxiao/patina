//! Patina CLI entry point.
//!
//! Parses the clap-derived command surface ([`cli`]) and dispatches to a
//! subcommand in [`cmd`]. User-facing output is routed exclusively through
//! the [`output::reporter::Reporter`] layer; logs go through `tracing`.
//! The process exit code is owned by the subcommand (REQ-022's exit-code
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
use is_terminal::IsTerminal;
use output::reporter::StreamReporter;

#[tokio::main]
async fn main() -> ! {
    let cli = Cli::parse();
    let mut reporter = StreamReporter::new();
    let outcome = match cli.command {
        Command::Init(args) => cmd::init::run(&args, &mut reporter).await,
        Command::Add(args) => {
            let tty = if std::io::stdin().is_terminal() {
                Tty::Interactive
            } else {
                Tty::NonInteractive
            };
            let mut reader = StdinReader;
            cmd::add::run(&args, tty, &mut reader, &mut reporter).await
        }
        Command::Remove(args) => {
            let tty = if std::io::stdin().is_terminal() {
                Tty::Interactive
            } else {
                Tty::NonInteractive
            };
            let mut reader = StdinReader;
            cmd::remove::run(&args, tty, &mut reader, &mut reporter).await
        }
        Command::Apply(args) => {
            let tty = if std::io::stdin().is_terminal() {
                Tty::Interactive
            } else {
                Tty::NonInteractive
            };
            let mut reader = StdinReader;
            cmd::apply::run(&args, tty, &mut reader, &mut reporter).await
        }
        Command::Status(args) => cmd::status::run(&args, &mut reporter).await,
        Command::Rollback(args) => {
            let tty = if std::io::stdin().is_terminal() {
                Tty::Interactive
            } else {
                Tty::NonInteractive
            };
            let mut reader = StdinReader;
            cmd::rollback::run(&args, tty, &mut reader, &mut reporter).await
        }
        // `debug` reports its own terminal state as an exit code already.
        Command::Debug(command) => Ok(cmd::debug::run(&command, &mut reporter)),
    };
    std::process::exit(resolve_exit_code(outcome, &mut reporter));
}
