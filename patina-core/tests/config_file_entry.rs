#![expect(
    clippy::indexing_slicing,
    reason = "integration tests use direct [0] / [1] indexing for assertion-only fixture inspection where the vector length is already asserted immediately above; bounds-check panics would be acceptable test signal anyway."
)]

//! Integration tests for the kind-typed `[[file]]` / `[[directory]]`
//! table-array schema (REQ-001).

use patina_core::ConfigParseError;
use patina_core::FileMode;
use patina_core::config::EntryKind;
use patina_core::config::FileEntryError;
use patina_core::config::parse_module_config_str;

#[test]
fn parses_single_target_explicit_symlink_mode() {
    // source = "zshrc", target = "~/.zshrc", mode = "symlink".
    let toml = r#"
[[file]]
source = "zshrc"
target = "~/.zshrc"
mode = "symlink"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 1);
    let entry = &config.files[0];
    assert_eq!(entry.kind, EntryKind::File);
    assert_eq!(entry.source.as_str(), "zshrc");
    assert_eq!(entry.mode, FileMode::Symlink);
    assert_eq!(entry.when, None);
    assert_eq!(
        entry.targets.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        vec!["~/.zshrc"]
    );
}

#[test]
fn file_with_omitted_mode_resolves_to_file_kind_symlink() {
    // CHK-001: a [[file]] with source = "zshrc", target = "~/.zshrc" and
    // no mode resolves to file kind and symlink mode.
    let toml = r#"
[[file]]
source = "zshrc"
target = "~/.zshrc"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 1);
    assert_eq!(config.files[0].kind, EntryKind::File);
    assert_eq!(config.files[0].mode, FileMode::Symlink);
}

#[test]
fn directory_symlink_tree_resolves_to_directory_kind_per_leaf_symlink() {
    // CHK-002: a [[directory]] with source = "mpv", target = "~/.config/mpv",
    // mode = "symlink-tree" resolves to directory kind and per-leaf symlink.
    let toml = r#"
[[directory]]
source = "mpv"
target = "~/.config/mpv"
mode = "symlink-tree"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert!(config.files.is_empty(), "no [[file]] entries declared");
    assert_eq!(config.directories.len(), 1);
    let entry = &config.directories[0];
    assert_eq!(entry.kind, EntryKind::Directory);
    assert_eq!(entry.mode, FileMode::SymlinkTree);
    assert_eq!(
        entry.targets.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        vec!["~/.config/mpv"]
    );
}

#[test]
fn directory_omitted_mode_resolves_to_atomic_whole_directory_symlink() {
    // REQ-001 done-when: a [[directory]] with mode omitted resolves to the
    // atomic whole-directory symlink (the prior `symlink-dir` behavior).
    let toml = r#"
[[directory]]
source = "nvim-config"
target = "~/.config/nvim"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.directories.len(), 1);
    assert_eq!(config.directories[0].mode, FileMode::SymlinkDir);
}

#[test]
fn directory_copy_resolves_to_recursive_copy() {
    // REQ-001 done-when: a [[directory]] with mode = "copy" resolves to a
    // recursive directory copy (the prior `copy-tree` behavior).
    let toml = r#"
[[directory]]
source = "scripts"
target = "~/bin"
mode = "copy"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.directories.len(), 1);
    assert_eq!(config.directories[0].mode, FileMode::CopyTree);
}

#[test]
fn parses_targets_array_with_copy_mode() {
    // A [[file]] with targets = ["~/a", "~/b"], mode = "copy" -> Copy.
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
fn carries_optional_when_expression_verbatim() {
    // The optional `when` field is parsed and carried as raw source
    // (evaluation lands in T-005).
    let toml = r#"
[[file]]
source = "wmrc"
target = "~/.wmrc"
when = "patina.os == 'windows'"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 1);
    assert_eq!(
        config.files[0].when.as_deref(),
        Some("patina.os == 'windows'")
    );
}

#[test]
fn file_with_symlink_tree_mode_is_rejected_naming_accepted_file_modes() {
    // CHK-003: a [[file]] declaring mode = "symlink-tree" fails with a
    // typed error whose message contains `symlink-tree` and the accepted
    // [[file]] modes `symlink` and `copy`.
    let toml = r#"
[[file]]
source = "x"
target = "y"
mode = "symlink-tree"
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(rendered.contains("symlink-tree"), "rendered: {rendered}");
    assert!(rendered.contains("symlink"), "rendered: {rendered}");
    assert!(rendered.contains("copy"), "rendered: {rendered}");
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::UnsupportedFileMode { .. })
    ));
}

