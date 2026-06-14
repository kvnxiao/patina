#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixtures; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for the `patina debug drift-cache <path>` CLI surface.
//!
//! The subcommand decodes a binary `drift.cache` file and renders it. These
//! tests write a cache file directly through the public `DriftCache::encode`
//! API (the same bytes the watcher atomically renames into place) and point
//! the CLI at it. That exercises the full binary path the scenario cares
//! about: read the file, check the version envelope, decode the body, render
//! to stdout.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::DRIFT_CACHE_MAJOR_VERSION;
use patina_core::DriftCache;
use patina_core::DriftEntry;
use std::process::Command;
use std::process::Output;
use tempfile::TempDir;

/// Write `bytes` to `<dir>/drift.cache` and return the path.
fn write_cache(dir: &Utf8Path, bytes: &[u8]) -> Utf8PathBuf {
    let path = dir.join("drift.cache");
    fs_err::write(&path, bytes).expect("write drift cache file");
    path
}

fn invoke(args: &[&str]) -> Output {
    let bin = env!("CARGO_BIN_EXE_patina");
    Command::new(bin).args(args).output().expect("spawn patina")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

#[test]
fn decodes_a_drift_cache_and_prints_version_timestamp_path_and_hashes() {
    // A populated drift cache renders with `version:`, the bound
    // journal timestamp, the target path, and both hash values; exit 0.
    let temp = TempDir::new().expect("tempdir");
    let dir = Utf8Path::from_path(temp.path()).expect("utf8 tempdir");
    let entry = DriftEntry::new("/home/u/.gitconfig", [0xab; 32], [0xcd; 32], 1_700_000_000);
    let cache = DriftCache::new("20260528T120000Z", vec![entry]);
    let path = write_cache(dir, &cache.encode().expect("encode"));

    let out = invoke(&["debug", "drift-cache", path.as_str()]);
    assert_eq!(
        code(&out),
        0,
        "debug drift-cache must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("version:"),
        "names the version; got: {stdout}"
    );
    assert!(
        stdout.contains("20260528T120000Z"),
        "names the bound journal timestamp; got: {stdout}"
    );
    assert!(
        stdout.contains(".gitconfig"),
        "names the target path; got: {stdout}"
    );
    assert!(
        stdout.contains(&"ab".repeat(32)),
        "names the expected hash; got: {stdout}"
    );
    assert!(
        stdout.contains(&"cd".repeat(32)),
        "names the actual hash; got: {stdout}"
    );
}

#[test]
fn missing_path_exits_one_and_names_the_path_on_stderr() {
    // A non-existent path -> exit 1, path on stderr.
    let out = invoke(&["debug", "drift-cache", "/nonexistent/drift.cache"]);
    assert_eq!(code(&out), 1, "missing path must exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("/nonexistent/drift.cache"),
        "stderr names the missing path; got: {stderr}"
    );
}

#[test]
fn newer_version_envelope_exits_one_and_names_both_versions() {
    // A cache whose envelope major is u16::MAX is
    // refused by a binary whose supported drift-cache major is 1 -> exit 1,
    // both versions plus the word "version" on stderr.
    let temp = TempDir::new().expect("tempdir");
    let dir = Utf8Path::from_path(temp.path()).expect("utf8 tempdir");
    let cache = DriftCache::new("20260528T120000Z", vec![]);
    let mut bytes = cache.encode().expect("encode");
    bytes
        .get_mut(..2)
        .expect("envelope")
        .copy_from_slice(&u16::MAX.to_le_bytes());
    let path = write_cache(dir, &bytes);

    let out = invoke(&["debug", "drift-cache", path.as_str()]);
    assert_eq!(code(&out), 1, "newer-major cache must exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("65535"), "names u16::MAX; got: {stderr}");
    assert!(
        stderr.contains(&DRIFT_CACHE_MAJOR_VERSION.to_string()),
        "names the supported major; got: {stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("version"),
        "names the version dimension; got: {stderr}"
    );
}

#[test]
fn debug_help_names_drift_cache_subcommand() {
    // `patina debug --help` names `drift-cache` and exits 0.
    let out = invoke(&["debug", "--help"]);
    assert_eq!(
        code(&out),
        0,
        "debug --help must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("drift-cache"),
        "debug --help names the drift-cache subcommand; got: {stdout}"
    );
}
