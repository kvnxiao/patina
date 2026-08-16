#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! `deny.toml` structure and policy integration test.
//!
//! Parses the workspace-root `deny.toml` as TOML and asserts:
//!
//! 1. The document has the top-level tables `licenses`, `advisories`, `bans`,
//!    and `sources`.
//! 2. The `[licenses].allow` allowlist contains no GPL-family licence, so a
//!    GPL-3.0 dependency is rejected by `cargo deny check`. Whether the binary
//!    actually exits non-zero on such a tree is a CI-execution fact; this test
//!    scripts the policy that makes it so.
//! 3. `[bans].wildcards` is set to `"deny"`, so a `some-crate = "*"` dependency
//!    is rejected.
//!
//! `deny.toml` is the policy artifact itself, so what the test gates is table
//! presence by key and the allow/deny decisions that determine `cargo deny`
//! outcomes. Each assertion fails for a realistic regression: a dropped
//! table, a GPL licence slipping into the allowlist, `wildcards` relaxed to
//! `"allow"`.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use toml::Table;
use toml::Value;

/// Absolute path to a file at the workspace root. `CARGO_MANIFEST_DIR` is the
/// `patina-cli` crate dir; the workspace root is its parent.
fn workspace_root_path(file: &str) -> Utf8PathBuf {
    let manifest_dir = Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .expect("patina-cli has a workspace-root parent");
    root.join(file)
}

fn parse_deny_toml() -> Table {
    let path = workspace_root_path("deny.toml");
    let text = fs_err::read_to_string(&path).expect("read deny.toml");
    text.parse::<Table>().expect("deny.toml parses as TOML")
}

#[test]
fn deny_toml_has_four_top_level_tables() {
    // deny.toml has every required top-level table. A dropped or renamed
    // section fails this assertion by name.
    let doc = parse_deny_toml();
    for table in ["licenses", "advisories", "bans", "sources"] {
        let entry = doc.get(table);
        assert!(
            entry.is_some_and(Value::is_table),
            "deny.toml missing top-level `[{table}]` table (or it is not a table): {entry:?}"
        );
    }
}

#[test]
fn licenses_allowlist_excludes_gpl_family() {
    // No GPL-family licence is in the allowlist, so a
    // GPL-3.0 dependency fails `cargo deny check licenses`. This catches a
    // GPL licence slipping into `allow` in a future edit.
    let doc = parse_deny_toml();
    let allow = doc
        .get("licenses")
        .and_then(Value::as_table)
        .and_then(|t| t.get("allow"))
        .and_then(Value::as_array)
        .expect("deny.toml [licenses].allow is an array");

    for entry in allow {
        let id = entry.as_str().expect("each allowed licence is a string");
        let upper = id.to_ascii_uppercase();
        assert!(
            !upper.contains("GPL"),
            "deny.toml [licenses].allow contains a GPL-family licence `{id}`; \
             GPL licences must not be allow-listed"
        );
    }
}

#[test]
fn bans_denies_wildcard_versions() {
    // `[bans].wildcards = "deny"` so a
    // `some-crate = "*"` dependency fails `cargo deny check bans`. This
    // catches a relaxation to "allow" or "warn".
    let doc = parse_deny_toml();
    let wildcards = doc
        .get("bans")
        .and_then(Value::as_table)
        .and_then(|t| t.get("wildcards"))
        .and_then(Value::as_str)
        .expect("deny.toml [bans].wildcards is a string");
    assert_eq!(
        wildcards, "deny",
        "deny.toml [bans].wildcards must be \"deny\" so wildcard versions are rejected"
    );
}
