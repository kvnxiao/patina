//! Patina CLI entry point.
//!
//! Parses the clap-derived command surface ([`cli`]) and dispatches to a
//! subcommand in [`cmd`]. User-facing output is routed exclusively through
//! the [`output::reporter::Reporter`] layer; logs go through `tracing`.
//! The process exit code is owned by the subcommand (REQ-017's exit-code
//! contract), so `main` translates the returned code into
//! [`std::process::exit`].

mod cli;
mod cmd;
mod output;

use clap::Parser;
use cli::Cli;
use cli::Command;
use cmd::apply::StdinReader;
use cmd::apply::Tty;
use is_terminal::IsTerminal;
use output::reporter::StreamReporter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Apply(args) => {
            let tty = if std::io::stdin().is_terminal() {
                Tty::Interactive
            } else {
                Tty::NonInteractive
            };
            let mut reader = StdinReader;
            let mut reporter = StreamReporter::new();
            cmd::apply::run(&args, tty, &mut reader, &mut reporter).await?
        }
        Command::Status(args) => {
            let mut reporter = StreamReporter::new();
            cmd::status::run(&args, &mut reporter).await?
        }
        Command::Rollback(args) => {
            let tty = if std::io::stdin().is_terminal() {
                Tty::Interactive
            } else {
                Tty::NonInteractive
            };
            let mut reader = StdinReader;
            let mut reporter = StreamReporter::new();
            cmd::rollback::run(&args, tty, &mut reader, &mut reporter).await?
        }
    };
    std::process::exit(code);
}
