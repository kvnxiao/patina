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

/// The manifest filename, at the repository root and in every module.
pub(crate) const MANIFEST_FILENAME: &str = "patina.toml";

/// Acquire the shared lock. On failure, warn and proceed: the read-only escape
/// hatch every non-mutating subcommand uses.
///
/// `quiet` suppresses the warning for the one surface that must leave stderr
/// clean, the `remote check --hook` prompt path. The prompt path times out
/// while an apply holds the exclusive lock, so without `quiet` every new shell
/// would print the warning until the apply finishes.
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
