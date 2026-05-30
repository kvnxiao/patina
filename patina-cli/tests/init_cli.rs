#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

//! Integration coverage for `patina init` (REQ-001, REQ-010).
//!
//! Each test spawns the real `patina` binary against an isolated tempdir
//! repo + state + home (via the shared [`common::Fixture`]). `init` targets
//! a fresh directory under the fixture's home so the fixture's own root
//! manifest never collides with the directory under test.

mod common;

use common::Fixture;
use common::code;

/// CHK-001: `patina init T` in an empty directory scaffolds the root
/// manifest, persists the canonical pointer, prints the next-step hint, and
/// exits 0.
#[test]
fn init_scaffolds_manifest_pointer_and_hint() {
    let fx = Fixture::new();
    let target = fx.home.join("dot");

    let out = fx.run(&["init", target.as_str()], &[]);
    assert_eq!(code(&out), 0, "init in an empty dir must exit 0");

    // The manifest exists with [patina] root = true.
    let manifest = target.join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read manifest");
    let parsed: toml::Value = toml::from_str(&body).expect("manifest parses as TOML");
    assert_eq!(
        parsed
            .get("patina")
            .and_then(|t| t.get("root"))
            .and_then(toml::Value::as_bool),
        Some(true),
        "[patina].root must be true"
    );

    // The state directory's default_repo file holds the canonical absolute
    // path of the target.
    let pointer = fx.state_root().join("default_repo");
    let recorded = fs_err::read_to_string(pointer.as_std_path()).expect("read default_repo");
    let canonical = canonical_string(&target);
    assert_eq!(
        recorded.trim(),
        canonical,
        "default_repo must hold the canonical target path"
    );

    // The final stdout line is the `patina add` next-step hint.
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let last = stdout.lines().last().expect("at least one stdout line");
    assert!(
        last.starts_with("Next: run `patina add ")
            && last.ends_with("register an existing dotfile."),
        "final stdout line must be the add hint, got: {last:?}"
    );
}

/// CHK-002: `patina init T` against a directory that already contains a
/// `patina.toml` leaves the file byte-identical, names it on stderr with
/// `already exists`, and exits 1.
#[test]
fn init_refuses_when_manifest_exists() {
    let fx = Fixture::new();
    let target = fx.home.join("dot");
    fs_err::create_dir_all(target.as_std_path()).expect("mkdir target");
    let manifest = target.join("patina.toml");
    let original = "[patina]\nroot = true\n# hand-written\n";
    fs_err::write(manifest.as_std_path(), original).expect("seed manifest");

    let out = fx.run(&["init", target.as_str()], &[]);
    assert_eq!(
        code(&out),
        1,
        "init against an existing manifest must exit 1"
    );

    let after = fs_err::read_to_string(manifest.as_std_path()).expect("read manifest");
    assert_eq!(after, original, "existing manifest must be byte-identical");

    let stderr = String::from_utf8(out.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("already exists"),
        "stderr must contain `already exists`, got: {stderr:?}"
    );
    assert!(
        stderr.contains(manifest.as_str()),
        "stderr must name the existing manifest path, got: {stderr:?}"
    );

    // The state directory was never touched: no pointer was written.
    let pointer = fx.state_root().join("default_repo");
    assert!(
        !pointer.exists(),
        "a refused init must not write the default_repo pointer"
    );
}

/// CHK-017: `patina init T --json` run twice against an already-initialized
/// directory produces byte-identical stdout (deterministic failure
/// document), and a successful `--json` run is itself byte-stable on
/// repetition.
#[test]
fn init_json_failure_is_byte_stable_across_reruns() {
    let fx = Fixture::new();
    let target = fx.home.join("dot");
    fs_err::create_dir_all(target.as_std_path()).expect("mkdir target");
    fs_err::write(
        target.join("patina.toml").as_std_path(),
        "[patina]\nroot = true\n",
    )
    .expect("seed manifest");

    let first = fx.run(&["init", target.as_str(), "--json"], &[]);
    let second = fx.run(&["init", target.as_str(), "--json"], &[]);

    assert_eq!(code(&first), 1, "already-initialized init must exit 1");
    assert_eq!(code(&second), 1, "already-initialized init must exit 1");
    assert_eq!(
        first.stdout, second.stdout,
        "two failing --json runs must emit byte-identical stdout"
    );

    // The failing --json stdout is a typed-error document naming the path.
    let stdout = String::from_utf8(first.stdout).expect("utf8 stdout");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("failing --json stdout is one JSON doc");
    assert_eq!(
        doc.get("error").and_then(serde_json::Value::as_str),
        Some("already_exists")
    );
    assert_eq!(
        doc.get("path").and_then(serde_json::Value::as_str),
        Some(target.join("patina.toml").as_str())
    );
}

/// REQ-010 success schema: a successful `init --json` emits a single
/// deterministic JSON document on stdout whose `initialized` and
/// `default_repo` fields carry the canonical target and pointer paths and
/// nothing non-deterministic (no `created_at` timestamp).
#[test]
fn init_json_success_emits_deterministic_schema() {
    let fx = Fixture::new();
    let target = fx.home.join("dot");

    let out = fx.run(&["init", target.as_str(), "--json"], &[]);
    assert_eq!(code(&out), 0, "init success must exit 0");

    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is one JSON doc");
    let canonical = canonical_string(&target);
    assert_eq!(
        doc.get("initialized").and_then(serde_json::Value::as_str),
        Some(canonical.as_str()),
        "initialized field must carry the canonical target path"
    );
    let pointer = fx.state_root().join("default_repo");
    assert_eq!(
        doc.get("default_repo").and_then(serde_json::Value::as_str),
        Some(pointer.as_str()),
        "default_repo field must carry the pointer path"
    );
}

/// Canonicalize `path` the same way the engine's `canonicalize_path` does,
/// returning the UTF-8 string form. The test computes the expected pointer
/// value independently of the binary under test.
fn canonical_string(path: &camino::Utf8Path) -> String {
    let canon = fs_err::canonicalize(path.as_std_path()).expect("canonicalize target");
    camino::Utf8PathBuf::from_path_buf(canon)
        .expect("canonical path is utf8")
        .to_string()
}
