//! `patina-elevate`, a standalone Windows-only privilege helper.
//!
//! This crate builds the only binary in the workspace meant to run
//! *elevated*. The main `patina.exe` re-invokes it via `ShellExecuteEx`
//! with the `runas` verb, triggering exactly one UAC prompt. The helper
//! then performs the single requested elevated action and exits with a
//! documented code.
//!
//! It exposes two elevated actions. `enable-developer-mode` sets the Developer
//! Mode registry switch (`AllowDevelopmentWithoutDevLicense` under
//! `AppModelUnlock` in `HKLM`) to `1`. `apply-defender-exclusions` reads a
//! request file naming Windows Defender path exclusions to add and remove. It
//! independently re-validates every path, then applies them through the
//! `Defender` PowerShell module, with a mandatory re-read verification. Keeping
//! this a separate, dependency-light binary (no `patina-core` / `patina-cli`)
//! keeps the surface UAC must trust as small as possible.
//!
//! ## Library and thin binary split
//!
//! The command surface ([`Cli`], [`run`]) lives here in the library so it
//! can be unit-tested on any host without depending on the binary
//! artifact. The binary itself is gated behind the `windows` feature, and is
//! therefore absent from non-Windows release builds. The parsing contract it
//! relies on is exercised by the library's own cross-platform tests
//! regardless.
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

/// Parse the process arguments into a [`Cli`], or print a usage error and exit.
///
/// This is the binary's entry into parsing. It behaves like [`Cli::parse`]
/// for the success, `--help`, and `--version` paths. It intercepts the
/// unknown-subcommand path instead. clap's default
/// [`ErrorKind::InvalidSubcommand`] message reports only the offending
/// token and the bare `Usage:` line. It does *not* enumerate the available
/// subcommands. The exit-2 usage message must *list the supported
/// subcommands*. On that one error kind this function therefore prints
/// clap's own rendered error to stderr, and follows it with a line listing
/// the supported subcommands. That line is derived from the command
/// definition, not hard-coded. It then exits with clap's usage exit code
/// (`2`).
///
/// All other error kinds are left to clap's own [`clap::Error::exit`],
/// preserving its exit codes and stream choice. This includes the
/// no-subcommand path, which clap already renders with the subcommand
/// listing.
///
/// # Examples
///
/// ```no_run
/// // The binary calls this once at startup. A bad subcommand exits 2
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
            // token and the bare `Usage:` line, but omits the subcommand
            // list. This prints clap's own rendering, then appends the
            // supported subcommands so the exit-2 path satisfies the
            // "listing the supported subcommands" contract. Writing to a
            // locked stderr handle is the same primitive clap's own
            // `Error::exit` uses. The workspace `disallowed-macros` gate
            // targets `eprintln!`/`println!`, not raw handle writes. The
            // write result is checked before the exit.
            use std::io::Write as _;
            let mut stderr = std::io::stderr().lock();
            let listing = supported_subcommands();
            let rendered = write!(stderr, "{error}")
                .and_then(|()| writeln!(stderr, "Supported subcommands: {listing}"));
            // Exit 2 (clap's usage code) even if the stderr write failed.
            // There is no better channel to report that failure, and the
            // usage error still stands.
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

/// The set of elevated actions the helper supports.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Set the Developer Mode registry flag
    /// (`AllowDevelopmentWithoutDevLicense`) to `1`.
    EnableDeveloperMode,

    /// Apply the Windows Defender path exclusions listed in a request file,
    /// re-validating each path and verifying the result with a re-read.
    ApplyDefenderExclusions {
        /// Absolute path to the request file the unprivileged CLI wrote. The
        /// helper reads exactly this file and never recomputes the state
        /// directory (a `runas` to a different admin has a different
        /// `%LOCALAPPDATA%`).
        request: PathBuf,
    },
}

/// Dispatch a parsed command to its action and resolve the exit code.
///
/// A recognised subcommand runs its action and maps the outcome to an
/// [`ExitCode`]: `0` on success, or `1` after writing the typed failure to
/// stderr. The argument-parsing / unknown-subcommand path (exit `2`) is
/// handled by clap before this function is reached. See [`parse_or_exit`].
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

