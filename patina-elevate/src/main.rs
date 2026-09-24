//! Binary entry point for the `patina-elevate` helper.
//!
//! The command surface lives in the library crate, where the cross-platform
//! tests exercise it without the binary artifact.

use std::process::ExitCode;

fn main() -> ExitCode {
    match patina_elevate::parse() {
        Ok(cli) => patina_elevate::run(&cli.command),
        Err(code) => code,
    }
}
