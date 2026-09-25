//! Integration tests for apply crash recovery.

#![cfg(test)]

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;
use common::snapshot;
use common::stderr;
use patina_core::journal::COMMIT_SUFFIX;
use patina_core::journal::PLAN_SUFFIX;
use patina_core::recover_orphans;

const BOTH: &str = "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n\n\
                    [[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n";

const ONLY_A: &str = "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n";

/// Declare copy entries for `~/.a` and `~/.b` whose sources hold `NEW-A` and
/// `NEW-B`.
fn two_copy_entries() -> Fixture {
    let fx = Fixture::new();
    let module = fx.module("shell", BOTH);
    fs_err::write(module.join("a"), "NEW-A\n").expect("write source a");
    fs_err::write(module.join("b"), "NEW-B\n").expect("write source b");
    fx
}

fn setup(pre_existing: &str, original: &str) -> Fixture {
    let fx = two_copy_entries();
    fs_err::write(fx.home.join(pre_existing), original).expect("seed pre-existing target");
    fx
}

/// Commit an apply, then wait so the next apply gets its own `<ts>` files.
fn commit_first_apply(fx: &Fixture) {
    let first = fx.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "the first apply must commit; stderr: {}",
        stderr(&first)
    );
}

/// Apply both entries of a fresh fixture, then drop `~/.b`'s entry, so the next
/// apply plans a `Remove` for `~/.b` after an `Unchanged` `~/.a`.
fn applied_then_b_dropped() -> Fixture {
    let fx = two_copy_entries();
    commit_first_apply(&fx);
    fx.module("shell", ONLY_A);
    fx
}

/// Commit an apply over a pre-existing `~/.a`, then kill a second apply after
/// it rewrites `~/.a`, leaving an orphan plan beside the first commit.
fn committed_then_interrupted() -> Fixture {
    let fx = setup(".a", "OLD-A\n");
    commit_first_apply(&fx);
    fs_err::write(fx.root.join("shell").join("a"), "NEWER-A\n").expect("edit source a");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(
        code(&killed),
        70,
        "the crash seam must terminate the second apply (exit 70); stderr: {}",
        stderr(&killed)
    );
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read ~/.a after the kill"),
        "NEWER-A\n",
        "the killed apply must have rewritten ~/.a"
    );
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

fn kept_copy(stderr: &str) -> &str {
    stderr
        .lines()
        .find_map(|line| {
            line.split_once(" from before the recovery at ")
                .map(|(_, path)| path.trim())
        })
        .unwrap_or_else(|| panic!("the recovery must name the kept copy; stderr: {stderr}"))
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
        stderr(&out)
    );

    let state = fx.state_root();
    let journal = state.join("journal");

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

    let report = recover_orphans(&state).expect("recovery");
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
        stderr(&out)
    );
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".b")).expect("read ~/.b before recovery"),
        "OLD-B\n",
        "the crash seam must fire before the second op writes ~/.b"
    );

    let state = fx.state_root();
    recover_orphans(&state).expect("recovery");

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
        stderr(&out)
    );

    let state = fx.state_root();
    let journal = state.join("journal");

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

    recover_orphans(&state).expect("recovery");

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
        stderr(&out)
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

    recover_orphans(&state).expect("recovery");

    assert_eq!(
        fs_err::read_to_string(fx.home.join(".b")).ok(),
        Some("NEW-B\n".to_owned()),
        "recovery must restore the reaped target from its backup"
    );
}

