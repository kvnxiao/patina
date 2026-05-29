#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for deterministic `patina apply` stdout (REQ-021,
//! CHK-035).
//!
//! REQ-021 requires that two consecutive `patina apply` invocations against
//! an unchanged source repository produce byte-identical stdout — in both
//! `--json` and human modes — and that no wall-clock timestamp, PID, or
//! random ID leaks into user-facing output. The journal `<ts>` filename is
//! the only place a timestamp is permitted, and it never appears on stdout.
//!
//! Each test builds a self-contained tempdir dotfiles repository, points
//! `PATINA_REPO` at it, and isolates the per-machine state directory under
//! the tempdir so the apply never touches the developer's real `$HOME`.

mod common;

use common::Fixture;
use common::code;

/// A fixture rich enough to exercise multiple modes and a multi-target
/// entry — the kind REQ-021's behaviour leans on for a meaningful proof.
fn rich_fixture() -> Fixture {
    let f = Fixture::new();
    // A copy mode and a template mode in one module; module order is fixed
    // by discovery's alphabetical sort, so two applies see the same plan.
    let editor = f.module(
        "editor",
        "[[file]]\nsource = \"config\"\ntarget = \"~/.editorconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(editor.join("config"), "indent = 2\n").expect("write copy source");

    let shell = f.module(
        "shell",
        "[[file]]\nsource = \"rc.tmpl\"\ntarget = \"~/.shellrc.tmpl\"\n",
    );
    fs_err::write(shell.join("rc.tmpl"), "export EDITOR=vim\n").expect("write tmpl source");
    f
}

#[test]
fn json_apply_is_byte_identical_across_two_runs() {
    // CHK-035: against an unchanged repository, two consecutive
    // `--yes --json` applies emit byte-identical stdout. The repo is first
    // converged with a priming apply so the two *measured* runs both observe
    // the same on-disk state — the property REQ-021 guards is stability of
    // stdout as a function of inputs, and the inputs are identical here.
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
    // REQ-021 behaviour: against an unchanged repository, two consecutive
    // human-mode `--yes` applies emit byte-identical stdout. As above, a
    // priming apply converges the repo so the two measured runs share state.
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
fn multi_target_rows_preserve_input_declaration_order() {
    // A multi-target [[file]] whose `targets` are declared in deliberately
    // non-alphabetical order. "Deterministic" means "stable function of
    // inputs", not "alphabetised": the plan rows must appear in the
    // declared order (.codex before .claude), not sorted.
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
