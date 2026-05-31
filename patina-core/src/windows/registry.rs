//! Windows-only registry and process-token reads backing the Developer
//! Mode capability (REQ-007, read side).
//!
//! This module is compiled only under `#[cfg(windows)]`; the
//! cross-platform façade in the parent module routes here on Windows and
//! returns stub values everywhere else, so non-Windows builds never
//! resolve `winsafe` into the dependency graph.
//!
//! Three IO-free reads live here:
//!
//! - [`read_dev_mode_flag`] — the `AllowDevelopmentWithoutDevLicense` DWORD
//!   under `AppModelUnlock` (the Developer Mode switch).
//! - [`process_is_elevated`] — the current process token's `TokenIsElevated`
//!   flag.
//! - [`build_number`] — the OS build number, read from the `CurrentBuildNumber`
//!   registry string rather than `GetVersionEx`, which under-reports the build
//!   for unmanifested processes.
//!
//! The registry key path and value name are held as constants here. Per
//! DEC-002 the standalone `patina-elevate` helper crate duplicates these
//! constants deliberately — it must not depend on `patina-core` — so they
//! are intentionally not shared across a crate boundary.

use super::WindowsError;
use winsafe::co;

/// Registry subkey holding the Developer Mode switch.
pub(crate) const DEV_MODE_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock";

/// Value name under [`DEV_MODE_KEY`] that is `1` when Developer Mode is on.
pub(crate) const DEV_MODE_VALUE: &str = "AllowDevelopmentWithoutDevLicense";

/// Registry subkey holding the current OS build number.
pub(crate) const CURRENT_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

/// Value name under [`CURRENT_VERSION_KEY`] holding the build number as a
/// decimal string (e.g. `"19045"`).
pub(crate) const CURRENT_BUILD_VALUE: &str = "CurrentBuildNumber";

/// First Windows 10 build (1703 / Creators Update) to support Developer
/// Mode symbolic-link creation without elevation.
pub(crate) const DEV_MODE_MIN_BUILD: u32 = 15063;

/// Wrap a winsafe `co::ERROR` as a [`WindowsError`] naming the failing call.
fn win_err(call: &'static str, err: co::ERROR) -> WindowsError {
    WindowsError::WinApi {
        call,
        source: std::io::Error::other(err),
    }
}

/// Read the Developer Mode DWORD flag from `HKLM`.
///
/// Returns `Ok(Some(value))` when the value exists and is a DWORD,
/// `Ok(None)` when the key or value is absent (Developer Mode was never
/// toggled on this machine), or `Err` when the registry call itself fails
/// for a reason other than "not found".
///
/// # Errors
///
/// Returns [`WindowsError::WinApi`] when opening the key or querying the
/// value fails with an error other than `FILE_NOT_FOUND`.
pub(crate) fn read_dev_mode_flag() -> Result<Option<u32>, WindowsError> {
    read_dword(DEV_MODE_KEY, DEV_MODE_VALUE)
}

/// Read the OS build number from `CurrentBuildNumber`.
///
/// Returns `None` when the value is absent or cannot be parsed as a build
/// number; the parent module treats that as "build floor not met" and
/// reports [`super::DevModeStatus::Unsupported`].
pub(crate) fn build_number() -> Option<u32> {
    let key = winsafe::HKEY::LOCAL_MACHINE
        .RegOpenKeyEx(
            Some(CURRENT_VERSION_KEY),
            co::REG_OPTION::default(),
            co::KEY::READ,
        )
        .ok()?;
    match key.RegQueryValueEx(Some(CURRENT_BUILD_VALUE)).ok()? {
        winsafe::RegistryValue::Sz(text) => text.trim().parse().ok(),
        winsafe::RegistryValue::Dword(value) => Some(value),
        _ => None,
    }
}

/// Whether the current process token reports `TokenIsElevated`.
///
/// # Errors
///
/// Returns [`WindowsError::WinApi`] when opening the process token or
/// querying its elevation information fails.
pub(crate) fn process_is_elevated() -> Result<bool, WindowsError> {
    let token = winsafe::HPROCESS::GetCurrentProcess()
        .OpenProcessToken(co::TOKEN::QUERY)
        .map_err(|err| win_err("OpenProcessToken", err))?;
    match token
        .GetTokenInformation(co::TOKEN_INFORMATION_CLASS::Elevation)
        .map_err(|err| win_err("GetTokenInformation", err))?
    {
        winsafe::TokenInfo::Elevation(elevation) => Ok(elevation.TokenIsElevated()),
        // GetTokenInformation echoes the requested class; any other
        // variant is impossible for the Elevation class but is handled
        // without panicking per the no-panic rule.
        _ => Ok(false),
    }
}

/// Open `HKLM\{sub_key}` read-only and read `{value}` as a DWORD.
///
/// A missing key or value is reported as `Ok(None)` rather than an error
/// — Developer Mode being un-toggled is a normal state, not a failure.
/// (`co::ERROR` is a newtype over an integer and cannot appear in a match
/// pattern, so the not-found case is checked with `==`.)
fn read_dword(sub_key: &str, value: &str) -> Result<Option<u32>, WindowsError> {
    let key = match winsafe::HKEY::LOCAL_MACHINE.RegOpenKeyEx(
        Some(sub_key),
        co::REG_OPTION::default(),
        co::KEY::READ,
    ) {
        Ok(key) => key,
        Err(err) if err == co::ERROR::FILE_NOT_FOUND => return Ok(None),
        Err(err) => return Err(win_err("RegOpenKeyEx", err)),
    };
    match key.RegQueryValueEx(Some(value)) {
        Ok(winsafe::RegistryValue::Dword(found)) => Ok(Some(found)),
        // Present but not a DWORD: treat as "no usable value".
        Ok(_) => Ok(None),
        Err(err) if err == co::ERROR::FILE_NOT_FOUND => Ok(None),
        Err(err) => Err(win_err("RegQueryValueEx", err)),
    }
}
