//! `patina-elevate`, a standalone Windows-only privilege helper.
//!
//! `patina.exe` re-invokes this binary through `ShellExecuteEx` with the
//! `runas` verb, raising exactly one UAC prompt. The helper runs the single
//! requested action and exits with a documented code.
//!
//! `enable-developer-mode` sets the Developer Mode registry switch
//! (`AllowDevelopmentWithoutDevLicense` under `AppModelUnlock` in `HKLM`) to
//! `1`. `apply-defender-exclusions` reads a request file naming Windows
//! Defender path exclusions to add and remove, re-validates every path, then
//! applies them through the `Defender` PowerShell module and verifies the
//! change with a mandatory re-read. The helper depends on no other workspace
//! crate, so the UAC prompt gates a binary with no workspace dependencies.
//!
//! ## Library and thin binary split
//!
//! The command surface ([`Cli`], [`run`]) lives in the library, where the
//! cross-platform tests exercise the parsing contract on a host that never
//! builds the `windows`-gated binary.
//!
//! ## Exit codes
//!
//! | Code | Meaning                                                        |
//! |------|----------------------------------------------------------------|
//! | 0    | The requested action succeeded.                                |
//! | 1    | The action ran but failed (e.g. non-elevated → access denied, or a Defender write blocked by Tamper Protection). |
//! | 2    | Argument parsing failed (unknown subcommand / usage). clap.    |

use clap::CommandFactory;
use clap::Parser;
use clap::Subcommand;
use clap::error::ErrorKind;
use std::path::PathBuf;
use std::process::ExitCode;

pub mod defender;
pub mod devmode;

/// `patina-elevate`: perform one elevated action and exit.
#[derive(Debug, Parser)]
#[command(name = "patina-elevate", version, about)]
pub struct Cli {
    /// The elevated action to perform.
    #[command(subcommand)]
    pub command: Command,
}

/// Parse the process arguments into a [`Cli`].
///
/// # Errors
///
/// On [`ErrorKind::InvalidSubcommand`], writes clap's rendered error to
/// stderr, appends a line listing the subcommands derived from the command
/// definition, and returns exit code `2`. On every other error kind,
/// including help, version, and the no-subcommand path, prints the error
/// through [`clap::Error::print`] and returns clap's own exit code.
///
/// # Examples
///
/// ```no_run
/// let code = match patina_elevate::parse() {
///     Ok(cli) => patina_elevate::run(&cli.command),
///     Err(code) => code,
/// };
/// ```
pub fn parse() -> Result<Cli, ExitCode> {
    let error = match Cli::try_parse() {
        Ok(cli) => return Ok(cli),
        Err(error) => error,
    };
    if error.kind() == ErrorKind::InvalidSubcommand {
        use std::io::Write as _;
        let mut stderr = std::io::stderr().lock();
        let listing = supported_subcommands();
        let rendered = write!(stderr, "{error}")
            .and_then(|()| writeln!(stderr, "Supported subcommands: {listing}"));
        // Return exit code 2 even if the stderr write failed.
        drop(rendered);
        return Err(ExitCode::from(2));
    }
    drop(error.print());
    Err(u8::try_from(error.exit_code()).map_or(ExitCode::FAILURE, ExitCode::from))
}

fn supported_subcommands() -> String {
    Cli::command()
        .get_subcommands()
        .map(clap::Command::get_name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The set of elevated actions the helper supports.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Set the Developer Mode registry flag
    /// (`AllowDevelopmentWithoutDevLicense`) to `1`.
    EnableDeveloperMode,

    /// Apply the Windows Defender path exclusions listed in a request file,
    /// re-validating each path and verifying the result with a re-read.
    ApplyDefenderExclusions {
        /// Absolute path to the request file that the unprivileged CLI wrote. A
        /// `runas` to a different admin resolves a different `%LOCALAPPDATA%`,
        /// so the helper reads this path and never recomputes the state
        /// directory.
        request: PathBuf,
    },
}

/// Dispatch a parsed command to its action and resolve the exit code.
///
/// The action's outcome maps to `0` on success, or `1` after writing the typed
/// failure to stderr. [`parse`] owns the exit-`2` usage path.
#[must_use = "the returned code is the process's terminal exit status"]
pub fn run(command: &Command) -> ExitCode {
    match command {
        Command::EnableDeveloperMode => {
            report_result("enable-developer-mode", devmode::enable_developer_mode())
        }
        Command::ApplyDefenderExclusions { request } => report_result(
            "apply-defender-exclusions",
            defender::apply_defender_exclusions(request),
        ),
    }
}

#[expect(
    clippy::print_stderr,
    reason = "the helper has no Reporter; the typed error on stderr is the documented exit-1 path"
)]
fn report_result<E: std::error::Error>(action: &str, result: Result<(), E>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("patina-elevate: {action} failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn parses_the_enable_developer_mode_subcommand() {
        let cli = Cli::try_parse_from(["patina-elevate", "enable-developer-mode"])
            .expect("enable-developer-mode is a valid invocation");
        assert_eq!(cli.command, Command::EnableDeveloperMode);
    }

    #[test]
    fn parses_the_apply_defender_exclusions_subcommand() {
        let cli = Cli::try_parse_from([
            "patina-elevate",
            "apply-defender-exclusions",
            r"C:\Users\kevin\AppData\Local\patina\defender-request.txt",
        ])
        .expect("apply-defender-exclusions is a valid invocation");
        assert_eq!(
            cli.command,
            Command::ApplyDefenderExclusions {
                request: std::path::PathBuf::from(
                    r"C:\Users\kevin\AppData\Local\patina\defender-request.txt"
                )
            }
        );
    }

    #[test]
    fn unknown_subcommand_is_a_usage_error() {
        let err = Cli::try_parse_from(["patina-elevate", "frobnicate"])
            .expect_err("an unknown subcommand must be rejected");
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rendered_help_lists_the_supported_subcommand() {
        let mut cmd = <Cli as clap::CommandFactory>::command();
        let help = cmd.render_long_help().to_string();
        assert!(
            help.contains("enable-developer-mode"),
            "help must list the supported subcommand; got:\n{help}"
        );
    }

    #[test]
    fn missing_subcommand_does_not_run_an_action() {
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
        let err = devmode::enable_developer_mode()
            .expect_err("enable-developer-mode is unsupported off Windows");
        assert!(matches!(err, devmode::DevModeError::NotWindows));
    }

    #[cfg(not(windows))]
    #[test]
    fn apply_defender_on_non_windows_reports_not_windows() {
        let err = defender::apply_defender_exclusions(std::path::Path::new("/tmp/request.txt"))
            .expect_err("apply-defender-exclusions is unsupported off Windows");
        assert!(matches!(err, defender::DefenderError::NotWindows));
    }

    #[cfg(not(windows))]
    #[test]
    fn run_dispatches_apply_defender_exclusions_to_a_failure_exit() {
        let code = run(&Command::ApplyDefenderExclusions {
            request: PathBuf::from("/tmp/patina-defender-request.txt"),
        });
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    #[test]
    fn report_result_maps_ok_to_a_success_exit_code() {
        let code = report_result::<std::io::Error>("test-action", Ok(()));
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    }
}
