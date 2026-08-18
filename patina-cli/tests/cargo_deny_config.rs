//! Integration tests for `cargo_deny_config`.
#![expect(
    clippy::expect_used,
    reason = "Integration tests use .expect() for fixture setup outside #[cfg(test)] modules; allow-expect-in-tests does not cover integration-crate roots."
)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use toml::Table;
use toml::Value;

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
