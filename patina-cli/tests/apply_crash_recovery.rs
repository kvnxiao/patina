//! Integration tests for apply crash recovery.

#![cfg(test)]

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;
use common::wait_for_next_second;
use patina_core::journal::COMMIT_SUFFIX;
use patina_core::journal::PLAN_SUFFIX;
use patina_core::recover_orphans;

fn setup(pre_existing: &str, original: &str) -> Fixture {
    let fx = Fixture::new();
    let module = fx.module(
        "shell",
        "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n\n\
         [[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("a"), "NEW-A\n").expect("write source a");
    fs_err::write(module.join("b"), "NEW-B\n").expect("write source b");
    fs_err::write(fx.home.join(pre_existing), original).expect("seed pre-existing target");
    fx
}

const ONLY_A: &str = "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n";

/// Apply both entries of a fresh fixture, then drop `~/.b`'s entry, so the next
/// apply plans a `Remove` for `~/.b` after an `Unchanged` `~/.a`.
fn applied_then_b_dropped() -> Fixture {
    let fx = Fixture::new();
    let module = fx.module(
        "shell",
        "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n\n\
         [[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("a"), "NEW-A\n").expect("write source a");
    fs_err::write(module.join("b"), "NEW-B\n").expect("write source b");
    let first = fx.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "the first apply must commit; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    wait_for_next_second();
    fx.module("shell", ONLY_A);
    fx
}

fn plan_files(journal: &Utf8Path) -> Vec<camino::Utf8PathBuf> {
    fs_err::read_dir(journal)
        .expect("read journal dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(PLAN_SUFFIX))
        .map(|entry| camino::Utf8PathBuf::from_path_buf(entry.path()).expect("utf8 plan path"))
        .collect()
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
    let fx = setup(".a", "OLD-A\n");

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
fn kill_before_a_pre_existing_second_target_is_written_keeps_its_bytes_on_recovery() {
    let fx = setup(".b", "OLD-B\n");

    let out = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(
        code(&out),
        70,
        "the crash seam must terminate the apply mid-materialize (exit 70); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".b")).expect("read ~/.b before recovery"),
        "OLD-B\n",
        "the crash seam must fire before the second op writes ~/.b"
    );

    let state = fx.state_root();
    recover_orphans(state.join("journal"), state.join("backups")).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(fx.home.join(".b")).ok(),
        Some("OLD-B\n".to_owned()),
        "a pre-existing target the crashed apply never reached must keep its pre-apply bytes"
    );
}

#[test]
fn kill_after_all_ops_before_commit_converges_to_pre_apply_on_recovery() {
    let fx = setup(".a", "OLD-A\n");

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

#[test]
fn kill_after_a_reap_restores_the_reaped_target_on_recovery() {
    let fx = applied_then_b_dropped();

    let out = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(
        code(&out),
        70,
        "the crash seam must terminate the apply after the reap (exit 70); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !fx.home.join(".b").as_std_path().exists(),
        "the crash seam must fire after the reap removed ~/.b"
    );

    let state = fx.state_root();
    let journal = state.join("journal");
    let plans = plan_files(&journal);
    let [plan] = plans.as_slice() else {
        panic!("a killed apply must leave exactly one orphan plan, got {plans:?}");
    };
    let rendered = fx.run(&["debug", "journal", plan.as_str()], &[]);
    let rendered = String::from_utf8_lossy(&rendered.stdout);
    assert!(
        rendered
            .lines()
            .zip(rendered.lines().skip(1))
            .any(|(mode, target)| {
                mode.ends_with("] remove")
                    && target
                        .strip_prefix("    target: ")
                        .and_then(|path| Utf8Path::new(path).file_name())
                        == Some(".b")
            }),
        "the orphan plan must record the reap of ~/.b as a remove; got:\n{rendered}"
    );

    recover_orphans(&journal, state.join("backups")).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(fx.home.join(".b")).ok(),
        Some("NEW-B\n".to_owned()),
        "recovery must restore the reaped target from its backup"
    );
}
