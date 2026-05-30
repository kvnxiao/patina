//! Subcommand implementations.
//!
//! Each subcommand's control flow and presentation live here; the engine
//! semantics live in `patina_core`.

pub mod add;
pub mod apply;
pub mod debug;
pub mod init;
pub mod managed;
pub mod promote;
pub mod remove;
pub mod rollback;
pub mod status;

/// The per-module manifest filename the subcommands read and write.
pub(crate) const MANIFEST_FILENAME: &str = "patina.toml";