#[test]
fn file_with_removed_dir_mode_names_accepted_file_modes() {
    // REQ-001 done-when: a [[file]] declaring a removed `symlink-dir` /
    // `copy-tree` mode is rejected naming the accepted [[file]] modes.
    for removed in ["symlink-dir", "copy-tree"] {
        let toml = format!(
            r#"
[[file]]
source = "x"
target = "y"
mode = "{removed}"
"#
        );
        let err = parse_module_config_str(&toml).expect_err("parse fails");
        let rendered = err.to_string();
        assert!(rendered.contains(removed), "rendered: {rendered}");
        assert!(rendered.contains("symlink"), "rendered: {rendered}");
        assert!(rendered.contains("copy"), "rendered: {rendered}");
        assert!(matches!(
            err,
            ConfigParseError::FileEntry(FileEntryError::UnsupportedFileMode { .. })
        ));
    }
}

#[test]
fn directory_with_removed_mode_names_accepted_directory_modes() {
    // REQ-001 done-when: a [[directory]] declaring `symlink-dir` or
    // `copy-tree` is rejected naming the accepted [[directory]] modes
    // `symlink`, `symlink-tree`, `copy`.
    for removed in ["symlink-dir", "copy-tree"] {
        let toml = format!(
            r#"
[[directory]]
source = "d"
target = "~/d"
mode = "{removed}"
"#
        );
        let err = parse_module_config_str(&toml).expect_err("parse fails");
        let rendered = err.to_string();
        assert!(rendered.contains(removed), "rendered: {rendered}");
        assert!(rendered.contains("symlink"), "rendered: {rendered}");
        assert!(rendered.contains("symlink-tree"), "rendered: {rendered}");
        assert!(rendered.contains("copy"), "rendered: {rendered}");
        assert!(matches!(
            err,
            ConfigParseError::FileEntry(FileEntryError::UnsupportedDirectoryMode { .. })
        ));
    }
}

#[test]
fn directory_with_tmpl_source_is_rejected() {
    // REQ-001: a [[directory]] whose source ends in `.tmpl` is rejected —
    // template render is file-only.
    let toml = r#"
[[directory]]
source = "theme.tmpl"
target = "~/.config/theme"
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(rendered.contains("theme.tmpl"), "rendered: {rendered}");
    assert!(rendered.contains(".tmpl"), "rendered: {rendered}");
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::DirectoryTemplateSource { .. })
    ));
}

#[test]
fn rejects_target_and_targets_both_set_on_file() {
    // Exactly-one-of rule applies to [[file]].
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
fn rejects_target_and_targets_both_set_on_directory() {
    // The exactly-one-of rule applies identically to [[directory]].
    let toml = r#"
[[directory]]
source = "d"
target = "~/d"
targets = ["~/e"]
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::TargetAndTargets)
    ));
}

#[test]
fn rejects_neither_target_nor_targets() {
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
fn rejects_empty_targets_array_on_directory() {
    // The non-empty-targets rule applies identically to [[directory]].
    let toml = r#"
[[directory]]
source = "d"
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
fn rejects_unknown_file_mode_naming_accepted_values() {
    // A wholly unknown mode on [[file]] is rejected naming the accepted
    // [[file]] modes only (not the removed dir-mode spellings).
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
    assert!(rendered.contains("copy"), "rendered: {rendered}");
    assert!(
        !rendered.contains("symlink-dir") && !rendered.contains("copy-tree"),
        "removed mode spellings must not appear in the [[file]] error: {rendered}"
    );
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::UnsupportedFileMode { .. })
    ));
}

#[test]
fn rejects_tmpl_source_with_explicit_mode_on_file() {
    // A [[file]] .tmpl source plus an explicit mode is rejected, naming the
    // .tmpl suffix and the implicit-template rule.
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
fn tmpl_file_source_resolves_to_template_render_mode() {
    // A [[file]] .tmpl source with no explicit mode resolves to
    // FileMode::TemplateRender.
    let toml = r#"
[[file]]
source = "gitconfig.tmpl"
target = "~/.gitconfig"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 1);
    assert_eq!(config.files[0].kind, EntryKind::File);
    assert_eq!(config.files[0].mode, FileMode::TemplateRender);
}

#[test]
fn both_tables_parse_together_into_their_respective_vecs() {
    // REQ-001 behavior: a manifest with both a [[file]] (mode omitted) and
    // a [[directory]] with mode = "symlink-tree" resolves the file entry to
    // a single-file symlink and the directory entry to per-leaf symlinks.
    let toml = r#"
[[file]]
source = "zshrc"
target = "~/.zshrc"

[[directory]]
source = "mpv"
target = "~/.config/mpv"
mode = "symlink-tree"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.files.len(), 1);
    assert_eq!(config.files[0].mode, FileMode::Symlink);
    assert_eq!(config.directories.len(), 1);
    assert_eq!(config.directories[0].mode, FileMode::SymlinkTree);
}

#[test]
fn variables_table_is_preserved() {
    // A module's [variables] table is captured raw and surfaced on
    // ModuleConfig.variables.
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