#[test]
fn the_next_apply_after_a_crash_applies_against_the_recovered_state() {
    let fx = setup(".a", "OLD-A\n");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));

    let next = fx.apply(&["--yes"]);
    assert_eq!(code(&next), 0, "stderr: {}", stderr(&next));
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read ~/.a"),
        "NEW-A\n",
        "the target must reach the declared bytes"
    );
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".b")).expect("read ~/.b"),
        "NEW-B\n"
    );
    assert!(
        stderr(&next).contains("reverted an interrupted apply"),
        "the apply must report the recovery; stderr: {}",
        stderr(&next)
    );

    let rollback = fx.run(&["rollback", "--yes"], &[]);
    assert_eq!(code(&rollback), 0, "stderr: {}", stderr(&rollback));
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).ok(),
        Some("OLD-A\n".to_owned()),
        "the apply must have backed up the recovered original before overwriting it"
    );
    assert!(!fx.home.join(".b").as_std_path().exists());
}

#[test]
fn the_next_apply_after_a_crashed_reap_plans_the_reap_again() {
    let fx = applied_then_b_dropped();
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));
    assert!(!fx.home.join(".b").as_std_path().exists());

    let next = fx.apply(&["--yes", "--json"]);
    assert_eq!(code(&next), 0, "stderr: {}", stderr(&next));
    let document: serde_json::Value =
        serde_json::from_slice(&next.stdout).expect("stdout is one JSON document");
    let reaped: Vec<&str> = document
        .get("reaped")
        .and_then(serde_json::Value::as_array)
        .expect("a reaped array")
        .iter()
        .filter_map(|row| row.get("target").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        reaped
            .iter()
            .any(|target| Utf8Path::new(target).file_name() == Some(".b")),
        "recovery must restore ~/.b before planning, so the apply reaps it again; reaped: {reaped:?}"
    );
    assert!(
        stderr(&next).contains("reverted an interrupted apply"),
        "stderr: {}",
        stderr(&next)
    );
}

#[test]
fn rollback_with_a_pending_orphan_recovers_it_before_rolling_back() {
    let fx = committed_then_interrupted();

    let rollback = fx.run(&["rollback", "--yes"], &[]);
    assert_eq!(code(&rollback), 0, "stderr: {}", stderr(&rollback));

    assert!(
        stderr(&rollback).contains("reverted an interrupted apply"),
        "stderr: {}",
        stderr(&rollback)
    );
    assert_eq!(
        count_suffix(&fx.state_root().join("journal"), PLAN_SUFFIX),
        0,
        "no orphan plan may survive to restore its backups over the rollback"
    );
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).ok(),
        Some("OLD-A\n".to_owned()),
        "~/.a must hold its bytes from before the committed apply"
    );
    assert!(
        !fx.home.join(".b").as_std_path().exists(),
        "~/.b did not exist before the committed apply"
    );
}

#[test]
fn a_rollback_that_finds_no_prior_apply_still_reports_its_recovery() {
    let fx = setup(".a", "OLD-A\n");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));

    let rollback = fx.run(&["rollback", "--yes"], &[]);

    assert_eq!(code(&rollback), 1, "stderr: {}", stderr(&rollback));
    assert!(
        stderr(&rollback).contains("no prior apply found"),
        "stderr: {}",
        stderr(&rollback)
    );
    assert!(
        stderr(&rollback).contains("reverted an interrupted apply"),
        "the recovery must be reported even though the rollback failed; stderr: {}",
        stderr(&rollback)
    );
}

#[test]
fn remove_with_a_pending_orphan_recovers_it_before_its_own_writes() {
    let fx = committed_then_interrupted();

    let out = fx.run(&["remove", "~/.b", "--yes"], &[]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));

    assert!(
        stderr(&out).contains("reverted an interrupted apply"),
        "stderr: {}",
        stderr(&out)
    );
    assert_eq!(
        count_suffix(&fx.state_root().join("journal"), PLAN_SUFFIX),
        0,
        "the orphan plan must be recovered"
    );
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".b")).ok(),
        Some("NEW-B\n".to_owned()),
        "remove must leave ~/.b as a regular file with its last-applied bytes"
    );

    let before = fx.deployment_snapshot();
    let rollback = fx.run(&["rollback", "--yes"], &[]);
    assert_eq!(code(&rollback), 0, "stderr: {}", stderr(&rollback));
    assert_eq!(
        fx.deployment_snapshot(),
        before,
        "rolling back the commit that remove wrote must not change a file"
    );
}

