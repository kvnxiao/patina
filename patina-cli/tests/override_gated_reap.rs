//! Integration tests for `-v`-gated entries and the orphan reap.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use .expect()/panic! on fixtures and asserted output; allow-*-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

mod common;

use common::Fixture;
use common::code;

/// A module whose only entry is gated on the `deploy` variable, plus an
/// optional repo-shared default for it.
fn gated_fixture(repo_shared: Option<&str>) -> Fixture {
    let f = Fixture::new();
    if let Some(value) = repo_shared {
        fs_err::write(
            f.root.join("patina.toml"),
            format!("[patina]\nroot = true\n\n[variables]\ndeploy = \"{value}\"\n"),
        )
        .expect("rewrite root manifest");
    }
    let module = f.module(
        "cfg",
        "[[file]]\nsource = \"conf\"\ntarget = \"~/.conf\"\nmode = \"copy\"\n\
         when = \"deploy == 'yes'\"\n",
    );
    fs_err::write(module.join("conf"), b"payload\n").expect("write source");
    f
}

fn status_json(out: &std::process::Output) -> serde_json::Value {
    assert_eq!(
        code(out),
        0,
        "status must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .expect("status stdout must be a single JSON document")
}

fn state_for(doc: &serde_json::Value, suffix: &str) -> String {
    let files = doc
        .get("files")
        .and_then(serde_json::Value::as_array)
        .expect("files array");
    for entry in files {
        let path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if path.replace('\\', "/").ends_with(suffix) {
            return entry
                .get("state")
                .and_then(serde_json::Value::as_str)
                .expect("state string")
                .to_owned();
        }
    }
    panic!("no files entry ending in `{suffix}` in {doc}");
}

#[test]
fn an_override_gated_entry_survives_the_apply_that_materialized_it() {
    let f = gated_fixture(Some("no"));
    let target = f.home.join(".conf");

    let applied = f.apply(&["--yes", "-v", "deploy=yes"]);
    assert_eq!(
        code(&applied),
        0,
        "the overridden apply must commit; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(
        target.is_file(),
        "the reap must not delete the target the same apply just materialized"
    );

    let again = f.apply(&["--yes", "-v", "deploy=yes"]);
    assert_eq!(
        code(&again),
        0,
        "the second overridden apply must succeed; stderr: {}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("Already up to date"),
        "a committed override-gated entry must re-apply as a no-op; stdout: {}",
        String::from_utf8_lossy(&again.stdout)
    );
    assert!(target.is_file(), "the target must still be on disk");
}

#[test]
fn status_classifies_an_override_gated_target_under_the_same_overrides() {
    let f = gated_fixture(Some("no"));

    let applied = f.apply(&["--yes", "-v", "deploy=yes"]);
    assert_eq!(
        code(&applied),
        0,
        "the overridden apply must commit; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    let with_override = status_json(&f.run(&["status", "--json", "-v", "deploy=yes"], &[]));
    assert_eq!(
        state_for(&with_override, "/.conf"),
        "clean",
        "under the apply's own overrides the target is managed and clean: {with_override}"
    );

    let without_override = status_json(&f.run(&["status", "--json"], &[]));
    assert_eq!(
        state_for(&without_override, "/.conf"),
        "orphaned",
        "with the repo-shared default the entry is gated off, so status reports the \
         leftover: {without_override}"
    );
}

#[test]
fn an_override_gated_entry_with_no_repo_shared_default_still_applies() {
    let f = gated_fixture(None);
    let target = f.home.join(".conf");

    let applied = f.apply(&["--yes", "-v", "deploy=yes"]);
    assert_eq!(
        code(&applied),
        0,
        "an override is the only binding for `deploy`, and the apply must not \
         re-resolve the predicate without it; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(target.is_file(), "the target must be materialized");
}

/// The watcher re-applies with `ApplyRequest::default()`, the same
/// no-override request a bare `patina apply` builds.
#[test]
fn an_apply_without_the_override_refuses_rather_than_reaping() {
    let f = gated_fixture(None);
    let target = f.home.join(".conf");

    let applied = f.apply(&["--yes", "-v", "deploy=yes"]);
    assert_eq!(
        code(&applied),
        0,
        "the overridden apply must commit; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    let bare = f.apply(&["--yes"]);
    let stderr = String::from_utf8_lossy(&bare.stderr);
    assert_eq!(
        code(&bare),
        1,
        "`deploy` is bound only by the override, so the predicate is undefined; \
         stderr: {stderr}"
    );
    assert!(
        stderr.contains("deploy"),
        "the error must name the undefined variable; stderr: {stderr}"
    );
    assert!(
        target.is_file(),
        "a plan that never resolved must mutate nothing, the reap included"
    );
}
