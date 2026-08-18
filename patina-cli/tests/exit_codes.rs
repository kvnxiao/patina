//! Integration tests for exit codes.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

mod common;

use common::Fixture;
use common::code;
use patina_core::LockKind;
use std::time::Duration;

fn hook_module(f: &Fixture, event: &str, command: &str) {
    let module = f.module(
        "shell",
        &format!(
            "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n\n\
             [[hook]]\nevent = \"{event}\"\ncommand = \"{command}\"\n"
        ),
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");
}

#[test]
fn pre_apply_hook_failure_exits_2() {
    let f = Fixture::new();
    hook_module(&f, "pre_apply", "false");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        2,
        "a must_succeed pre_apply hook failure must exit 2; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs_err::symlink_metadata(f.home.join(".rc")).is_err(),
        "no file operation may run when a pre_apply hook aborts the apply"
    );
}

#[test]
fn post_apply_hook_failure_exits_3() {
    let f = Fixture::new();
    hook_module(&f, "post_apply", "exit 1");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        3,
        "a must_succeed post_apply hook failure must exit 3; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs_err::symlink_metadata(f.home.join(".rc")).is_err(),
        "the copied file must be reversed on the post_apply rollback"
    );
}

#[test]
fn toml_syntax_error_exits_1_and_includes_the_failure() {
    let f = Fixture::new();
    f.module("broken", "[[file]]\nsource =\n");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        1,
        "a TOML syntax error must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.stderr.is_empty(),
        "stderr must carry the parse-failure message"
    );
}

#[test]
fn exclusive_lock_timeout_exits_4() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");

    let lock_path = f.state_root().join("lock");
    let _held = patina_core::acquire_lock(&lock_path, LockKind::Exclusive, Duration::from_secs(5))
        .expect("hold the exclusive lock for the duration of the subprocess apply");

    let out = f.apply_with_env(&["--yes"], &[("PATINA_LOCK_TIMEOUT_MS", "200")]);

    assert_eq!(
        code(&out),
        4,
        "an exclusive-lock timeout must exit 4; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn successful_apply_exits_0() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "applied\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "a successful apply must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".rc")).expect("target written"),
        "applied\n"
    );
}
