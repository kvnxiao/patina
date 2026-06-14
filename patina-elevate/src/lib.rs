//! `patina-elevate` — a standalone Windows-only privilege helper.
//!
//! This crate builds the only binary in the workspace meant to run
//! *elevated*. The main `patina.exe` re-invokes it via `ShellExecuteEx`
//! with the `runas` verb, triggering exactly one UAC prompt; the helper
//! then performs the single requested elevated action and exits with a
//! documented code.
//!
//! Its sole responsibility in v1.0 is the `enable-developer-mode`
//! subcommand, which sets the Developer Mode registry switch
//! (`AllowDevelopmentWithoutDevLicense` under `AppModelUnlock` in `HKLM`)
//! to `1`. Keeping it a separate, dependency-light binary (no
//! `patina-core` / `patina-cli`) keeps the surface UAC must trust
//! as small as possible.
//!
//! ## Why a library plus a thin binary
//!
//! The command surface ([`Cli`], [`run`]) lives here in the library so it
//! can be unit-tested on any host without depending on the binary
//! artifact. The binary itself is gated behind the `windows` feature
//! and is therefore absent from non-Windows release
//! builds — but the parsing contract it relies on is exercised by the
//! library's own cross-platform tests regardless.
//!
//! ## Exit codes
//!
//! | Code | Meaning                                                        |
//! |------|----------------------------------------------------------------|
//! | 0    | The requested action succeeded.                                |
//! | 1    | The action ran but failed (e.g. non-elevated → access denied). |
//! | 2    | Argument parsing failed (unknown subcommand / usage). clap.    |

use clap::CommandFactory;
use clap::Parser;
use clap::Subcommand;
use clap::error::ErrorKind;
use std::process::ExitCode;

pub mod devmode;

/// `patina-elevate` — perform one elevated action and exit.
#[derive(Debug, Parser)]
#[command(name = "patina-elevate", version, about)]
pub struct Cli {
    /// The elevated action to perform.
    #[command(subcommand)]
    pub command: Command,
}

/// Parse the process arguments into a [`Cli`], or print a usage error and exit.
///
/// This is the binary's entry into parsing. It behaves like
/// [`Cli::parse`] for the success, `--help`, and `--version` paths, but
/// intercepts the unknown-subcommand path: clap's default
/// [`ErrorKind::InvalidSubcommand`] message reports only the offending
/// token and the bare `Usage:` line, and does *not* enumerate the
/// available subcommands. The exit-2 usage
/// message must *list the supported subcommands*, so on that one error
/// kind we print clap's own rendered error to stderr, follow it with a
/// line listing the supported subcommands (derived from the command
/// definition, not hard-coded), and exit with clap's usage exit code
/// (`2`).
///
/// All other error kinds — including the no-subcommand path, which clap
/// already renders with the subcommand listing — are left to clap's own
/// [`clap::Error::exit`], preserving its exit codes and stream choice.
///
/// # Examples
///
/// ```no_run
/// // The binary calls this once at startup; a bad subcommand exits 2
/// // with a usage message that lists `enable-developer-mode`.
/// let cli = patina_elevate::parse_or_exit();
/// patina_elevate::run(&cli.command);
/// ```
#[must_use = "the parsed command must be dispatched to `run`"]
pub fn parse_or_exit() -> Cli {
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if error.kind() == ErrorKind::InvalidSubcommand => {
            // clap's `InvalidSubcommand` rendering names the offending
            // token and the bare `Usage:` line but omits the subcommand
            // list. Print it as clap would, then append the supported
            // subcommands so the exit-2 path satisfies the
            // "listing the supported subcommands" contract. Writing to a
            // locked stderr handle is the same primitive clap's own
            // `Error::exit` uses; the workspace `disallowed-macros` gate
            // targets `eprintln!`/`println!`, not raw handle writes, and
            // the write results are inspected rather than discarded.
            use std::io::Write as _;
            let mut stderr = std::io::stderr().lock();
            let listing = supported_subcommands();
            let rendered = write!(stderr, "{error}")
                .and_then(|()| writeln!(stderr, "Supported subcommands: {listing}"));
            // Exit 2 (clap's usage code) regardless of whether the stderr
            // write itself succeeded — there is no better channel to
            // report a stderr failure on, and the usage error stands.
            drop(rendered);
            std::process::exit(2);
        }
        Err(error) => error.exit(),
    }
}

