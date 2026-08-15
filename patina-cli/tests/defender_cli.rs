//! Integration tests for the `patina defender` CLI surface.
//!
//! Every test here stays on a **read-only** path. `apply` and `clear` mutate
//! antivirus configuration behind a UAC prompt, which no test may raise, so the
//! suite covers the preview and status paths that reach the same derivation,
//! diff, and rendering without ever launching the elevated helper: a non-TTY
//! subprocess without `--yes` previews and exits `0` by contract.
//!
//! These tests cover the CLI's report on a Defender exclusion list it cannot
//! read. Unelevated, as CI and a normal developer shell are,
//! `Get-MpPreference` withholds the list, and the tests pin the honest
//! rendering of that case. Elevated, a real list arrives and the
//! `current_readable` assertions legitimately flip, so every test that turns
//! on the distinction skips when the process is elevated.
//!
//! This doc block sits *above* the `cfg` deliberately. `patina defender` is
//! Windows-only, so the crate is gated away elsewhere, and a crate root
//! stripped down to nothing, doc comment included, trips `missing_docs` on the
//! cross-OS clippy leg.

#![cfg(windows)]
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use .expect()/panic! on fixtures and asserted JSON; allow-*-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]
#![expect(
    clippy::indexing_slicing,
    reason = "`serde_json::Value` indexing yields Value::Null for a missing key rather than panicking, so an assertion on a renamed envelope field fails with the field name and the whole envelope, which is better test signal than an unwrapped .get()."
)]

mod common;

use common::Fixture;
use common::code;
use std::process::Output;

/// A repository declaring one symlinked file, so the derived exclusion set is
/// the repository root plus exactly one managed target.
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

/// Whether this test process can read Defender's exclusion list. The
/// `current_readable` assertions below are only meaningful when it cannot.
fn elevated() -> bool {
    patina_core::is_elevated()
}

/// Canonicalize `path` the way the engine does, as the string form the CLI
/// reports.
///
/// Derived paths come out of the plan canonicalized, so a raw fixture path is
/// not comparable to one. The two forms differ on a CI runner, whose `%TEMP%`
/// resolves through a junction, and match on most developer machines. That
/// difference passes locally and fails in CI. `dunce::canonicalize` mirrors
/// the engine's `canonicalize_path`: a filesystem canonicalize with the
/// Windows `\\?\` verbatim prefix stripped where the plain form is
/// equivalent.
fn canonical(path: &camino::Utf8Path) -> String {
    let canon = dunce::canonicalize(path.as_std_path()).expect("canonicalize a fixture path");
    camino::Utf8PathBuf::from_path_buf(canon)
        .expect("a canonical fixture path is utf8")
        .into_string()
}

#[test]
fn apply_without_yes_previews_and_writes_nothing() {
    // This is the contract a non-interactive shell relies on: no `--yes`, no
    // mutation, exit 0. The rest of this suite depends on it holding.
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
    // Elevated, this process would see a real list and `current_readable` would
    // legitimately be true, so the assertion below would not hold.
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
    // Elevated, this process would see a real list and `current_readable` would
    // legitimately be true, so the assertion below would not hold.
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
    // With the live list withheld only the two ledger-derived states can arise;
    // `unmanaged` needs a readable list to be detected at all.
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
    // The kind is color-only in human output, so `--json` is the only place it
    // survives a pipe. The state token is the field a consumer branches on.
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
    // Derivation reaching the rendered envelope: the repository root as a
    // folder plus the one declared target as a file, and nothing else.
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
    // The target does not exist yet. Canonicalize its parent and rejoin the
    // leaf, matching the engine's own path resolution.
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
    // `clear` must stay usable as the reversibility escape hatch even with
    // nothing recorded, and it plans no repository, hence a null `repo_root`.
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
    // The deterministic-stdout bar. It also guards the ledger fallback: a diff
    // recomputed from a withheld list must not vary between runs.
    let fixture = fixture();
    let first = fixture.run(&["defender", "apply", "--json"], &[]);
    let second = fixture.run(&["defender", "apply", "--json"], &[]);

    assert_eq!(first.stdout, second.stdout);
}
