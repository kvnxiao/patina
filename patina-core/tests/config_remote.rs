//! The `[remote]` module table and the trust boundaries it draws.
//!
//! A `[remote]` table makes a module remote-backed. The parse-level
//! consequences are that the table itself is validated and that every entry in
//! the module loses the implicit `.tmpl` template render: third-party bytes
//! are never handed to `MiniJinja`. See `docs/REMOTE_SOURCES.md`
//! "Remote-backed modules" and "Trust boundaries".

use patina_core::FileMode;
use patina_core::parse_module_config_str;
use std::time::Duration;

#[test]
fn a_remote_table_is_parsed_with_its_ref_and_min_age() {
    let config = parse_module_config_str(
        "[remote]\n\
         url = \"https://github.com/blader/humanizer\"\n\
         ref = \"main\"\n\
         min_age = \"0s\"\n\n\
         [[directory]]\n\
         source = \"skills/humanizer\"\n\
         target = \"~/.claude/skills/humanizer\"\n\
         mode = \"copy\"\n",
    )
    .expect("a remote-backed module parses");

    let remote = config.remote.expect("the module is remote-backed");
    assert_eq!(remote.url, "https://github.com/blader/humanizer");
    assert_eq!(remote.git_ref.as_deref(), Some("main"));
    assert_eq!(remote.min_age, Some(Duration::from_secs(0)));
    assert_eq!(
        config
            .directories
            .first()
            .expect("one directory entry")
            .mode,
        FileMode::CopyTree
    );
}

#[test]
fn a_remote_table_without_ref_or_min_age_defers_both() {
    let config = parse_module_config_str("[remote]\nurl = \"git@example.invalid:r.git\"\n")
        .expect("a minimal remote table parses");
    let remote = config.remote.expect("the module is remote-backed");
    assert_eq!(remote.git_ref, None, "no `ref` means the default branch");
    assert_eq!(
        remote.min_age, None,
        "no per-remote `min_age` defers to the root table, then the shipped default"
    );
}

#[test]
fn a_module_without_a_remote_table_is_local() {
    let config = parse_module_config_str("[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\n")
        .expect("a local module parses");
    assert_eq!(config.remote, None);
}

#[test]
fn a_remote_tmpl_source_is_plain_bytes_not_a_template() {
    // The implicit render would hand third-party `{{ }}` to strict-undefined
    // MiniJinja. In a remote-backed module the suffix is just a filename.
    let config = parse_module_config_str(
        "[remote]\nurl = \"https://example.invalid/r\"\n\n\
         [[file]]\nsource = \"prompts/agent.tmpl\"\ntarget = \"~/.agent.tmpl\"\n",
    )
    .expect("a remote `.tmpl` source parses");
    assert_eq!(
        config.files.first().expect("one file entry").mode,
        FileMode::Symlink,
        "a remote `.tmpl` source must fall to the declared (here defaulted) mode"
    );
}

#[test]
fn a_remote_tmpl_source_may_declare_an_explicit_mode() {
    // The implicit-template rule forbids `mode` beside a local `.tmpl` source.
    // With no implicit render there is nothing for it to conflict with.
    let config = parse_module_config_str(
        "[remote]\nurl = \"https://example.invalid/r\"\n\n\
         [[file]]\nsource = \"a.tmpl\"\ntarget = \"~/.a\"\nmode = \"copy\"\n",
    )
    .expect("a remote `.tmpl` source with an explicit mode parses");
    assert_eq!(
        config.files.first().expect("one file entry").mode,
        FileMode::Copy
    );
}

#[test]
fn a_remote_tmpl_directory_source_is_an_ordinary_directory_name() {
    let config = parse_module_config_str(
        "[remote]\nurl = \"https://example.invalid/r\"\n\n\
         [[directory]]\nsource = \"templates.tmpl\"\ntarget = \"~/.templates\"\n",
    )
    .expect("a remote `.tmpl` directory source parses");
    assert_eq!(
        config.directories.first().expect("one entry").mode,
        FileMode::SymlinkDir
    );
}

#[test]
fn a_local_tmpl_source_still_renders() {
    // The guard for the two tests above: without a `[remote]` table the same
    // source must resolve to the implicit template render, so the policy switch
    // is doing real work rather than disabling the feature outright.
    let config = parse_module_config_str(
        "[[file]]\nsource = \"gitconfig.tmpl\"\ntarget = \"~/.gitconfig\"\n",
    )
    .expect("a local `.tmpl` source parses");
    assert_eq!(
        config.files.first().expect("one file entry").mode,
        FileMode::TemplateRender
    );
}

#[test]
fn a_local_tmpl_source_with_an_explicit_mode_is_still_rejected() {
    parse_module_config_str("[[file]]\nsource = \"a.tmpl\"\ntarget = \"~/.a\"\nmode = \"copy\"\n")
        .expect_err("the implicit-template rule still applies to local sources");
}

#[test]
fn a_remote_table_with_an_empty_url_is_rejected() {
    parse_module_config_str("[remote]\nurl = \"\"\n")
        .expect_err("a remote-backed module needs a URL");
}

#[test]
fn a_remote_table_with_a_malformed_min_age_is_rejected() {
    parse_module_config_str(
        "[remote]\nurl = \"https://example.invalid/r\"\nmin_age = \"3 fortnights\"\n",
    )
    .expect_err("a malformed per-remote min_age must be rejected");
}

#[test]
fn a_remote_table_missing_its_url_is_rejected() {
    parse_module_config_str("[remote]\nref = \"main\"\n")
        .expect_err("`url` is required in a `[remote]` table");
}
