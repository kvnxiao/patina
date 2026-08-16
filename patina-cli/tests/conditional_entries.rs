//! A `when` predicate on a managed entry gates its presence in the plan,
//! evaluated before the source is canonicalized.
//!
//! Each test drives `PATINA_REPO=<tempdir> patina apply --yes` over a
//! fixture repo. Its module declares a `[[file]]` entry carrying a `when`
//! predicate. A false predicate drops the entry from the plan entirely, with
//! no operation and no target. A true predicate plans it like an un-gated
//! entry, byte-identical second-run parity included.

mod common;

use common::Fixture;
use common::code;

/// The OS family string the engine's `patina.os` built-in resolves to on
/// this host (`"macos"`, `"linux"`, or `"windows"`). `std::env::consts::OS`
/// matches the value `normalized_os` returns on the three supported
/// platforms, so a `when` built from it is deterministically true here.
fn current_os_family() -> &'static str {
    std::env::consts::OS
}

#[test]
fn when_false_entry_creates_no_target_and_plans_zero_operations() {
    // An entry whose `when` is `patina.os == 'definitely-not-this-os'` does
    // not contribute an operation. Its target is not created, and the `--json`
    // plan records zero operations for it.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\n\
         when = \"patina.os == 'definitely-not-this-os'\"\n",
    );
    fs_err::write(module.join("zshrc"), "export EDITOR=vim\n").expect("write source");

    let out = f.apply(&["--json", "--yes"]);

    assert_eq!(
        code(&out),
        0,
        "a `when`-false entry must not fail the apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !f.home.join(".zshrc").exists(),
        "a `when`-false entry must not materialize its target"
    );

    // The plan array holds one row per planned operation. A `when`-false entry
    // does not add a row, so `.zshrc` is absent from the array.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be a single JSON document");
    let plan = doc
        .get("plan")
        .and_then(serde_json::Value::as_array)
        .expect("the envelope must carry a `plan` array");
    assert!(
        !plan.iter().any(|row| row
            .get("target")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|t| t.contains(".zshrc"))),
        "a `when`-false entry must record zero operations in the plan, got: {plan:?}"
    );
}

#[test]
fn when_true_entry_materializes_and_second_run_is_byte_identical() {
    // An entry whose `when` equals `patina.os == '<current OS>'` materializes
    // its target. Two consecutive applies over the unchanged source produce
    // byte-identical stdout, so `when`-parity holds when a `when` is present.
    // As in `deterministic_stdout.rs`, a priming apply converges the repo
    // first, so the two measured runs observe the same on-disk state.
    //
    // Copy mode isolates the property under test: a symlink's plan diff
    // renders its link target differently on a fresh run versus a converged
    // run, an orthogonal quirk `deterministic_stdout.rs` sidesteps the same
    // way.
    let f = Fixture::new();
    let when = format!("patina.os == '{}'", current_os_family());
    let module = f.module(
        "shell",
        &format!(
            "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"copy\"\nwhen = \"{when}\"\n"
        ),
    );
    fs_err::write(module.join("zshrc"), "export EDITOR=vim\n").expect("write source");

    let prime = f.apply(&["--yes"]);
    assert_eq!(
        code(&prime),
        0,
        "a `when`-true entry must apply; stderr: {}",
        String::from_utf8_lossy(&prime.stderr)
    );
    assert!(
        f.home.join(".zshrc").exists(),
        "a `when`-true entry must materialize its target"
    );

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "the first measured apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "the second apply must succeed; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    assert_eq!(
        first.stdout,
        second.stdout,
        "two consecutive applies with a `when`-gated entry on unchanged source must produce byte-identical stdout;\nfirst:  {}\nsecond: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
    );
}

#[test]
fn multi_target_false_when_plans_none_of_its_targets() {
    // For a multi-target entry, the `when` gates all targets together. A false
    // predicate leaves every one of them unplanned.
    let f = Fixture::new();
    let module = f.module(
        "agent",
        "[[file]]\nsource = \"agent.toml\"\n\
         targets = [\"~/.codex/agent.toml\", \"~/.claude/agent.toml\"]\n\
         mode = \"copy\"\n\
         when = \"patina.os == 'definitely-not-this-os'\"\n",
    );
    fs_err::write(module.join("agent.toml"), "model = \"x\"\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "a multi-target `when`-false entry must not fail the apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !f.home.join(".codex/agent.toml").exists(),
        "no target of a `when`-false multi-target entry may be created"
    );
    assert!(
        !f.home.join(".claude/agent.toml").exists(),
        "no target of a `when`-false multi-target entry may be created"
    );
}

#[test]
fn multi_target_true_when_plans_all_of_its_targets() {
    // With a true predicate every target of a multi-target entry is planned
    // (the gate is per-entry, outside the target loop).
    let f = Fixture::new();
    let when = format!("patina.os == '{}'", current_os_family());
    let module = f.module(
        "agent",
        &format!(
            "[[file]]\nsource = \"agent.toml\"\n\
             targets = [\"~/.codex/agent.toml\", \"~/.claude/agent.toml\"]\n\
             mode = \"copy\"\n\
             when = \"{when}\"\n"
        ),
    );
    fs_err::write(module.join("agent.toml"), "model = \"x\"\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "a multi-target `when`-true entry must apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.home.join(".codex/agent.toml").exists(),
        "every target of a `when`-true multi-target entry must be created"
    );
    assert!(
        f.home.join(".claude/agent.toml").exists(),
        "every target of a `when`-true multi-target entry must be created"
    );
}
