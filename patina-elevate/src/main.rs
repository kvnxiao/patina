//! Thin binary entry point for the `patina-elevate` helper.
//!
//! All logic lives in the library crate (see `lib.rs`) so the command
//! surface stays unit-testable without the binary artifact, which is
//! absent from non-Windows release builds (DEC-003 / CHK-015). This file
//! only parses arguments — [`patina_elevate::parse_or_exit`] owns the
//! exit-2 usage path and augments the unknown-subcommand message with the
//! supported-subcommand listing (REQ-008) — and hands the parsed command
//! to [`patina_elevate::run`].

use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = patina_elevate::parse_or_exit();
    patina_elevate::run(&cli.command)
}
