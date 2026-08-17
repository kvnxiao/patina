//! The `--help` flag works on the root command and on every subcommand, so the
//! auto-generated `patina help` subcommand is redundant.
//! `disable_help_subcommand` turns it off.

mod common;

use common::Fixture;
use common::code;

#[test]
fn help_subcommand_is_rejected() {
    // `disable_help_subcommand` does not propagate in derive mode, so every
    // level that owns subcommands sets it and every level is checked here.
    // clap then treats `help` as an unknown argument and exits 2 (usage error).
    // `defender` sets it too and is absent from this loop: it is
    // `#[cfg(windows)]`, and CI is not Windows.
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

/// `patina --help` prints the usage line and the subcommand list, so the flag
/// covers what the disabled `help` subcommand would have printed.
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
    // Named subcommands rather than a non-emptiness check: a renderer that
    // printed the usage line alone would still be non-empty. `defender` is
    // omitted because it is Windows-only, and `debug` because it is hidden
    // from the summary.
    for subcommand in ["init", "apply", "status", "remote", "watch"] {
        assert!(
            stdout.contains(subcommand),
            "`patina --help` must list the {subcommand} subcommand: {stdout}"
        );
    }
}
