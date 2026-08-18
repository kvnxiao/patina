//! The `enable-developer-mode` action: set the Developer Mode registry switch
//! to `1`.
//!
//! The registry write is `#[cfg(windows)]`-gated. Other hosts return
//! [`DevModeError::NotWindows`] without touching the registry.
//!
//! ## Duplicated constants
//!
//! The helper duplicates the registry key and value names used by the CLI.

use std::fmt;

/// Failure modes of [`enable_developer_mode`].
#[derive(Debug)]
pub enum DevModeError {
    /// The action was invoked on a non-Windows build.
    NotWindows,

    /// A Windows registry call failed, including `ERROR_ACCESS_DENIED` without
    /// elevation.
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

/// Set `AllowDevelopmentWithoutDevLicense` to `1` in `HKLM`.
///
/// Opens `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock`,
/// creating it if absent, and writes the `AllowDevelopmentWithoutDevLicense`
/// DWORD as `1`.
/// The value enables unprivileged symbolic-link creation. `HKLM` requires
/// elevation for this write.
///
/// # Errors
///
/// Returns [`DevModeError::Registry`] when opening the key or writing the value
/// fails.
#[cfg(windows)]
pub fn enable_developer_mode() -> Result<(), DevModeError> {
    use winsafe::co;

    // Keep these literals synchronized with the CLI's registry integration.
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

/// Return [`DevModeError::NotWindows`] on non-Windows targets.
///
/// # Errors
///
/// Always returns [`DevModeError::NotWindows`].
#[cfg(not(windows))]
pub fn enable_developer_mode() -> Result<(), DevModeError> {
    Err(DevModeError::NotWindows)
}
