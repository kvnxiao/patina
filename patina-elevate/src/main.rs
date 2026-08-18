//! Binary entry point for `patina-elevate`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = patina_elevate::parse_or_exit();
    patina_elevate::run(&cli.command)
}
