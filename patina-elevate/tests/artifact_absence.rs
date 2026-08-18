//! A non-Windows release build of the workspace emits the main `patina`
//! binary but skips the `patina-elevate` bin.
//!
//! Scanning the shared `target/release/` directory would race concurrent
//! builds and could match a stale artifact left by an earlier
//! `--features windows` run. The test drives `cargo build --release
//! --message-format=json` in a hermetic target dir and reads the set of
//! executables Cargo reports emitting.
//!
//! Skipped on Windows, where the bin is built.

#![cfg(not(windows))]

use serde_json::Value;
use std::path::Path;
use std::process::Command;

#[test]
fn release_build_emits_patina_but_not_patina_elevate() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("the crate dir has a workspace-root parent");
    let target_dir = tempfile::tempdir().expect("create scratch target dir");

    let output = Command::new(env!("CARGO"))
        .args(["build", "--workspace", "--release", "--message-format=json"])
        .current_dir(workspace_root)
        .env("CARGO_TARGET_DIR", target_dir.path())
        .output()
        .expect("spawn cargo build");

    assert!(
        output.status.success(),
        "release build must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("cargo build stdout is utf8");
    let mut executables = Vec::new();
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        // Library units report a null `executable`; the file stem keeps the
        // assertion suffix-agnostic.
        if let Some(exe) = value.get("executable").and_then(Value::as_str)
            && let Some(name) = Path::new(exe).file_stem().and_then(|s| s.to_str())
        {
            executables.push(name.to_owned());
        }
    }

    assert!(
        executables.iter().any(|name| name == "patina"),
        "the release build must emit the main `patina` binary; emitted: {executables:?}"
    );
    assert!(
        !executables
            .iter()
            .any(|name| name == "patina-elevate" || name == "patina-elevate.exe"),
        "a non-Windows release build must not emit `patina-elevate`; emitted: {executables:?}"
    );
}