#[test]
fn promote_with_a_pending_orphan_recovers_it_before_its_own_writes() {
    let fx = committed_then_interrupted();
    fs_err::write(fx.home.join(".b"), "EDITED-B\n").expect("edit ~/.b outside patina");

    let out = fx.run(&["promote", "~/.b", "--yes"], &[]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));

    assert!(
        stderr(&out).contains("reverted an interrupted apply"),
        "stderr: {}",
        stderr(&out)
    );
    assert_eq!(
        count_suffix(&fx.state_root().join("journal"), PLAN_SUFFIX),
        0,
        "the orphan plan must be recovered"
    );
    assert_eq!(
        fs_err::read_to_string(fx.root.join("shell").join("b")).ok(),
        Some("EDITED-B\n".to_owned()),
        "promote must copy the edited bytes into the source"
    );
}

#[test]
fn promote_refuses_a_case_respelled_target_its_recovery_reverted() {
    let fx = two_copy_entries();
    commit_first_apply(&fx);
    fx.module("shell", &BOTH.replace("~/.a", "~/.A"));
    fs_err::write(fx.root.join("shell").join("a"), "NEWER-A\n").expect("edit source a");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));

    let out = fx.run(&["promote", "~/.a", "--yes"], &[]);

    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));
    assert_eq!(
        fs_err::read_to_string(fx.root.join("shell").join("a")).expect("read source a"),
        "NEWER-A\n",
        "a refused promotion must not write the repository source"
    );
}

#[test]
fn a_json_promote_refusal_names_the_kept_copy_only_on_stderr() {
    let fx = committed_then_interrupted();

    let out = fx.run(&["promote", "~/.a", "--yes", "--json"], &[]);

    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));
    let err = stderr(&out);
    let kept = kept_copy(&err);
    let document: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON document");
    let message = document
        .get("message")
        .and_then(serde_json::Value::as_str)
        .expect("the refusal carries a message");
    assert!(
        !message.contains(kept),
        "the stdout message must not carry the timestamped copy path: {message}"
    );
}

#[test]
fn promote_copies_a_drifted_target_the_interrupted_apply_never_wrote() {
    let fx = two_copy_entries();
    commit_first_apply(&fx);
    fs_err::write(fx.root.join("shell").join("a"), "NEWER-A\n").expect("edit source a");
    fs_err::write(fx.home.join(".b"), "EDITED-B\n").expect("edit ~/.b outside patina");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));

    let out = fx.run(&["promote", "~/.b", "--yes"], &[]);

    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(
        fs_err::read_to_string(fx.root.join("shell").join("b")).expect("read source b"),
        "EDITED-B\n",
        "promote must copy the drifted bytes the interrupted apply never replaced"
    );
    let b = fx.home.join(".b");
    assert!(
        !stderr(&out).contains(&format!("kept a copy of {b} ")),
        "recovery must not report a target it left as it was; stderr: {}",
        stderr(&out)
    );
}

