//! Binary entry point for the `patina-elevate` helper.
//!
//! The command surface lives in the library crate, where the cross-platform
//! tests exercise it without the binary artifact.

use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = patina_elevate::parse_or_exit();
    patina_elevate::run(&cli.command)
}
