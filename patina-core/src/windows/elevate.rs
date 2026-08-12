//! Windows-only one-time UAC elevation launch for the Developer Mode flow
//! (write side).
//!
//! This module is compiled only under `#[cfg(windows)]`. It is the launch
//! side: when [`super::decide_symlink_gate`] reports that
//! elevation is required, the CLI drives the one-time UAC flow by calling
//! [`launch_elevate_helper`], which locates the bundled `patina-elevate.exe`
//! beside the running `patina.exe`, launches it with the `runas` verb via
//! `ShellExecuteEx` (the OS renders the UAC consent UI), then re-reads the
//! Developer Mode registry flag to learn the outcome.
//!
//! The re-read polls rather than sampling once; the parent module's
//! `poll_until` carries why.
//!
//! The helper is a standalone crate with no `patina-core`
//! dependency; we invoke it purely as a sibling executable. The
//! engine never renders the UAC *prompt*, which is the CLI's job, but the
//! `ShellExecuteEx` launch and the post-launch flag re-read are an engine
//! capability and live here.

use super::WindowsError;
use super::registry;
use std::env;
use std::time::Duration;
use winsafe::co;

/// The verb that asks the shell to launch a target elevated, raising the
/// UAC consent dialog.
const RUNAS_VERB: &str = "runas";

/// The helper executable's file name, resolved as a sibling of the running
/// `patina` binary so a relocated install still finds its own helper.
const HELPER_EXE: &str = "patina-elevate.exe";

/// The subcommand the helper exposes to toggle the Developer Mode flag.
const HELPER_SUBCOMMAND: &str = "enable-developer-mode";

/// How the one-time UAC elevation attempt settled.
///
/// The CLI maps these onto its control flow: [`EnabledNow`] lets
/// the apply proceed; [`Declined`] is the exit-5 user-declined path; and
/// [`RanButStillDisabled`] is the typed exit-1 error naming the registry
/// path.
///
/// [`EnabledNow`]: ElevationOutcome::EnabledNow
/// [`Declined`]: ElevationOutcome::Declined
/// [`RanButStillDisabled`]: ElevationOutcome::RanButStillDisabled
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationOutcome {
    /// The helper ran and the Developer Mode flag now reads `1`; the apply
    /// may proceed.
    EnabledNow,
    /// The user dismissed the UAC consent dialog (the canonical
    /// `ERROR_CANCELLED` pattern); nothing was changed.
    Declined,
    /// The helper launched and returned, but the flag still does not read
    /// `1` afterward (the helper failed to write, or the write did not
    /// take). The CLI surfaces this as a typed error naming the registry
    /// path and exits 1.
    RanButStillDisabled,
}

/// Resolve the bundled `patina-elevate.exe` as a sibling of the running
/// executable, launch it elevated via `ShellExecuteEx` with the `runas`
/// verb, and re-read the Developer Mode flag to determine the outcome.
///
/// The main `patina.exe` process never runs elevated: only the
/// helper is launched elevated, via the OS consent UI. A user who dismisses
/// the UAC dialog yields [`ElevationOutcome::Declined`]
/// (`ERROR_CANCELLED`).
///
/// # Errors
///
/// Returns [`WindowsError`] when the running executable's path cannot be
/// resolved, or when the `ShellExecuteEx` launch fails for a reason other
/// than the user declining consent (which is reported as
/// [`ElevationOutcome::Declined`], not an error).
pub fn launch_elevate_helper() -> Result<ElevationOutcome, WindowsError> {
    let helper = helper_path()?;

    let info = winsafe::SHELLEXECUTEINFO {
        verb: Some(RUNAS_VERB),
        file: &helper,
        parameters: Some(HELPER_SUBCOMMAND),
        show: co::SW::HIDE,
        ..Default::default()
    };

    match winsafe::ShellExecuteEx(&info) {
        Ok(()) => Ok(reread_outcome()),
        // The user clicked "No" on the UAC dialog: not an error, the
        // canonical declined path (→ exit 5).
        Err(err) if err == co::ERROR::CANCELLED => Ok(ElevationOutcome::Declined),
        Err(err) => Err(WindowsError::WinApi {
            call: "ShellExecuteEx",
            source: std::io::Error::other(err),
        }),
    }
}

/// Resolve `patina-elevate.exe` next to the running `patina.exe`.
///
/// Shared with the Defender launch path ([`super::defender`]), which resolves
/// the same sibling helper for its own `runas` invocation.
pub(crate) fn helper_path() -> Result<String, WindowsError> {
    let current = env::current_exe().map_err(|source| WindowsError::WinApi {
        call: "GetModuleFileName",
        source,
    })?;
    let dir = current.parent().ok_or_else(|| WindowsError::WinApi {
        call: "GetModuleFileName",
        source: std::io::Error::other("running executable has no parent directory"),
    })?;
    Ok(dir.join(HELPER_EXE).to_string_lossy().into_owned())
}

/// How long to keep re-reading the Developer Mode flag before concluding the
/// helper did not set it.
///
/// The helper's work is one registry write, so the whole wait is its startup:
/// image load, runtime init, argument parsing. A few seconds covers a cold
/// start on a loaded machine.
const FLAG_DEADLINE: Duration = Duration::from_secs(10);

/// How often to re-read the flag while waiting. Short, because the write itself
/// is instant once the helper is running. The wait is startup, not work.
const FLAG_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Re-read the Developer Mode flag after the helper has run and classify
/// the result. A `1` means the toggle took; anything else (including a
/// failed read) means the apply must not proceed.
///
/// Polls rather than reading once, for the reason the parent module's
/// `poll_until` gives. Only a full deadline without a `1` is
/// [`ElevationOutcome::RanButStillDisabled`].
fn reread_outcome() -> ElevationOutcome {
    let enabled = super::poll_until(FLAG_DEADLINE, FLAG_POLL_INTERVAL, || {
        matches!(registry::read_dev_mode_flag(), Ok(Some(1))).then_some(())
    });
    match enabled {
        Some(()) => ElevationOutcome::EnabledNow,
        None => ElevationOutcome::RanButStillDisabled,
    }
}