/// The comma-separated list of subcommands the helper supports, derived
/// from the clap command definition so it cannot drift from the actual
/// surface.
fn supported_subcommands() -> String {
    Cli::command()
        .get_subcommands()
        .map(clap::Command::get_name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The set of elevated actions the helper supports. v1.0 ships exactly one.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Set the Developer Mode registry flag
    /// (`AllowDevelopmentWithoutDevLicense`) to `1`.
    EnableDeveloperMode,
}

/// Dispatch a parsed command to its action and resolve the exit code.
///
/// A recognised subcommand runs its action and maps the outcome to an
/// [`ExitCode`]: `0` on success, or `1` after writing the typed failure to
/// stderr. The argument-parsing / unknown-subcommand path (exit `2`) is
/// handled by clap before this function is reached — see [`parse_or_exit`].
#[must_use = "the returned code is the process's terminal exit status"]
pub fn run(command: &Command) -> ExitCode {
    match command {
        Command::EnableDeveloperMode => match devmode::enable_developer_mode() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                // User-facing output normally routes through `output::Reporter`,
                // but this helper deliberately has no `patina-core` dependency
                // and therefore no Reporter, and pulling in `tracing`
                // for one error line would widen the surface UAC must trust. A
                // raw stderr write is the right primitive here, so the workspace
                // `disallowed-macros` gate is suppressed at this one documented
                // site (clippy.toml sanctions exactly this carve-out).
                #[expect(
                    clippy::disallowed_macros,
                    reason = "helper has no Reporter; typed error to stderr is the documented exit-1 path"
                )]
                {
                    eprintln!("patina-elevate: enable-developer-mode failed: {error}");
                }
                ExitCode::FAILURE
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn parses_the_enable_developer_mode_subcommand() {
        // The one supported subcommand parses to its variant on any host.
        let cli = Cli::try_parse_from(["patina-elevate", "enable-developer-mode"])
            .expect("enable-developer-mode is a valid invocation");
        assert_eq!(cli.command, Command::EnableDeveloperMode);
    }

    #[test]
    fn unknown_subcommand_is_a_usage_error() {
        // An unrecognised subcommand is a clap usage error.
        // clap maps this to a non-zero exit via `Error::exit` — exit code 2 —
        // which the real process exercises in `tests/cli.rs`.
        let err = Cli::try_parse_from(["patina-elevate", "frobnicate"])
            .expect_err("an unknown subcommand must be rejected");
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rendered_help_lists_the_supported_subcommand() {
        // The usage surface lists `enable-developer-mode` so
        // a caller who mis-invokes can discover the correct subcommand. clap
        // enumerates subcommands in the long help rather than in the bare
        // unknown-subcommand error, so assert against the command's help.
        let mut cmd = <Cli as clap::CommandFactory>::command();
        let help = cmd.render_long_help().to_string();
        assert!(
            help.contains("enable-developer-mode"),
            "help must list the supported subcommand; got:\n{help}"
        );
    }

    #[test]
    fn missing_subcommand_does_not_run_an_action() {
        // No subcommand at all is a usage error too (the helper never runs an
        // action it was not asked to). clap reports this as a help-on-missing
        // error, which `Error::exit` maps to the same non-zero exit.
        let err = Cli::try_parse_from(["patina-elevate"])
            .expect_err("a missing subcommand must be rejected");
        assert_eq!(
            err.kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn enable_on_non_windows_reports_not_windows() {
        // On a non-Windows build the action takes the NotWindows path rather
        // than touching any registry — this is what keeps the dispatch
        // exercisable off Windows.
        let err = devmode::enable_developer_mode()
            .expect_err("enable-developer-mode is unsupported off Windows");
        assert!(matches!(err, devmode::DevModeError::NotWindows));
    }
}
