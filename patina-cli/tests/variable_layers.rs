//! Integration tests for `variable_layers`.
#![expect(
    clippy::expect_used,
    reason = "root_manifest_with runs outside #[cfg(test)] modules; allow-expect-in-tests does not cover its fixture setup."
)]

mod common;

use common::Fixture;
use common::code;

fn root_manifest_with(f: &Fixture, trailing: &str) {
    let body = format!("[patina]\nroot = true\n\n{trailing}");
    fs_err::write(f.root.join("patina.toml"), body).expect("rewrite root manifest");
}

#[test]
fn root_variable_renders_into_module_template() {
    let f = Fixture::new();
    root_manifest_with(&f, "[variables]\neditor = \"nvim\"\n");
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"editor.tmpl\"\ntarget = \"~/.editor\"\n",
    );
    fs_err::write(module.join("editor.tmpl"), "editor = {{ editor }}\n").expect("write tmpl");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rendered = fs_err::read_to_string(f.home.join(".editor")).expect("target written");
    assert!(
        rendered.contains("editor = nvim"),
        "root [variables] value must render into the target, got: {rendered}"
    );
}

#[test]
fn active_profile_variable_shadows_repo_shared() {
    let f = Fixture::new();
    root_manifest_with(
        &f,
        "[variables]\neditor = \"nvim\"\n\n[profiles.work.variables]\neditor = \"code\"\n",
    );
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"editor.tmpl\"\ntarget = \"~/.editor\"\n",
    );
    fs_err::write(module.join("editor.tmpl"), "editor = {{ editor }}\n").expect("write tmpl");

    let out = f.apply_with_env(&["--yes"], &[("PATINA_PROFILE", "work")]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rendered = fs_err::read_to_string(f.home.join(".editor")).expect("target written");
    assert!(
        rendered.contains("editor = code"),
        "active profile value must shadow the repo-shared value, got: {rendered}"
    );
}

#[test]
fn per_module_variable_beats_repo_shared() {
    let f = Fixture::new();
    root_manifest_with(&f, "[variables]\neditor = \"nvim\"\n");
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"editor.tmpl\"\ntarget = \"~/.editor\"\n\n\
         [variables]\neditor = \"emacs\"\n",
    );
    fs_err::write(module.join("editor.tmpl"), "editor = {{ editor }}\n").expect("write tmpl");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rendered = fs_err::read_to_string(f.home.join(".editor")).expect("target written");
    assert!(
        rendered.contains("editor = emacs"),
        "per-module value must beat the repo-shared value, got: {rendered}"
    );
}

#[test]
fn no_profile_selects_no_per_profile_table() {
    let f = Fixture::new();
    root_manifest_with(
        &f,
        "[variables]\neditor = \"nvim\"\n\n[profiles.work.variables]\neditor = \"code\"\n",
    );
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"editor.tmpl\"\ntarget = \"~/.editor\"\n",
    );
    fs_err::write(module.join("editor.tmpl"), "editor = {{ editor }}\n").expect("write tmpl");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rendered = fs_err::read_to_string(f.home.join(".editor")).expect("target written");
    assert!(
        rendered.contains("editor = nvim"),
        "with no active profile the repo-shared value must render, got: {rendered}"
    );
}
