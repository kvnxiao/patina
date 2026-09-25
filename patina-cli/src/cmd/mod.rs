//! Subcommand implementations.
//!
//! Each subcommand's control flow and presentation live here; the engine
//! semantics live in `patina_core`.

pub(crate) mod add;
pub(crate) mod apply;
pub(crate) mod debug;
#[cfg(windows)]
pub(crate) mod defender;
pub(crate) mod doctor;
pub(crate) mod init;
pub(crate) mod managed;
pub(crate) mod promote;
pub(crate) mod remote;
pub(crate) mod remove;
pub(crate) mod rollback;
pub(crate) mod status;

/// The manifest filename, at the repository root and in every module.
pub(crate) const MANIFEST_FILENAME: &str = "patina.toml";
