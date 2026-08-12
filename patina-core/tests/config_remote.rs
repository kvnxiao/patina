//! The root `[[remote]]` registry, per-entry remote selection, and the trust
//! boundary the pair draws.
//!
//! A remote is declared once in the root manifest and named by any entry that
//! wants its bytes. The parse-level consequences are that each declaration is
//! validated and named, that a module manifest may not declare a remote
//! of its own, and that an entry naming one gets no implicit `.tmpl` template
//! render — third-party bytes are never handed to `MiniJinja` — while its local
//! neighbours in the same manifest keep it. See `docs/REMOTE_SOURCES.md` "The
//! remote registry" and "Trust boundaries".

use patina_core::FileMode;
use patina_core::parse_module_config_str;
use patina_core::parse_root_config_str;
use std::time::Duration;

#[test]
fn a_remote_declaration_is_parsed_with_its_derived_name_ref_and_min_age() {
    let config = parse_root_config_str(
        "[patina]\nroot = true\n\n\
         [[remote]]\n\
         url = \"https://github.com/blader/humanizer.git\"\n\
         ref = \"main\"\n\
         min_age = \"0s\"\n",
    )
    .expect("a root registry parses");

    let remote = config.remotes.first().expect("one declared remote");
    assert_eq!(remote.name, "humanizer");
    assert_eq!(remote.url, "https://github.com/blader/humanizer.git");
    assert_eq!(remote.git_ref.as_deref(), Some("main"));
    assert_eq!(remote.min_age, Some(Duration::ZERO));
}

#[test]
fn a_declaration_without_ref_or_min_age_defers_both() {
    let config = parse_root_config_str("[[remote]]\nurl = \"git@example.invalid:r.git\"\n")
        .expect("a minimal declaration parses");
    let remote = config.remotes.first().expect("one declared remote");
    assert_eq!(remote.name, "r", "the scp-like form names the last segment");
    assert_eq!(remote.git_ref, None, "no `ref` means the default branch");
    assert_eq!(
        remote.min_age, None,
        "no per-remote `min_age` defers to `[patina] remote_min_age`, then to the default"
    );
}

#[test]
fn a_written_name_wins_over_the_url() {
    let config = parse_root_config_str(
        "[[remote]]\nname = \"agents\"\nurl = \"https://github.com/blader/humanizer.git\"\n",
    )
    .expect("an explicitly named declaration parses");
    assert_eq!(
        config.remotes.first().expect("one remote").name,
        "agents",
        "a written name must not be overridden by the derived one"
    );
}

#[test]
fn a_url_with_no_derivable_name_says_to_write_one() {
    let err = parse_root_config_str("[[remote]]\nurl = \"https://example.invalid/repo?ref=x\"\n")
        .expect_err("a URL with no legal last segment must be rejected");
    assert!(
        err.to_string().contains("name = "),
        "the message must point at the key that fixes it, got: {err}"
    );
}

#[test]
fn two_declarations_may_not_share_a_name() {
    let err = parse_root_config_str(
        "[[remote]]\nurl = \"https://a.invalid/humanizer\"\n\n\
         [[remote]]\nurl = \"https://b.invalid/humanizer.git\"\n",
    )
    .expect_err("two remotes may not answer to one name");
    assert!(
        err.to_string().contains("humanizer"),
        "the message must name the collision, got: {err}"
    );
}

#[test]
fn names_differing_only_in_case_are_one_name() {
    parse_root_config_str(
        "[[remote]]\nname = \"Humanizer\"\nurl = \"https://a.invalid/r\"\n\n\
         [[remote]]\nname = \"humanizer\"\nurl = \"https://b.invalid/r\"\n",
    )
    .expect_err("case-only-differing names must collide");
}

#[test]
fn a_declaration_with_an_empty_url_is_rejected() {
    parse_root_config_str("[[remote]]\nurl = \"\"\n").expect_err("a remote needs a URL");
}

#[test]
fn a_declaration_missing_its_url_is_rejected() {
    parse_root_config_str("[[remote]]\nref = \"main\"\n")
        .expect_err("`url` is required in a `[[remote]]` table");
}

#[test]
fn a_declaration_with_a_malformed_min_age_is_rejected() {
    parse_root_config_str(
        "[[remote]]\nurl = \"https://example.invalid/r\"\nmin_age = \"3 fortnights\"\n",
    )
    .expect_err("a malformed per-remote min_age must be rejected");
}

