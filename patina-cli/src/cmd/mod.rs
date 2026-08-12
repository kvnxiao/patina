//! Subcommand implementations.
//!
//! Each subcommand's control flow and presentation live here; the engine
//! semantics live in `patina_core`.

pub mod add;
pub mod apply;
pub mod debug;
#[cfg(windows)]
pub mod defender;
pub mod doctor;
pub mod init;
pub mod managed;
pub mod promote;
pub mod remote;
pub mod remove;
pub mod rollback;
pub mod status;
pub mod watch;

/// The per-module manifest filename the subcommands read and write.
pub(crate) const MANIFEST_FILENAME: &str = "patina.toml";

/// Acquire the shared lock, warning and proceeding on failure: the read-only
/// escape hatch every non-mutating subcommand uses.
///
/// `quiet` suppresses the warning for surfaces that must stay silent on
/// stderr (the `remote check --hook` prompt path): a timeout there means an
/// apply is holding the exclusive lock, and every new shell would otherwise
/// print the warning until it finishes.
pub(crate) fn shared_lock(
    lock_path: &camino::Utf8Path,
    quiet: bool,
    reporter: &mut impl crate::output::reporter::Reporter,
) -> Option<patina_core::LockGuard> {
    match patina_core::acquire_lock(
        lock_path,
        patina_core::LockKind::Shared,
        patina_core::SHARED_TIMEOUT,
    ) {
        Ok(guard) => Some(guard),
        Err(error) => {
            if !quiet {
                reporter.warn(&format!("proceeding without the shared lock: {error}"));
            }
            None
        }
    }
}
