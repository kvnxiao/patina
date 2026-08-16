//! Patina CLI entry point.
//!
//! Parses the clap-derived command surface ([`cli`]) and dispatches to a
//! subcommand in [`cmd`]. Every subcommand returns an `anyhow::Result<i32>`,
//! and [`cli::resolve_exit_code`] maps that to the integer `main` hands to
//! [`std::process::exit`].

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

/// Detect whether stdin is a terminal.
///
/// A `NonInteractive` result suppresses every prompt. Each command then
/// decides on its own whether that previews, declines, or refuses.
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
    let mut reporter = StreamReporter::new(cli.color.choice());
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
        Command::Remote(args) => {
            let mut reader = StdinReader;
            cmd::remote::run(&args, detect_tty(), &mut reader, &mut reporter)
        }
        Command::Watch(args) => cmd::watch::run(&args, &mut reporter).await,
        #[cfg(windows)]
        Command::Defender(args) => {
            let mut reader = StdinReader;
            cmd::defender::run(&args, detect_tty(), &mut reader, &mut reporter)
        }
        // A decode failure is a terminal state, not an engine error, so
        // `debug` returns its exit code directly.
        Command::Debug(command) => Ok(cmd::debug::run(&command, &mut reporter)),
    };
    std::process::exit(resolve_exit_code(outcome, &mut reporter));
}