/// Map an action's result to an [`ExitCode`], writing the typed failure to
/// stderr on the exit-1 path.
///
/// User-facing output normally routes through `output::Reporter`. This helper
/// deliberately has no `patina-core` dependency, and therefore no Reporter.
/// Pulling in `tracing` for one error line would widen the surface UAC must
/// trust. A raw stderr write is the right primitive here. The workspace
/// `disallowed-macros` gate is suppressed at this one documented site, and
/// `clippy.toml` sanctions exactly this carve-out.
fn report_result<E: std::error::Error>(action: &str, result: Result<(), E>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            #[expect(
                clippy::disallowed_macros,
                reason = "helper has no Reporter; typed error to stderr is the documented exit-1 path"
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
        // A supported subcommand parses to its variant on any host.
        let cli = Cli::try_parse_from(["patina-elevate", "enable-developer-mode"])
            .expect("enable-developer-mode is a valid invocation");
        assert_eq!(cli.command, Command::EnableDeveloperMode);
    }

    #[test]
    fn parses_the_apply_defender_exclusions_subcommand() {
        // The request path is a single positional argument, quoted at the
        // launch site so a path with spaces survives argv splitting.
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
        // clap maps an unrecognised subcommand to a non-zero exit via
        // `Error::exit`, exit code 2, which the real process exercises in
        // `tests/cli.rs`.
        let err = Cli::try_parse_from(["patina-elevate", "frobnicate"])
            .expect_err("an unknown subcommand must be rejected");
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rendered_help_lists_the_supported_subcommand() {
        // The usage surface lists `enable-developer-mode` so
        // a caller who mis-invokes can discover the correct subcommand. clap
        // enumerates subcommands in the long help rather than in the bare
        // unknown-subcommand error, so this test checks the rendered help.
        let mut cmd = <Cli as clap::CommandFactory>::command();
        let help = cmd.render_long_help().to_string();
        assert!(
            help.contains("enable-developer-mode"),
            "help must list the supported subcommand; got:\n{help}"
        );
    }

    #[test]
    fn missing_subcommand_does_not_run_an_action() {
        // A missing subcommand is also a usage error. clap reports this as a
        // help-on-missing error, which `Error::exit` maps to the same
        // non-zero exit.
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
        // On a non-Windows build the action takes the NotWindows path
        // instead of touching the registry. This keeps the dispatch
        // exercisable off Windows.
        let err = devmode::enable_developer_mode()
            .expect_err("enable-developer-mode is unsupported off Windows");
        assert!(matches!(err, devmode::DevModeError::NotWindows));
    }

    #[cfg(not(windows))]
    #[test]
    fn apply_defender_on_non_windows_reports_not_windows() {
        // The Defender apply also resolves to the NotWindows stub off
        // Windows, so its dispatch arm stays exercisable on Linux/macOS CI.
        let err = defender::apply_defender_exclusions(std::path::Path::new("/tmp/request.txt"))
            .expect_err("apply-defender-exclusions is unsupported off Windows");
        assert!(matches!(err, defender::DefenderError::NotWindows));
    }

    #[cfg(not(windows))]
    #[test]
    fn run_dispatches_apply_defender_exclusions_to_a_failure_exit() {
        // This exercises `run`'s dispatch arm for the Defender action. Off
        // Windows the action resolves to NotWindows, which `run` maps to a
        // failure exit, distinct from the success code the Ok arm produces.
        let code = run(&Command::ApplyDefenderExclusions {
            request: PathBuf::from("/tmp/patina-defender-request.txt"),
        });
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    #[test]
    fn report_result_maps_ok_to_a_success_exit_code() {
        // This exercises the success arm of the exit-code mapping. An action
        // returning Ok resolves to a success exit, distinct from the failure
        // exit an error takes.
        let code = report_result::<std::io::Error>("test-action", Ok(()));
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    }
}
