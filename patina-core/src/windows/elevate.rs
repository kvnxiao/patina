//! Windows-only one-time UAC elevation launch for the Developer Mode flow
//! (REQ-007, write side).
//!
//! This module is compiled only under `#[cfg(windows)]`. It is the launch
//! side of REQ-007: when [`super::decide_symlink_gate`] reports that
//! elevation is required, the CLI drives the one-time UAC flow by calling
//! [`launch_elevate_helper`], which locates the bundled `patina-elevate.exe`
//! beside the running `patina.exe`, launches it with the `runas` verb via
//! `ShellExecuteEx` (the OS renders the UAC consent UI), then re-reads the
//! Developer Mode registry flag to learn the outcome.
//!
//! Per DEC-002 the helper is a standalone crate with no `patina-core`
//! dependency; we invoke it purely as a sibling executable. Per DEC-008 the
//! engine never renders the UAC *prompt* — that is the CLI's job — but the
//! `ShellExecuteEx` launch and the post-launch flag re-read are an engine
//! capability and live here.

use super::WindowsError;
use super::registry;
use std::env;
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
/// The CLI maps these onto its control flow (REQ-007): [`EnabledNow`] lets
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
/// The main `patina.exe` process never runs elevated (REQ-007): only the
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
        // canonical declined path (REQ-007 → exit 5).
        Err(err) if err == co::ERROR::CANCELLED => Ok(ElevationOutcome::Declined),
        Err(err) => Err(WindowsError::WinApi {
            call: "ShellExecuteEx",
            source: std::io::Error::other(err),
        }),
    }
}

/// Resolve `patina-elevate.exe` next to the running `patina.exe`.
fn helper_path() -> Result<String, WindowsError> {
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

/// Re-read the Developer Mode flag after the helper has run and classify
/// the result. A `1` means the toggle took; anything else (including a
/// failed read) means the apply must not proceed.
fn reread_outcome() -> ElevationOutcome {
    match registry::read_dev_mode_flag() {
        Ok(Some(1)) => ElevationOutcome::EnabledNow,
        Ok(_) | Err(_) => ElevationOutcome::RanButStillDisabled,
    }
}