#[test]
fn plan_only_apply_with_a_pending_orphan_warns_and_writes_nothing() {
    let fx = setup(".a", "OLD-A\n");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));

    for args in [&[][..], &["--json"][..]] {
        let before = snapshot(&[&fx.home, &fx.state]);
        let out = fx.apply(args);
        assert_eq!(
            code(&out),
            0,
            "a preview exits 0 for {args:?}; stderr: {}",
            stderr(&out)
        );
        assert_eq!(
            snapshot(&[&fx.home, &fx.state]),
            before,
            "a preview must not write under the home or state directory for {args:?}"
        );
        assert!(
            stderr(&out).contains("an interrupted apply is pending"),
            "a preview must warn for {args:?}; stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn status_with_a_pending_orphan_warns_and_writes_nothing() {
    let fx = setup(".a", "OLD-A\n");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));

    for args in [&["status"][..], &["status", "--json"][..]] {
        let before = snapshot(&[&fx.home, &fx.state]);
        let out = fx.run(args, &[]);
        assert_eq!(code(&out), 0, "{args:?} exits 0; stderr: {}", stderr(&out));
        assert_eq!(
            snapshot(&[&fx.home, &fx.state]),
            before,
            "{args:?} must not write under the home or state directory"
        );
        assert!(
            stderr(&out).contains("an interrupted apply is pending"),
            "{args:?} must warn; stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn a_preview_over_a_crash_that_left_every_target_unchanged_exits_zero_and_writes_nothing() {
    let fx = committed_then_interrupted();
    let before = snapshot(&[&fx.home, &fx.state]);

    let out = fx.apply(&[]);

    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(
        snapshot(&[&fx.home, &fx.state]),
        before,
        "a preview must not write under the home or state directory"
    );
    assert!(
        stderr(&out).contains("an interrupted apply is pending"),
        "stderr: {}",
        stderr(&out)
    );

    for plan in plan_files(&fx.state_root().join("journal")) {
        fs_err::remove_file(&plan).expect("drop the orphan plan");
    }
    let settled = fx.apply(&[]);
    assert_eq!(code(&settled), 0, "stderr: {}", stderr(&settled));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&settled.stdout),
        "a preview over a pending apply prints what the same plan prints without one"
    );
}

#[test]
fn a_preview_and_status_while_the_lock_is_held_say_the_apply_may_still_be_running() {
    let fx = setup(".a", "OLD-A\n");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));
    let _held = patina_core::acquire_lock(
        &fx.state_root().join("lock"),
        patina_core::LockKind::Exclusive,
        std::time::Duration::from_secs(5),
    )
    .expect("hold the exclusive lock as a running apply would");

    for args in [&["apply"][..], &["status"][..]] {
        let out = fx.run(args, &[]);
        assert_eq!(code(&out), 0, "{args:?} exits 0; stderr: {}", stderr(&out));
        assert!(
            stderr(&out).contains("another apply is running or was interrupted"),
            "{args:?} must not claim the apply was interrupted; stderr: {}",
            stderr(&out)
        );
        assert!(
            !stderr(&out).contains("an interrupted apply is pending"),
            "{args:?} stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn a_post_apply_rollback_leaves_no_orphan_plan_and_status_no_pending_warning() {
    let fx = Fixture::new();
    let module = fx.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n\n\
         [[hook]]\nevent = \"post_apply\"\ncommand = \"exit 1\"\n",
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");

    let out = fx.apply(&["--yes"]);
    assert_eq!(code(&out), 3, "stderr: {}", stderr(&out));

    assert_eq!(
        count_suffix(&fx.state_root().join("journal"), PLAN_SUFFIX),
        0,
        "a rollback that reverted every operation must not leave an orphan plan"
    );
    let status = fx.run(&["status"], &[]);
    assert_eq!(code(&status), 0, "stderr: {}", stderr(&status));
    assert!(
        !stderr(&status).contains("apply is pending")
            && !stderr(&status).contains("running or was interrupted"),
        "status must not report a pending apply; stderr: {}",
        stderr(&status)
    );
}

#[test]
fn the_next_apply_keeps_bytes_written_after_the_crash_and_names_their_copy() {
    let fx = setup(".a", "OLD-A\n");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));
    fs_err::write(fx.home.join(".a"), "USER-A\n").expect("edit ~/.a after the crash");

    let next = fx.apply(&["--yes"]);
    assert_eq!(code(&next), 0, "stderr: {}", stderr(&next));

    let err = stderr(&next);
    assert_eq!(
        fs_err::read_to_string(kept_copy(&err)).expect("read the kept copy"),
        "USER-A\n",
        "the bytes written after the crash must survive the recovery"
    );
}

