//! Integration tests for help subcommand.

mod common;

use common::Fixture;
use common::code;

#[test]
fn help_subcommand_is_rejected() {
    let f = Fixture::new();

    for args in [
        ["help"].as_slice(),
        &["debug", "help"],
        &["watch", "help"],
        &["remote", "help"],
    ] {
        let out = f.run(args, &[]);

        assert_eq!(
            code(&out),
            2,
            "`patina {}` must be rejected as an unknown subcommand; stderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn help_flag_prints_usage_and_the_subcommand_list() {
    let f = Fixture::new();

    let out = f.run(&["--help"], &[]);

    assert_eq!(
        code(&out),
        0,
        "`patina --help` must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Usage:"),
        "`patina --help` must print a usage line: {stdout}"
    );
    for subcommand in ["init", "apply", "status", "remote", "watch"] {
        assert!(
            stdout.contains(subcommand),
            "`patina --help` must list the {subcommand} subcommand: {stdout}"
        );
    }
}
