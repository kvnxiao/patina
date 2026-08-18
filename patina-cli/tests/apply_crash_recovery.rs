//! Integration tests for apply crash recovery.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;
use patina_core::journal::COMMIT_SUFFIX;
use patina_core::journal::PLAN_SUFFIX;
use patina_core::recover_orphans;

fn setup() -> Fixture {
    let fx = Fixture::new();
    let module = fx.module(
        "shell",
        "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n\n\
         [[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("a"), "NEW-A\n").expect("write source a");
    fs_err::write(module.join("b"), "NEW-B\n").expect("write source b");
    fs_err::write(fx.home.join(".a"), "OLD-A\n").expect("seed pre-existing target");
    fx
}

fn count_suffix(journal: &Utf8Path, suffix: &str) -> usize {
    fs_err::read_dir(journal)
        .expect("read journal dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(suffix))
        .count()
}

#[test]
fn kill_after_first_op_converges_to_pre_apply_on_recovery() {
    let fx = setup();

    let out = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(
        code(&out),
        70,
        "the crash seam must terminate the apply mid-materialize (exit 70); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let state = fx.state_root();
    let journal = state.join("journal");
    let backups = state.join("backups");

    assert_eq!(
        count_suffix(&journal, PLAN_SUFFIX),
        1,
        "a killed apply must leave exactly one orphan plan"
    );
    assert_eq!(
        count_suffix(&journal, COMMIT_SUFFIX),
        0,
        "a killed apply must not have written a COMMIT sentinel"
    );

    let report = recover_orphans(&journal, &backups).expect("recovery");
    assert!(report.recovered_any(), "the orphan plan must be recovered");

    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read restored ~/.a"),
        "OLD-A\n",
        "the overwritten pre-existing target must be restored to its pre-apply bytes"
    );
    assert!(
        !fx.home.join(".b").as_std_path().exists(),
        "the not-yet-started fresh create must remain absent after recovery"
    );
    assert_eq!(
        count_suffix(&journal, PLAN_SUFFIX),
        0,
        "recovery clears the orphan plan"
    );
}

#[test]
fn kill_after_all_ops_before_commit_converges_to_pre_apply_on_recovery() {
    let fx = setup();

    let out = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "2")]);
    assert_eq!(
        code(&out),
        70,
        "the crash seam must terminate the apply after all ops (exit 70); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let state = fx.state_root();
    let journal = state.join("journal");
    let backups = state.join("backups");

    assert_eq!(
        count_suffix(&journal, COMMIT_SUFFIX),
        0,
        "an uncommitted apply must not have written a COMMIT sentinel"
    );
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read ~/.a before recovery"),
        "NEW-A\n"
    );
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".b")).expect("read ~/.b before recovery"),
        "NEW-B\n"
    );

    recover_orphans(&journal, &backups).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read restored ~/.a"),
        "OLD-A\n",
        "the overwritten target is restored to its pre-apply bytes"
    );
    assert!(
        !fx.home.join(".b").as_std_path().exists(),
        "the fresh create is removed, converging to the pre-apply (absent) state"
    );
}
