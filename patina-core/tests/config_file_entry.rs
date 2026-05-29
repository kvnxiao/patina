#![expect(
    clippy::indexing_slicing,
    reason = "integration tests use direct [0] / [1] indexing for assertion-only fixture inspection where the vector length is already asserted immediately above; bounds-check panics would be acceptable test signal anyway."
)]

//! Integration tests for the `[[file]]` table-array schema (REQ-005).

use patina_core::ConfigParseError;
use patina_core::FileMode;
use patina_core::config::FileEntryError;
use patina_core::config::parse_module_config_str;

#[test]
fn parses_single_target_explicit_symlink_mode() {
    // First scenario: source = "zshrc", target = "~/.zshrc", mode = "symlink".
    let toml = r#"
[[file]]
source = "zshrc"
target = "~/.zshrc"
mode = "symlink"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 1);
    let entry = &config.files[0];
    assert_eq!(entry.source.as_str(), "zshrc");
    assert_eq!(entry.mode, FileMode::Symlink);
    assert_eq!(
        entry.targets.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        vec!["~/.zshrc"]
    );
}

#[test]
fn defaults_to_symlink_when_mode_omitted() {
    // CHK-041: source = "zshrc", target = "~/.zshrc", no mode -> Symlink.
    let toml = r#"
[[file]]
source = "zshrc"
target = "~/.zshrc"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 1);
    assert_eq!(config.files[0].mode, FileMode::Symlink);
}

#[test]
fn parses_targets_array_with_copy_mode() {
    // Third scenario: targets = ["~/a", "~/b"], mode = "copy" -> Copy.
    let toml = r#"
[[file]]
source = "agent.toml"
targets = ["~/a", "~/b"]
mode = "copy"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 1);
    let entry = &config.files[0];
    assert_eq!(entry.mode, FileMode::Copy);
    assert_eq!(
        entry.targets.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        vec!["~/a", "~/b"]
    );
}

#[test]
fn rejects_merge_json_mode_listing_accepted_values() {
    // CHK-012: mode = "merge-json" -> typed error whose Display contains
    // "merge-json", "symlink", "symlink-dir", "copy", "copy-tree".
    let toml = r#"
[[file]]
mode = "merge-json"
source = "x"
target = "y"
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(rendered.contains("merge-json"), "rendered: {rendered}");
    assert!(rendered.contains("symlink"), "rendered: {rendered}");
    assert!(rendered.contains("symlink-dir"), "rendered: {rendered}");
    assert!(rendered.contains("copy"), "rendered: {rendered}");
    assert!(rendered.contains("copy-tree"), "rendered: {rendered}");
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::UnsupportedMode { .. })
    ));
}

#[test]
fn rejects_target_and_targets_both_set() {
    // CHK-045: both target and targets declared -> Display names both
    // and contains "exactly one".
    let toml = r#"
[[file]]
source = "agent.toml"
target = "~/.claude/agent.toml"
targets = ["~/.codex/agent.toml"]
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(rendered.contains("target"), "rendered: {rendered}");
    assert!(rendered.contains("targets"), "rendered: {rendered}");
    assert!(rendered.contains("exactly one"), "rendered: {rendered}");
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::TargetAndTargets)
    ));
}

#[test]
fn rejects_neither_target_nor_targets() {
    // CHK-046: neither target nor targets -> Display contains "target",
    // "targets", and "missing".
    let toml = r#"
[[file]]
source = "agent.toml"
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(rendered.contains("target"), "rendered: {rendered}");
    assert!(rendered.contains("targets"), "rendered: {rendered}");
    assert!(rendered.contains("missing"), "rendered: {rendered}");
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::TargetMissing)
    ));
}

#[test]
fn rejects_empty_targets_array() {
    // CHK-047: targets = [] -> Display contains "targets" and "non-empty".
    let toml = r#"
[[file]]
source = "agent.toml"
targets = []
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(rendered.contains("targets"), "rendered: {rendered}");
    assert!(rendered.contains("non-empty"), "rendered: {rendered}");
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::TargetsEmpty)
    ));
}

#[test]
fn rejects_tmpl_source_with_explicit_mode() {
    // Eighth scenario: source = "foo.tmpl", mode = "copy" -> error
    // naming the .tmpl suffix and implicit-template rule.
    let toml = r#"
[[file]]
source = "foo.tmpl"
target = "y"
mode = "copy"
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(rendered.contains(".tmpl"), "rendered: {rendered}");
    assert!(
        rendered.contains("implicit-template"),
        "rendered: {rendered}"
    );
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::ImplicitTemplateModeDeclared { .. })
    ));
}

#[test]
fn tmpl_source_resolves_to_template_render_mode() {
    // Implicit-template rule positive coverage: a .tmpl source with no
    // explicit mode declaration resolves to FileMode::TemplateRender.
    let toml = r#"
[[file]]
source = "gitconfig.tmpl"
target = "~/.gitconfig"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 1);
    assert_eq!(config.files[0].mode, FileMode::TemplateRender);
}

#[test]
fn parses_symlink_dir_and_copy_tree_modes() {
    // Allowlist coverage for the other two declarable modes named in
    // REQ-005's parse-time rule 2.
    let toml = r#"
[[file]]
source = "nvim-config"
target = "~/.config/nvim"
mode = "symlink-dir"

[[file]]
source = "scripts"
target = "~/bin"
mode = "copy-tree"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 2);
    assert_eq!(config.files[0].mode, FileMode::SymlinkDir);
    assert_eq!(config.files[1].mode, FileMode::CopyTree);
}

#[test]
fn variables_table_is_preserved_for_t006() {
    // T-006 handoff: a module's [variables] table is captured raw and
    // surfaced on ModuleConfig.variables. Validation of keys (including
    // the reserved patina.* namespace rule) is T-006's responsibility.
    let toml = r#"
[variables]
email = "kevin@example.com"

[[file]]
source = "zshrc"
target = "~/.zshrc"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    let variables = config.variables.expect("variables table preserved");
    assert_eq!(
        variables
            .get("email")
            .and_then(|v| v.as_str())
            .expect("email key present"),
        "kevin@example.com"
    );
}