#[test]
fn a_refused_remove_or_promote_with_a_pending_orphan_warns_and_writes_nothing() {
    let fx = committed_then_interrupted();

    for (args, refusal) in [
        (&["remove", "~/.b"][..], 5),
        (&["promote", "~/.unmanaged"][..], 1),
    ] {
        let before = snapshot(&[&fx.home, &fx.state, &fx.root]);
        let out = fx.run(args, &[]);
        assert_eq!(code(&out), refusal, "{args:?} stderr: {}", stderr(&out));
        assert_eq!(
            snapshot(&[&fx.home, &fx.state, &fx.root]),
            before,
            "{args:?} must not write under the home, state, or repository directory"
        );
        assert!(
            stderr(&out).contains("an interrupted apply is pending"),
            "{args:?} stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn promote_refuses_a_target_its_recovery_reverted_and_leaves_the_source() {
    let fx = committed_then_interrupted();

    let out = fx.run(&["promote", "~/.a", "--yes"], &[]);

    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));
    assert_eq!(
        fs_err::read_to_string(fx.root.join("shell").join("a")).expect("read source a"),
        "NEWER-A\n",
        "a refused promotion must not write the repository source"
    );
    let err = stderr(&out);
    assert!(
        err.contains("re-run `patina promote`"),
        "the refusal must tell the user to review and re-run; stderr: {err}"
    );
    assert_eq!(
        fs_err::read_to_string(kept_copy(&err)).expect("read the kept copy"),
        "NEWER-A\n",
        "the kept copy holds the target as it was before the recovery"
    );
}

const NESTED: &str = "[[file]]\nsource = \"extra.lua\"\ntarget = \"~/.config/nvim/lua/extra.lua\"\nmode = \"copy\"\n\n\
                      [[directory]]\nsource = \"tree\"\ntarget = \"~/.config/nvim\"\nmode = \"copy\"\n";

/// Declare a copy `[[directory]]` over a live `~/.config/nvim` holding
/// `init.lua` and a user file, and a `[[file]]` whose target lies inside it,
/// seeded with `nested_original` or left absent.
fn nested(nested_original: Option<&str>) -> Fixture {
    let fx = Fixture::new();
    let module = fx.module("nvim", NESTED);
    fs_err::create_dir_all(module.join("tree")).expect("mkdir the tree source");
    fs_err::write(module.join("tree").join("init.lua"), "NEW-INIT").expect("write init source");
    fs_err::write(module.join("extra.lua"), "NEW-EXTRA").expect("write extra source");
    let live = nvim(&fx);
    fs_err::create_dir_all(live.join("lua")).expect("mkdir the live tree");
    fs_err::write(live.join("init.lua"), "OLD-INIT").expect("seed init.lua");
    fs_err::write(live.join("user.txt"), "USER").expect("seed a user file");
    if let Some(original) = nested_original {
        fs_err::write(live.join("lua").join("extra.lua"), original).expect("seed extra.lua");
    }
    fx
}

fn nvim(fx: &Fixture) -> camino::Utf8PathBuf {
    fx.home.join(".config").join("nvim")
}

fn read(path: &Utf8Path) -> Option<String> {
    fs_err::read_to_string(path).ok()
}

#[test]
fn a_kill_after_the_nested_file_is_written_keeps_every_file_of_the_directory() {
    let fx = nested(Some("OLD-EXTRA"));
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));

    recover_orphans(fx.state_root()).expect("recovery");

    let live = nvim(&fx);
    assert_eq!(read(&live.join("user.txt")).as_deref(), Some("USER"));
    assert_eq!(read(&live.join("init.lua")).as_deref(), Some("OLD-INIT"));
    assert_eq!(
        read(&live.join("lua").join("extra.lua")).as_deref(),
        Some("OLD-EXTRA")
    );
}