#[test]
fn a_module_level_remote_table_points_at_the_root_manifest() {
    let err = parse_module_config_str(
        "[remote]\nurl = \"https://github.com/blader/humanizer\"\n\n\
         [[file]]\nsource = \"SKILL.md\"\ntarget = \"~/.claude/skills/humanizer/SKILL.md\"\n",
    )
    .expect_err("a module may not declare a remote of its own");
    let message = err.to_string();
    assert!(
        message.contains("root patina.toml") && message.contains("[[remote]]"),
        "the message must say where the declaration belongs now, got: {message}"
    );
}

#[test]
fn an_entry_with_no_remote_key_is_local() {
    let config = parse_module_config_str("[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\n")
        .expect("a local entry parses");
    assert_eq!(config.files.first().expect("one entry").remote, None);
}

#[test]
fn an_entry_naming_a_remote_carries_the_name() {
    let config = parse_module_config_str(
        "[[file]]\nsource = \"SKILL.md\"\nremote = \"humanizer\"\n\
         target = \"~/.claude/skills/humanizer/SKILL.md\"\n",
    )
    .expect("a remote-sourced entry parses");
    assert_eq!(
        config
            .files
            .first()
            .expect("one entry")
            .remote
            .as_deref()
            .expect("the entry names a remote"),
        "humanizer"
    );
}

#[test]
fn an_entry_declaring_a_blank_remote_is_rejected() {
    // Silently reading `remote = ""` as "local" would resolve the source
    // against the wrong tree; omitting the key is how an entry stays local.
    parse_module_config_str("[[file]]\nsource = \"a\"\nremote = \"  \"\ntarget = \"~/.a\"\n")
        .expect_err("a blank `remote` must be rejected");
}

#[test]
fn the_template_policy_is_per_entry_within_one_module() {
    // The two entries sit in one manifest: the local `.tmpl` renders, the
    // remote-sourced one is plain bytes under its declared (here defaulted)
    // mode. A module-wide policy could not produce both.
    let config = parse_module_config_str(
        "[[file]]\nsource = \"gitconfig.tmpl\"\ntarget = \"~/.gitconfig\"\n\n\
         [[file]]\nsource = \"prompts/agent.tmpl\"\nremote = \"humanizer\"\n\
         target = \"~/.agent.tmpl\"\n",
    )
    .expect("a mixed module parses");
    let modes: Vec<FileMode> = config.files.iter().map(|entry| entry.mode).collect();
    assert_eq!(
        modes,
        [FileMode::TemplateRender, FileMode::Symlink],
        "the local `.tmpl` must render and the remote-sourced one must fall to its declared mode"
    );
}

#[test]
fn a_remote_sourced_tmpl_may_declare_an_explicit_mode() {
    // The implicit-template rule forbids `mode` beside a local `.tmpl` source.
    // With no implicit render there is nothing for it to conflict with.
    let config = parse_module_config_str(
        "[[file]]\nsource = \"a.tmpl\"\nremote = \"r\"\ntarget = \"~/.a\"\nmode = \"copy\"\n",
    )
    .expect("a remote-sourced `.tmpl` with an explicit mode parses");
    assert_eq!(
        config.files.first().expect("one entry").mode,
        FileMode::Copy
    );
}

#[test]
fn a_remote_sourced_tmpl_directory_is_an_ordinary_directory_name() {
    let config = parse_module_config_str(
        "[[directory]]\nsource = \"templates.tmpl\"\nremote = \"r\"\ntarget = \"~/.templates\"\n",
    )
    .expect("a remote-sourced `.tmpl` directory parses");
    assert_eq!(
        config.directories.first().expect("one entry").mode,
        FileMode::SymlinkDir
    );
}

#[test]
fn a_local_tmpl_source_with_an_explicit_mode_is_still_rejected() {
    parse_module_config_str("[[file]]\nsource = \"a.tmpl\"\ntarget = \"~/.a\"\nmode = \"copy\"\n")
        .expect_err("the implicit-template rule still applies to local sources");
}

#[test]
fn a_local_tmpl_directory_source_is_still_rejected() {
    parse_module_config_str("[[directory]]\nsource = \"t.tmpl\"\ntarget = \"~/.t\"\n")
        .expect_err("template render stays file-only for local sources");
}
