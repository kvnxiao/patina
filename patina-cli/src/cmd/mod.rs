//! Subcommand implementations.
//!
//! Each subcommand's control flow and presentation live here; the engine
//! semantics live in `patina_core`.

pub mod add;
pub mod apply;
pub mod debug;
pub mod init;
pub mod rollback;
pub mod status;