#[test]
fn rollback_restores_the_original_bytes_of_a_file_nested_in_a_copied_directory() {
    let fx = nested(Some("OLD-EXTRA"));
    let applied = fx.apply(&["--yes"]);
    assert_eq!(code(&applied), 0, "stderr: {}", stderr(&applied));

    let rollback = fx.run(&["rollback", "--yes"], &[]);
    assert_eq!(code(&rollback), 0, "stderr: {}", stderr(&rollback));

    let live = nvim(&fx);
    assert_eq!(
        read(&live.join("lua").join("extra.lua")).as_deref(),
        Some("OLD-EXTRA")
    );
    assert_eq!(read(&live.join("init.lua")).as_deref(), Some("OLD-INIT"));
    assert_eq!(read(&live.join("user.txt")).as_deref(), Some("USER"));
}

#[test]
fn rollback_deletes_a_created_file_nested_in_a_copied_directory() {
    let fx = nested(None);
    let applied = fx.apply(&["--yes"]);
    assert_eq!(code(&applied), 0, "stderr: {}", stderr(&applied));

    let rollback = fx.run(&["rollback", "--yes"], &[]);
    assert_eq!(code(&rollback), 0, "stderr: {}", stderr(&rollback));

    let live = nvim(&fx);
    assert_eq!(read(&live.join("lua").join("extra.lua")), None);
    assert_eq!(read(&live.join("init.lua")).as_deref(), Some("OLD-INIT"));
}

#[test]
fn recovery_deletes_a_created_file_nested_in_a_copied_directory() {
    let fx = nested(None);
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "2")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));

    recover_orphans(fx.state_root()).expect("recovery");

    let live = nvim(&fx);
    assert_eq!(read(&live.join("lua").join("extra.lua")), None);
    assert_eq!(read(&live.join("init.lua")).as_deref(), Some("OLD-INIT"));
    assert_eq!(read(&live.join("user.txt")).as_deref(), Some("USER"));
}

