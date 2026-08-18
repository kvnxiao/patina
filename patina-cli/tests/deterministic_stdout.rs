//! Integration tests for `deterministic_stdout`.
#![expect(
    clippy::expect_used,
    reason = "Integration tests use .expect() for fixture setup outside #[cfg(test)] modules; allow-expect-in-tests does not cover integration-crate roots."
)]

mod common;

use common::Fixture;
use common::code;

fn rich_fixture() -> Fixture {
    let f = Fixture::new();
    let editor = f.module(
        "editor",
        "[[file]]\nsource = \"config\"\ntarget = \"~/.editorconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(editor.join("config"), "indent = 2\n").expect("write copy source");

    let shell = f.module(
        "shell",
        "[[file]]\nsource = \"rc.tmpl\"\ntarget = \"~/.shellrc\"\n",
    );
    fs_err::write(shell.join("rc.tmpl"), "export EDITOR=vim\n").expect("write tmpl source");
    f
}

#[test]
fn json_apply_is_byte_identical_across_two_runs() {
    let f = rich_fixture();

    let prime = f.apply(&["--json", "--yes"]);
    assert_eq!(
        code(&prime),
        0,
        "priming apply must succeed; stderr: {}",
        String::from_utf8_lossy(&prime.stderr)
    );

    let first = f.apply(&["--json", "--yes"]);
    assert_eq!(
        code(&first),
        0,
        "first measured apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = f.apply(&["--json", "--yes"]);
    assert_eq!(
        code(&second),
        0,
        "second measured apply must succeed; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    assert_eq!(
        first.stdout,
        second.stdout,
        "two consecutive --json applies on an unchanged repo must produce byte-identical stdout;\nfirst:  {}\nsecond: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
    );
}

#[test]
fn human_apply_is_byte_identical_across_two_runs() {
    let f = rich_fixture();

    let prime = f.apply(&["--yes"]);
    assert_eq!(
        code(&prime),
        0,
        "priming apply must succeed; stderr: {}",
        String::from_utf8_lossy(&prime.stderr)
    );

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "first measured apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "second measured apply must succeed; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    assert_eq!(
        first.stdout,
        second.stdout,
        "two consecutive human-mode applies on an unchanged repo must produce byte-identical stdout;\nfirst:  {}\nsecond: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
    );
}

#[test]
fn fully_satisfied_applies_are_byte_identical_and_report_up_to_date() {
    let f = rich_fixture();

    assert_eq!(
        code(&f.apply(&["--yes"])),
        0,
        "priming apply must converge the repo"
    );

    let first = f.apply(&["--yes"]);
    assert_eq!(code(&first), 0, "first no-op apply must succeed");
    let second = f.apply(&["--yes"]);
    assert_eq!(code(&second), 0, "second no-op apply must succeed");

    assert_eq!(
        first.stdout, second.stdout,
        "two consecutive fully-satisfied applies must produce byte-identical stdout"
    );
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(
        stdout.contains("Already up to date"),
        "a fully-satisfied apply must print the up-to-date message, got: {stdout}"
    );
}

#[test]
fn multi_target_rows_preserve_input_declaration_order() {
    let f = Fixture::new();
    let agent = f.module(
        "agent",
        "[[file]]\nsource = \"agent.toml\"\n\
         targets = [\"~/.codex/agent.toml\", \"~/.claude/agent.toml\"]\n\
         mode = \"copy\"\n",
    );
    fs_err::write(agent.join("agent.toml"), "model = \"x\"\n").expect("write multi-target source");

    let out = f.apply(&["--json", "--yes"]);
    assert_eq!(
        code(&out),
        0,
        "multi-target apply must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be a single JSON document");
    let plan = doc
        .get("plan")
        .and_then(serde_json::Value::as_array)
        .expect("the envelope must carry a `plan` array");
    let targets: Vec<&str> = plan
        .iter()
        .filter_map(|row| row.get("target").and_then(serde_json::Value::as_str))
        .collect();

    let codex_pos = targets
        .iter()
        .position(|t| t.contains(".codex"))
        .expect("the .codex target must be present in the plan");
    let claude_pos = targets
        .iter()
        .position(|t| t.contains(".claude"))
        .expect("the .claude target must be present in the plan");
    assert!(
        codex_pos < claude_pos,
        "per-target rows must follow input declaration order (.codex before \
         .claude), not be alphabetised; got targets: {targets:?}"
    );
}
