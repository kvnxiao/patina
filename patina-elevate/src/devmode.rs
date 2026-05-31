//! The `enable-developer-mode` action: flip the Developer Mode registry
//! switch to `1` (REQ-008).
//!
//! The real registry write is `#[cfg(windows)]`-gated. On any other host
//! the action returns [`DevModeError::NotWindows`] without touching the
//! registry, which keeps the binary's argument-parsing surface exercisable
//! by the cross-platform integration tests (the `enable-developer-mode`
//! arm resolves to a clean error path on Linux/macOS rather than failing
//! to compile).
//!
//! ## Duplicated constants (DEC-002)
//!
//! The registry key path and value name below are copied verbatim from
//! `patina-core::windows::registry` *on purpose*. DEC-002 forbids this
//! helper from depending on `patina-core`, so the constants cannot be
//! shared across the crate boundary; the duplication is the deliberate
//! price of the minimal trust surface. Keep the two sites in sync by hand
//! if the Developer Mode key ever moves (it is a stable Windows ABI, so
//! this is effectively never).

use std::fmt;

/// Failure modes of [`enable_developer_mode`].
#[derive(Debug)]
pub enum DevModeError {
    /// The action was invoked on a non-Windows build. The registry write
    /// only exists under `#[cfg(windows)]`; everywhere else this is the
    /// terminal outcome.
    NotWindows,

    /// A Windows registry call failed. `call` names the failing API,
    /// `symbol` names the Win32 error constant (e.g. `ERROR_ACCESS_DENIED`
    /// when the helper was launched without elevation — REQ-008's
    /// non-elevated exit-1 path), and `source` carries the OS error with
    /// its formatted message.
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
/// On Windows this opens (creating if absent)
/// `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock` and
/// writes the `AllowDevelopmentWithoutDevLicense` DWORD as `1`, the switch
/// that lets unprivileged processes create symbolic links. The helper must
/// be running elevated for `HKLM` to be writable; a non-elevated invocation
/// surfaces `ERROR_ACCESS_DENIED` through [`DevModeError::Registry`].
///
/// On any non-Windows build this returns [`DevModeError::NotWindows`]
/// without side effects.
///
/// # Errors
///
/// Returns [`DevModeError::NotWindows`] off Windows, or
/// [`DevModeError::Registry`] when opening the key or writing the value
/// fails (notably access-denied when not elevated).
#[cfg(windows)]
pub fn enable_developer_mode() -> Result<(), DevModeError> {
    use winsafe::co;

    // DEC-002: duplicated verbatim from `patina-core::windows::registry`.
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
/// `ERROR_ACCESS_DENIED` (the non-elevated case REQ-008 calls out) is named
/// symbolically; every other failure carries the numeric Win32 code so the
/// "or the specific HRESULT observed" branch of the requirement is covered.
/// The `source` keeps winsafe's `Display`, which formats the OS message.
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
