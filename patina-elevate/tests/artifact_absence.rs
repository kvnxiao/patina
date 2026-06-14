//! A non-Windows release build of the workspace produces the main
//! `patina` binary but no `patina-elevate` artifact.
//!
//! The `patina-elevate` bin is gated `required-features = ["windows"]`, off
//! by default, so a plain `cargo build --release` skips it on macOS/Linux.
//! This test proves the gate actually bites — the most error-prone item in
//! the crate — rather than trusting it by inspection.
//!
//! Rather than scan the shared `target/release/` directory (which races every
//! other build and may hold stale artifacts from an earlier `--features
//! windows` run), it drives `cargo build --release --message-format=json` in a
//! hermetic target dir and reads the set of executables Cargo reports
//! emitting. That set is authoritative: an artifact Cargo did not build cannot
//! appear in it.
//!
//! Skipped on Windows, where the opposite is required (the bin *is* built);
//! this is a non-Windows-only contract.

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
    // Hermetic target dir under the OS temp area so the build does not race the
    // developer's shared `target/` (and reclaims cleanly).
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
        // Binary artifacts carry an `executable` path; library units carry
        // `null` there. Keep just the file stem so the assertion is
        // platform-suffix agnostic.
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