#[test]
fn a_reaped_leaf_inside_a_directory_backed_up_in_the_same_apply_is_restored_to_its_pre_apply_bytes()
{
    let fx = Fixture::new();
    let tree = "[[directory]]\nsource = \"app\"\ntarget = \"~/app\"\nmode = \"copy\"\n";
    let module = fx.module("app", tree);
    fs_err::create_dir_all(module.join("app")).expect("mkdir the tree source");
    fs_err::write(module.join("app").join("a.conf"), "A-ORIG").expect("write a.conf source");
    fs_err::write(module.join("app").join("b.conf"), "B-ORIG").expect("write b.conf source");
    commit_first_apply(&fx);

    fs_err::write(module.join("app").join("a.conf"), "A-NEW").expect("edit a.conf source");
    fs_err::remove_file(module.join("app").join("b.conf")).expect("drop the b.conf source");
    let rewrite = if cfg!(windows) {
        "Set-Content -NoNewline -Path (Join-Path $env:USERPROFILE app/b.conf) -Value HOOK"
    } else {
        "printf HOOK > \"$HOME/app/b.conf\""
    };
    fx.module(
        "app",
        &format!("{tree}\n[[hook]]\nevent = \"post_apply\"\ncommand = '{rewrite}'\n"),
    );
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "2")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));
    assert_eq!(
        read(&fx.home.join("app").join("b.conf")),
        None,
        "the reap ran"
    );

    recover_orphans(fx.state_root()).expect("recovery");

    assert_eq!(
        read(&fx.home.join("app").join("b.conf")).as_deref(),
        Some("B-ORIG"),
        "the directory's backup must hold the leaf as it was before the apply"
    );
    let partials: Vec<String> = fs_err::read_dir(fx.home.join("app"))
        .expect("read the restored tree")
        .map(|entry| {
            entry
                .expect("read a restored entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.contains(".partial."))
        .collect();
    assert!(partials.is_empty(), "the restored tree holds {partials:?}");
}

const APP: &str = "[[directory]]\nsource = \"app\"\ntarget = \"~/app\"\nmode = \"copy\"\n";

/// Commit a copy `~/app` holding `a.conf` and `z.conf`, replace `~/app` outside
/// Patina with a directory link to `~/elsewhere`, which holds its own `z.conf`,
/// and drop `z.conf` from the source. Return the fixture and that outside file.
fn app_replaced_by_a_link(link: &str) -> (Fixture, camino::Utf8PathBuf) {
    let fx = Fixture::new();
    let module = fx.module("app", APP);
    fs_err::create_dir_all(module.join("app")).expect("mkdir the tree source");
    fs_err::write(module.join("app").join("a.conf"), "A").expect("write a.conf source");
    fs_err::write(module.join("app").join("z.conf"), "Z").expect("write z.conf source");
    commit_first_apply(&fx);

    let outside = fx.home.join("elsewhere");
    fs_err::create_dir_all(&outside).expect("mkdir the link destination");
    let outside_z = outside.join("z.conf");
    fs_err::write(&outside_z, "OUTSIDE-Z").expect("write the outside file");
    fs_err::hard_link(&outside_z, outside.join("z.hard")).expect("hard-link the outside file");
    fs_err::remove_dir_all(fx.home.join("app")).expect("remove the applied tree");
    let destination = if link == "absolute" {
        outside.clone()
    } else {
        camino::Utf8PathBuf::from("elsewhere")
    };
    common::symlink_dir(&destination, &fx.home.join("app"));
    fs_err::remove_file(module.join("app").join("z.conf")).expect("drop the z.conf source");
    (fx, outside_z)
}

/// Assert that `outside` still holds its bytes and is the file its `z.hard`
/// hard link names, so nothing deleted and recreated it.
fn assert_untouched(outside: &Utf8Path) {
    assert_eq!(read(outside).as_deref(), Some("OUTSIDE-Z"));
    let mut file = fs_err::OpenOptions::new()
        .append(true)
        .open(outside)
        .expect("open the outside file");
    std::io::Write::write_all(&mut file, b"+").expect("append a marker");
    drop(file);
    assert_eq!(
        read(&outside.with_file_name("z.hard")).as_deref(),
        Some("OUTSIDE-Z+"),
        "the outside file must be the same file as before the apply"
    );
}

#[test]
fn an_apply_over_a_root_replaced_by_a_link_leaves_the_link_destination_alone() {
    for link in ["absolute", "relative"] {
        let (fx, outside_z) = app_replaced_by_a_link(link);
        let preview = fx.apply(&["--json"]);
        let document: serde_json::Value =
            serde_json::from_slice(&preview.stdout).expect("the preview is one JSON document");
        assert_eq!(
            document.get("reaped"),
            Some(&serde_json::json!([])),
            "{link} link: nothing behind the link is planned for removal"
        );

        let out = fx.apply(&["--yes"]);

        assert_eq!(code(&out), 0, "{link} link; stderr: {}", stderr(&out));
        assert_untouched(&outside_z);
        assert_eq!(
            read(&fx.home.join("app").join("a.conf")).as_deref(),
            Some("A")
        );
        assert!(
            plan_files(&fx.state_root().join("journal")).is_empty(),
            "{link} link: the apply must leave no orphan plan"
        );
    }
}

#[test]
fn recovery_of_an_apply_over_a_root_replaced_by_a_link_leaves_the_link_destination_alone() {
    let (fx, outside_z) = app_replaced_by_a_link("absolute");
    let killed = fx.apply_with_env(&["--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&killed), 70, "stderr: {}", stderr(&killed));

    let next = fx.apply(&["--yes"]);

    assert_eq!(code(&next), 0, "stderr: {}", stderr(&next));
    assert!(
        stderr(&next).contains("reverted an interrupted apply"),
        "stderr: {}",
        stderr(&next)
    );
    assert_untouched(&outside_z);
}
