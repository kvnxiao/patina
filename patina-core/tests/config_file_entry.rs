//! Integration tests for config file entry.

#![expect(
    clippy::indexing_slicing,
    reason = "integration tests use direct [0] / [1] indexing for assertion-only fixture inspection where the vector length is already asserted immediately above; bounds-check panics would be acceptable test signal anyway."
)]

use patina_core::ConfigParseError;
use patina_core::FileMode;
use patina_core::config::EntryKind;
use patina_core::config::FileEntryError;
use patina_core::config::parse_module_config_str;

#[test]
fn parses_single_target_explicit_symlink_mode() {
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
fn rejects_a_control_character_in_a_target() {
    for (escape, codepoint) in [
        ("\\t", 0x09_u32),
        ("\\n", 0x0A),
        ("\\r", 0x0D),
        ("\\u001B", 0x1B),
        ("\\u007F", 0x7F),
    ] {
        let toml = format!(
            "
[[file]]
source = \"agent.toml\"
target = \"~/.config/ag{escape}ent.toml\"
"
        );
        let err = parse_module_config_str(&toml).expect_err("a control character must be refused");
        let rendered = err.to_string();
        assert!(
            rendered.contains("control character"),
            "rendered: {rendered}"
        );
        assert!(
            rendered.contains(&format!("U+{codepoint:04X}")),
            "the message must name which character is at fault: {rendered}"
        );
        assert!(
            !rendered.chars().any(|c| c.is_ascii_control()),
            "the message must not embed the raw character it rejects: {rendered:?}"
        );
        assert!(matches!(
            err,
            ConfigParseError::FileEntry(FileEntryError::TargetControlCharacter {
                codepoint: found,
                ..
            }) if found == codepoint
        ));
    }
}
#[test]
fn rejects_a_control_character_in_a_source() {
    let toml = "
[[directory]]
source = \"sk\\tills\"
target = \"~/.claude/skills\"
";
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(
        rendered.contains("control character"),
        "rendered: {rendered}"
    );
    assert!(rendered.contains("source"), "rendered: {rendered}");
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::SourceControlCharacter { .. })
    ));
}

#[test]
fn rejects_a_control_character_in_a_later_fan_out_target() {
    let toml = "
[[directory]]
source = \"d\"
targets = [\"~/.config/a\", \"~/.config/b\\tc\"]
";
    let err = parse_module_config_str(toml).expect_err("parse fails");
    assert!(matches!(
        err,
        ConfigParseError::FileEntry(FileEntryError::TargetControlCharacter { codepoint, .. })
            if codepoint == 0x09
    ));
}

#[test]
fn accepts_spaces_and_non_ascii_in_paths() {
    let toml = "
[[file]]
source = \"agent.toml\"
target = \"~/Application Support/ünïcode 😀/agent.toml\"
";
    let module = parse_module_config_str(toml).expect("spaces and non-ASCII are legal");
    assert_eq!(
        module.files[0].targets[0].as_str(),
        "~/Application Support/ünïcode 😀/agent.toml"
    );
}

#[test]
fn rejects_unknown_file_mode_naming_accepted_values() {
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

#[test]
fn tree_modes_accept_an_ignore_list_in_declaration_order() {
    let toml = r#"
[[directory]]
source = "scripts"
target = "~/bin"
mode = "symlink-tree"
ignore = ["__pycache__/", "*.pyc"]

[[directory]]
source = "fonts"
target = "~/.fonts"
mode = "copy"
ignore = [".DS_Store"]
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.directories.len(), 2);
    assert_eq!(config.directories[0].mode, FileMode::SymlinkTree);
    assert_eq!(config.directories[0].ignore, ["__pycache__/", "*.pyc"]);
    assert_eq!(config.directories[1].mode, FileMode::CopyTree);
    assert_eq!(config.directories[1].ignore, [".DS_Store"]);
}

#[test]
fn a_file_entry_declaring_ignore_is_refused() {
    let toml = r#"
[[file]]
source = "zshrc"
target = "~/.zshrc"
ignore = ["*.pyc"]
"#;
    let err = parse_module_config_str(toml).expect_err("a [[file]] has no tree to filter");
    assert!(
        matches!(
            err,
            ConfigParseError::FileEntry(FileEntryError::FileIgnoreDeclared { .. })
        ),
        "got {err:?}"
    );
}

#[test]
fn a_whole_directory_symlink_declaring_ignore_is_refused() {
    let toml = r#"
[[directory]]
source = "mpv"
target = "~/.config/mpv"
ignore = ["*.log"]
"#;
    let err = parse_module_config_str(toml).expect_err("one atomic link filters nothing");
    assert!(
        matches!(
            err,
            ConfigParseError::FileEntry(FileEntryError::DirectorySymlinkIgnoreDeclared { .. })
        ),
        "got {err:?}"
    );
    assert!(
        err.to_string().contains("symlink-tree"),
        "the message must route the author to the mode that does filter, got: {err}"
    );
}

#[test]
fn a_module_level_patina_ignore_is_refused() {
    let toml = r#"
[patina]
ignore = ["*.pyc"]

[[directory]]
source = "scripts"
target = "~/bin"
mode = "symlink-tree"
"#;
    let err = parse_module_config_str(toml).expect_err("ignore is root-only");
    assert!(
        matches!(err, ConfigParseError::ModuleIgnoreList),
        "got {err:?}"
    );
}

#[test]
fn a_module_manifest_without_an_ignore_key_still_parses() {
    let toml = r#"
[patina]
root = true

[[directory]]
source = "scripts"
target = "~/bin"
mode = "symlink-tree"
"#;
    let config = parse_module_config_str(toml).expect("a [patina] table without ignore is fine");
    assert_eq!(config.directories.len(), 1);
}
