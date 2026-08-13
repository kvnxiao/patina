#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

//! End-to-end crash-safety coverage. This interrupts a real `patina apply`
//! process mid-materialize, then proves the next run converges.
//!
//! Unlike `patina-core/tests/recovery_crash.rs`, which stages on-disk orphan
//! states and calls `recover_orphans` directly, this suite spawns the actual
//! binary and kills it through the debug-only `PATINA_TEST_ABORT_AFTER_OP`
//! seam. The engine calls `std::process::exit` after the k-th materialized
//! operation, before writing the COMMIT sentinel, an approximation of
//! `kill -9`. The suite then asserts the crash window (a flushed
//! `<ts>.plan`, no `<ts>.COMMIT`) and drives `recover_orphans` to confirm the
//! filesystem returns to its pre-apply state byte-for-byte. This exercises
//! the engine's own recover-before-flush wiring that the direct-staging
//! tests cannot.
//!
//! Both cases converge to pre-apply because the interrupted apply never
//! committed. Completed overwrite operations are reversed from backups, and
//! completed fresh-creation operations are deleted.

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;
use patina_core::journal::COMMIT_SUFFIX;
use patina_core::journal::PLAN_SUFFIX;
use patina_core::recover_orphans;

/// A fixture whose repo declares two copy entries in a fixed order. `~/.a`,
/// declared first, overwrites a pre-existing unmanaged file, and `~/.b`,
/// declared second, is a fresh creation. Operations materialize in
/// declaration order, so `PATINA_TEST_ABORT_AFTER_OP=1` completes the
/// overwrite and leaves the fresh create un-started.
fn setup() -> Fixture {
    let fx = Fixture::new();
    let module = fx.module(
        "shell",
        "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n\n\
         [[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("a"), "NEW-A\n").expect("write source a");
    fs_err::write(module.join("b"), "NEW-B\n").expect("write source b");
    // `~/.a` pre-exists as an unmanaged file (the overwrite op); `~/.b` does not
    // exist yet (the fresh create).
    fs_err::write(fx.home.join(".a"), "OLD-A\n").expect("seed pre-existing target");
    fx
}

/// Count journal files whose name ends with `suffix` (e.g. `.plan`, `.COMMIT`).
fn count_suffix(journal: &Utf8Path, suffix: &str) -> usize {
    fs_err::read_dir(journal)
        .expect("read journal dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(suffix))
        .count()
}

/// A `patina apply` killed after its first operation leaves a flushed plan
/// and no COMMIT. The next run's recovery reverses the completed overwrite
/// from its backup and leaves the un-started fresh create absent, restoring
/// the pre-apply state.
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

    // The plan is flushed, but the apply never committed.
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

    // The next run recovers the orphan, converging to the pre-apply state.
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

/// A `patina apply` killed after all operations but before the COMMIT is
/// still an uncommitted orphan. Recovery reverses every completed op back to
/// the pre-apply state, rather than treating the run as successful.
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

    // Both ops ran, but with no COMMIT the run is still an orphan.
    assert_eq!(
        count_suffix(&journal, COMMIT_SUFFIX),
        0,
        "an uncommitted apply must not have written a COMMIT sentinel"
    );
    // Before recovery, both targets are already materialized.
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
