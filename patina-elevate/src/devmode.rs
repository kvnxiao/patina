//! The `enable-developer-mode` action: set the Developer Mode registry switch
//! to `1`.
//!
//! The registry write is `#[cfg(windows)]`-gated, and every other host returns
//! [`DevModeError::NotWindows`] without touching the registry, to keep the
//! dispatch exercisable by the cross-platform tests.
//!
//! ## Duplicated constants
//!
//! The registry key path and value name below are copied verbatim from
//! `patina-core::windows::registry`, which this helper must not depend on.
//! Keep the sites in sync by hand.

use std::fmt;

/// Failure modes of [`enable_developer_mode`].
#[derive(Debug)]
pub enum DevModeError {
    /// The action was invoked on a non-Windows build.
    NotWindows,

    /// A Windows registry call failed. A helper running without elevation
    /// fails here with `ERROR_ACCESS_DENIED`.
    #[cfg(windows)]
    Registry {
        /// The winsafe / Win32 function that failed.
        call: &'static str,
        /// The Win32 error constant name, e.g. `ERROR_ACCESS_DENIED`.
        symbol: &'static str,
        /// The underlying OS error (code + formatted message).
        source: std::io::Error,
    },
}

impl fmt::Display for DevModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWindows => write!(
                f,
                "enable-developer-mode is a Windows-only action; this binary was not built for Windows"
            ),
            #[cfg(windows)]
            Self::Registry {
                call,
                symbol,
                source,
            } => {
                write!(
                    f,
                    "Windows registry call `{call}` failed with {symbol}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for DevModeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotWindows => None,
            #[cfg(windows)]
            Self::Registry { source, .. } => Some(source),
        }
    }
}

/// Set the Developer Mode registry flag to `1`.
///
/// Opens `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock`,
/// creating it if absent, and writes the `AllowDevelopmentWithoutDevLicense`
/// DWORD as `1`, the switch that lets unprivileged processes create symbolic
/// links. `HKLM` is writable only under elevation.
///
/// # Errors
///
/// Returns [`DevModeError::Registry`] when opening the key or writing the
/// value fails, notably access-denied when the helper is not elevated.
#[cfg(windows)]
pub fn enable_developer_mode() -> Result<(), DevModeError> {
    use winsafe::co;

    // Duplicated verbatim from `patina-core::windows::registry`.
    const DEV_MODE_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock";
    const DEV_MODE_VALUE: &str = "AllowDevelopmentWithoutDevLicense";

    let (key, _disposition) = winsafe::HKEY::LOCAL_MACHINE
        .RegCreateKeyEx(
            DEV_MODE_KEY,
            None,
            co::REG_OPTION::NON_VOLATILE,
            co::KEY::SET_VALUE,
            None,
        )
        .map_err(|err| registry_error("RegCreateKeyEx", err))?;

    key.RegSetValueEx(Some(DEV_MODE_VALUE), winsafe::RegistryValue::Dword(1))
        .map_err(|err| registry_error("RegSetValueEx", err))
}

/// Map a failing winsafe registry call to a [`DevModeError::Registry`].
///
/// Only `ERROR_ACCESS_DENIED` is named symbolically; every other failure
/// reports the OS error's own formatted message through `source`.
#[cfg(windows)]
fn registry_error(call: &'static str, err: winsafe::co::ERROR) -> DevModeError {
    use winsafe::co;

    let symbol = if err == co::ERROR::ACCESS_DENIED {
        "ERROR_ACCESS_DENIED"
    } else {
        "the Win32 error below"
    };
    DevModeError::Registry {
        call,
        symbol,
        source: std::io::Error::other(err),
    }
}

/// Non-Windows fallback: the registry write does not exist on this target.
///
/// # Errors
///
/// Always returns [`DevModeError::NotWindows`].
#[cfg(not(windows))]
pub fn enable_developer_mode() -> Result<(), DevModeError> {
    Err(DevModeError::NotWindows)
}
