#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! `deny.toml` structure and policy integration test (REQ-028, CHK-060;
//! plus the licence- and bans-policy invariants behind CHK-061).
//!
//! Parses the workspace-root `deny.toml` as TOML and asserts:
//!
//! 1. (CHK-060) the document carries the four top-level tables `licenses`,
//!    `advisories`, `bans`, and `sources`.
//! 2. (CHK-061 policy half) no GPL-family licence appears in the
//!    `[licenses].allow` allowlist, so a GPL-3.0 dependency is rejected by
//!    `cargo deny check`. Whether the binary actually exits non-zero on such a
//!    tree is a CI-execution fact; what is scriptable here is the policy that
//!    makes it so.
//! 3. (wildcard task-scenario policy half) `[bans].wildcards` is set to
//!    `"deny"`, so a `some-crate = "*"` dependency is rejected.
//!
//! The test gates the *policy* encoded in `deny.toml` — table presence by key
//! and the allow/deny decisions that determine `cargo deny` outcomes. It does
//! not substring-match the file's comments or re-assert a production constant:
//! `deny.toml` is the policy artifact itself, and each assertion would fail for
//! a realistic regression (a dropped table, a GPL licence slipping into the
//! allowlist, `wildcards` relaxed to `"allow"`).

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
    // CHK-060: deny.toml carries the four required top-level tables. A dropped
    // or renamed section drops out of the check and fails here naming it.
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
    // CHK-061 (policy half): no GPL-family licence is in the allowlist, so a
    // GPL-3.0 dependency fails `cargo deny check licenses`. Catches a GPL
    // licence slipping into `allow` in a future edit.
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
             GPL licences must not be allow-listed (REQ-028)"
        );
    }
}

#[test]
fn bans_denies_wildcard_versions() {
    // Wildcard task-scenario (policy half): `[bans].wildcards = "deny"` so a
    // `some-crate = "*"` dependency fails `cargo deny check bans`. Catches a
    // relaxation to "allow"/"warn".
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
