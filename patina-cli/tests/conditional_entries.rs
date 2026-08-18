//! Integration tests for `conditional_entries`.
mod common;

use common::Fixture;
use common::code;

fn current_os_family() -> &'static str {
    std::env::consts::OS
}

#[test]
fn when_false_entry_creates_no_target_and_plans_zero_operations() {
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
