//! `patina-elevate`, a Windows-only privilege helper.
//!
//! `patina.exe` re-invokes this binary through `ShellExecuteEx` with the
//! `runas` verb, raising exactly one UAC prompt. The helper runs the single
//! requested action and exits with a documented code.
//!
//! `enable-developer-mode` sets the Developer Mode registry switch
//! (`AllowDevelopmentWithoutDevLicense` under `AppModelUnlock` in `HKLM`) to
//! `1`. `apply-defender-exclusions` reads a request file naming Windows
//! Defender path exclusions to add and remove, re-validates every path, applies
//! them through the `Defender` PowerShell module, and verifies the change with
//! a mandatory re-read. The helper has no workspace-crate dependency.
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
//! | 2    | Argument parsing failed (unknown subcommand or usage error).   |

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

/// Parse process arguments or exit with a usage error.
///
/// On [`ErrorKind::InvalidSubcommand`], writes clap's rendered error to
/// stderr, appends a line listing the subcommands derived from the command
/// definition, and exits `2`. Every other error kind, the no-subcommand path
/// included, exits through [`clap::Error::exit`] with clap's own code and
/// stream.
///
/// # Examples
///
/// ```no_run
/// let cli = patina_elevate::parse_or_exit();
/// patina_elevate::run(&cli.command);
/// ```
#[must_use = "the parsed command must be dispatched to `run`"]
pub fn parse_or_exit() -> Cli {
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if error.kind() == ErrorKind::InvalidSubcommand => {
            // Raw handle writes are the only stderr path allowed by the
            // workspace's `disallowed_macros` lint here.
            use std::io::Write as _;
            let mut stderr = std::io::stderr().lock();
            let listing = supported_subcommands();
            let rendered = write!(stderr, "{error}")
                .and_then(|()| writeln!(stderr, "Supported subcommands: {listing}"));
            // The usage error still maps to exit 2 when stderr rejects the write.
            drop(rendered);
            std::process::exit(2);
        }
        Err(error) => error.exit(),
    }
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
/// failure to stderr. [`parse_or_exit`] owns the exit-`2` usage path.
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

fn report_result<E: std::error::Error>(action: &str, result: Result<(), E>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            #[expect(
                clippy::disallowed_macros,
                reason = "The helper has no Reporter; write the typed error to stderr for exit 1."
            )]
            {
                eprintln!("patina-elevate: {action} failed: {error}");
            }
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
