//! Integration tests for variable layers.

#![expect(
    clippy::expect_used,
    reason = "the root_manifest_with helper is a free fn at the integration-crate root, not inside a #[cfg(test)] module, so allow-expect-in-tests does not cover it; fixture setup panicking on failure is the intended test behaviour."
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

/// Declare one module that renders `{{ editor }}` into `~/.<name>-editor`,
/// with an optional `[variables]` table of its own.
fn editor_module(f: &Fixture, name: &str, editor: Option<&str>) {
    let variables = editor.map_or_else(String::new, |value| {
        format!("\n[variables]\neditor = \"{value}\"\n")
    });
    let module = f.module(
        name,
        &format!("[[file]]\nsource = \"editor.tmpl\"\ntarget = \"~/.{name}-editor\"\n{variables}"),
    );
    fs_err::write(module.join("editor.tmpl"), "editor = {{ editor }}\n").expect("write tmpl");
}

fn rendered_editor(f: &Fixture, name: &str) -> String {
    fs_err::read_to_string(f.home.join(format!(".{name}-editor"))).expect("target written")
}

#[test]
fn each_module_renders_under_its_own_variables_table() {
    let f = Fixture::new();
    root_manifest_with(&f, "[variables]\neditor = \"nvim\"\n");
    editor_module(&f, "alpha", Some("emacs"));
    editor_module(&f, "beta", Some("helix"));
    editor_module(&f, "gamma", None);

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        rendered_editor(&f, "alpha").contains("editor = emacs"),
        "alpha must render its own value, got: {}",
        rendered_editor(&f, "alpha")
    );
    assert!(
        rendered_editor(&f, "beta").contains("editor = helix"),
        "beta must render its own value, got: {}",
        rendered_editor(&f, "beta")
    );
    assert!(
        rendered_editor(&f, "gamma").contains("editor = nvim"),
        "a module with no [variables] table must fall through to the repo-shared value, got: {}",
        rendered_editor(&f, "gamma")
    );
}

#[test]
fn per_module_renders_converge_and_report_clean() {
    let f = Fixture::new();
    root_manifest_with(&f, "[variables]\neditor = \"nvim\"\n");
    editor_module(&f, "alpha", Some("emacs"));
    editor_module(&f, "beta", Some("helix"));
    editor_module(&f, "gamma", None);

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "the first apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let written: Vec<String> = ["alpha", "beta", "gamma"]
        .iter()
        .map(|name| rendered_editor(&f, name))
        .collect();

    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "the second apply must succeed; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let reread: Vec<String> = ["alpha", "beta", "gamma"]
        .iter()
        .map(|name| rendered_editor(&f, name))
        .collect();
    assert_eq!(
        written, reread,
        "re-applying must leave each module's rendered bytes alone"
    );
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("Already up to date"),
        "a target classified against one resolver and written from another replans \
         as drift forever; stdout: {}",
        String::from_utf8_lossy(&second.stdout)
    );

    let status = f.run(&["status", "--json"], &[]);
    assert_eq!(
        code(&status),
        0,
        "status must exit 0; stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&status.stdout)).expect("status json");
    assert_eq!(
        doc.get("drifted").and_then(serde_json::Value::as_u64),
        Some(0),
        "a target classified Unchanged against one resolver and written from another \
         would report drifted: {doc}"
    );
    assert_eq!(
        doc.get("clean").and_then(serde_json::Value::as_u64),
        Some(3),
        "every rendered target must report clean: {doc}"
    );
}

/// Declare a module whose `pre_apply` hook fails when its `when` holds, next
/// to a second module binding the same variable to the opposite value.
fn gated_hook_modules(f: &Fixture, gating: &str, trailing: &str) {
    f.module(
        "alpha",
        &format!(
            "[variables]\ngate = \"{gating}\"\n\n\
             [[hook]]\nevent = \"pre_apply\"\ncommand = \"exit 1\"\nwhen = \"gate == 'on'\"\n"
        ),
    );
    f.module("beta", &format!("[variables]\ngate = \"{trailing}\"\n"));
}

#[test]
fn a_hook_when_reads_its_own_module_binding() {
    let f = Fixture::new();
    gated_hook_modules(&f, "on", "off");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        2,
        "the hook's `when` must read alpha's `gate = on` and abort the apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_hook_when_ignores_a_later_module_binding() {
    let f = Fixture::new();
    gated_hook_modules(&f, "off", "on");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "beta's `gate = on` must not reach alpha's hook predicate; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
