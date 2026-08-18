//! Integration tests for `defender_cli`.
#![cfg(windows)]
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "Integration tests use .expect() and panic! for fixtures and asserted JSON outside #[cfg(test)] modules; allow-*-in-tests does not cover integration-crate roots."
)]
#![expect(
    clippy::indexing_slicing,
    reason = "Indexing a missing `serde_json::Value` key yields `Value::Null`; the assertion reports the renamed field and the full envelope."
)]

mod common;

use common::Fixture;
use common::code;
use std::process::Output;

fn fixture() -> Fixture {
    let fixture = Fixture::new();
    let module = fixture.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("gitconfig"), "[user]\n").expect("write the module source file");
    fixture
}

fn json_of(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|err| {
        panic!(
            "stdout must be a JSON envelope ({err}); stdout: {stdout}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn elevated() -> bool {
    patina_core::is_elevated()
}

fn canonical(path: &camino::Utf8Path) -> String {
    let canon = dunce::canonicalize(path.as_std_path()).expect("canonicalize a fixture path");
    camino::Utf8PathBuf::from_path_buf(canon)
        .expect("a canonical fixture path is utf8")
        .into_string()
}

#[test]
fn apply_without_yes_previews_and_writes_nothing() {
    let fixture = fixture();
    let output = fixture.run(&["defender", "apply", "--json"], &[]);

    assert_eq!(
        code(&output),
        0,
        "a preview must exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json_of(&output)["result"], "previewed");

    let state_dir = fixture.state_root();
    assert!(
        state_dir.exists(),
        "the run must have resolved this state directory for the checks below to mean anything"
    );
    for artifact in [
        "defender.json",
        "defender-request.txt",
        "defender-result.txt",
    ] {
        assert!(
            !state_dir.join(artifact).exists(),
            "a preview must not write `{artifact}`"
        );
    }
}

#[test]
fn a_preview_reports_that_the_live_list_was_not_readable() {
    if elevated() {
        return;
    }
    let envelope = json_of(&fixture().run(&["defender", "apply", "--json"], &[]));

    assert_eq!(
        envelope["current_readable"], false,
        "an unelevated run cannot read Defender's list and must say so: {envelope}"
    );
}

#[test]
fn status_reports_that_the_live_list_was_not_readable() {
    if elevated() {
        return;
    }
    let envelope = json_of(&fixture().run(&["defender", "status", "--json"], &[]));

    assert_eq!(envelope["current_readable"], false);
    let entries = envelope["exclusions"]
        .as_array()
        .expect("exclusions is an array");
    assert!(
        !entries.is_empty(),
        "the desired set is still reported when the live list is withheld: {envelope}"
    );
    for entry in entries {
        let state = entry["state"].as_str().expect("each entry carries a state");
        assert!(
            matches!(state, "recorded" | "unrecorded"),
            "an unprivileged read cannot produce `{state}`: {envelope}"
        );
    }
}

#[test]
fn every_status_entry_carries_its_kind_and_state_as_data() {
    let envelope = json_of(&fixture().run(&["defender", "status", "--json"], &[]));

    for entry in envelope["exclusions"]
        .as_array()
        .expect("exclusions is an array")
    {
        let kind = entry["kind"].as_str().expect("each entry carries a kind");
        assert!(
            matches!(kind, "file" | "folder"),
            "unexpected kind `{kind}`: {envelope}"
        );
        let state = entry["state"].as_str().expect("each entry carries a state");
        assert!(
            matches!(
                state,
                "owned" | "unmanaged" | "absent" | "recorded" | "unrecorded"
            ),
            "unexpected state `{state}`: {envelope}"
        );
    }
}

#[test]
fn the_preview_proposes_the_repo_root_and_each_managed_target() {
    let fixture = fixture();
    let envelope = json_of(&fixture.run(&["defender", "apply", "--json"], &[]));

    let proposed: Vec<(&str, &str)> = envelope["to_add"]
        .as_array()
        .expect("to_add is an array")
        .iter()
        .map(|entry| {
            (
                entry["path"].as_str().expect("path is a string"),
                entry["kind"].as_str().expect("kind is a string"),
            )
        })
        .collect();

    let repo_root = canonical(&fixture.root);
    assert!(
        proposed.contains(&(repo_root.as_str(), "folder")),
        "the repository root must be proposed as a folder exclusion: {proposed:?}"
    );
    let target = format!("{}\\.gitconfig", canonical(&fixture.home));
    assert!(
        proposed.contains(&(target.as_str(), "file")),
        "the declared target must be proposed as a file exclusion: {proposed:?}"
    );
    assert_eq!(
        proposed.len(),
        2,
        "nothing beyond the root and the one target: {proposed:?}"
    );
}

#[test]
fn clear_previews_an_empty_removal_set_with_no_ledger() {
    let output = fixture().run(&["defender", "clear", "--json"], &[]);

    assert_eq!(code(&output), 0);
    let envelope = json_of(&output);
    assert_eq!(envelope["result"], "previewed");
    assert!(envelope["repo_root"].is_null());
    assert_eq!(
        envelope["to_remove"].as_array().map(Vec::len),
        Some(0),
        "an empty ledger leaves nothing to reap: {envelope}"
    );
}

#[test]
fn repeated_previews_are_byte_identical() {
    let fixture = fixture();
    let first = fixture.run(&["defender", "apply", "--json"], &[]);
    let second = fixture.run(&["defender", "apply", "--json"], &[]);

    assert_eq!(first.stdout, second.stdout);
}
