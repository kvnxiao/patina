//! The auto-generated `patina help` subcommand is disabled
//! (`disable_help_subcommand`): the `--help` flag, supported on the root
//! command and every subcommand, makes it redundant.

mod common;

use common::Fixture;
use common::code;

#[test]
fn help_subcommand_is_rejected() {
    // The `help` subcommand is disabled at every level that owns subcommands
    // (root, `debug`, `watch`); clap treats `help` as an unknown argument and
    // exits 2 (usage error). `disable_help_subcommand` does not propagate in
    // derive mode, so each level is verified independently.
    let f = Fixture::new();

    for args in [["help"].as_slice(), &["debug", "help"], &["watch", "help"]] {
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
fn help_flag_still_works() {
    // The `--help` flag remains the supported way to print usage and exits 0.
    let f = Fixture::new();

    let out = f.run(&["--help"], &[]);

    assert_eq!(
        code(&out),
        0,
        "`patina --help` must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.stdout.is_empty(),
        "`patina --help` must print usage to stdout"
    );
}
