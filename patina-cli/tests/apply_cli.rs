//! Integration tests for the `patina apply` CLI surface (REQ-017,
//! CHK-028 / CHK-029 / CHK-030).
//!
//! Each test builds a self-contained tempdir dotfiles repository, points
//! `PATINA_REPO` at it, and isolates the per-machine state directory under
//! the tempdir so the apply never touches the developer's real `$HOME`.
//! The binary is invoked as a subprocess (its stdin is therefore not a
//! TTY, exercising the non-interactive path).

mod common;

use common::Fixture;
use common::code;
use std::process::Command;

#[test]
fn non_tty_apply_previews_without_mutating() {
    // CHK-028: a symlink [[file]] entry, `patina apply` (no --yes) in a
    // non-TTY: exit 0, no symlink created, stdout shows the diff.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("rc"), "export A=1\n").expect("write source");

    let out = f.apply(&[]);

    assert_eq!(code(&out), 0, "non-TTY preview must exit 0");
    let target = f.home.join(".rc");
    assert!(
        fs_err::symlink_metadata(&target).is_err(),
        "no symlink may be created on a preview"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(".rc"),
        "stdout must contain the rendered diff naming the target, got: {stdout}"
    );
}

#[test]
fn post_apply_hook_failure_rolls_back_and_exits_3() {
    // CHK-029: a post_apply hook `exit 1` (must_succeed = true default),
    // `patina apply --yes`: file ops execute then reverse, exit code 3.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n\n\
         [[hook]]\nevent = \"post_apply\"\ncommand = \"exit 1\"\n",
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        3,
        "a must_succeed post_apply hook failure must exit 3; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let target = f.home.join(".rc");
    assert!(
        fs_err::symlink_metadata(&target).is_err(),
        "the copied file must be reversed on rollback"
    );
}

#[test]
fn force_deploy_downgrades_hook_failure_and_exits_0() {
    // CHK-030: same hook, `patina apply --yes --force-deploy`: file ops
    // execute, hook fails but is NOT rolled back, stderr warns naming the
    // hook, exit code 0.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n\n\
         [[hook]]\nevent = \"post_apply\"\ncommand = \"exit 1\"\n",
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");

    let out = f.apply(&["--yes", "--force-deploy"]);

    assert_eq!(
        code(&out),
        0,
        "--force-deploy must downgrade the hook failure to a warning; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let target = f.home.join(".rc");
    assert_eq!(
        fs_err::read_to_string(&target).expect("the copied file must survive"),
        "payload\n",
        "file ops must NOT be rolled back under --force-deploy"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("exit 1"),
        "stderr must warn naming the failed hook, got: {stderr}"
    );
}

#[test]
fn json_without_yes_previews_and_does_not_mutate() {
    // --json without --yes: a single JSON document with result=previewed
    // and no filesystem mutation under HOME.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("rc"), "x\n").expect("write source");

    let out = f.apply(&["--json"]);

    assert_eq!(code(&out), 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be a single JSON document");
    assert_eq!(
        doc.get("result").and_then(serde_json::Value::as_str),
        Some("previewed")
    );
    assert!(
        fs_err::symlink_metadata(f.home.join(".rc")).is_err(),
        "no mutation may occur on a JSON preview"
    );
}

#[test]
fn json_with_yes_applies_and_reports_applied() {
    // --json --yes: result=applied and the symlink lands under HOME.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "applied-content\n").expect("write source");

    let out = f.apply(&["--json", "--yes"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be a single JSON document");
    assert_eq!(
        doc.get("result").and_then(serde_json::Value::as_str),
        Some("applied")
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".rc")).expect("target written"),
        "applied-content\n"
    );
}

#[test]
fn cli_variable_override_renders_into_template() {
    // -v email=... flows into a {{ email }} template render under --json.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"gitconfig.tmpl\"\ntarget = \"~/.gitconfig.tmpl\"\n",
    );
    fs_err::write(module.join("gitconfig.tmpl"), "email = {{ email }}\n").expect("write tmpl");

    let out = f.apply(&["--yes", "-v", "email=cli@example.com"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rendered = fs_err::read_to_string(f.home.join(".gitconfig")).expect("target written");
    assert!(
        rendered.contains("email = cli@example.com"),
        "rendered target must contain the CLI-overridden value, got: {rendered}"
    );
}

#[test]
fn missing_pager_falls_back_with_warning() {
    // --pager=delta on a host without delta: apply succeeds and stderr
    // carries a one-line fallback warning naming the missing tool.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "p\n").expect("write source");

    // Force a PATH with no `delta` so the fallback path is deterministic.
    let bin = env!("CARGO_BIN_EXE_patina");
    let out = Command::new(bin)
        .arg("apply")
        .args(["--pager=delta", "--yes"])
        .env("PATINA_REPO", f.root.as_str())
        .env("HOME", f.home.as_str())
        .env("USERPROFILE", f.home.as_str())
        .env("XDG_STATE_HOME", f.state.as_str())
        .env("LOCALAPPDATA", f.state.as_str())
        .env("PATH", f.state.as_str())
        .env_remove("PATINA_PROFILE")
        .output()
        .expect("spawn patina");

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("delta") && stderr.to_lowercase().contains("fall"),
        "stderr must warn about the missing pager, got: {stderr}"
    );
}
