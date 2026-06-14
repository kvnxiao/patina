#![expect(
    clippy::expect_used,
    reason = "the root_manifest_with helper is a free fn at the integration-crate root, not inside a #[cfg(test)] module, so allow-expect-in-tests does not cover it; fixture setup panicking on failure is the intended test behaviour."
)]

//! Apply planning populates the repo-shared and
//! active-profile variable layers.
//!
//! Each test drives `PATINA_REPO=<tempdir> patina apply --yes` over a
//! fixture repo whose root `patina.toml` declares `[variables]` (and, for
//! the profile case, `[profiles.<name>.variables]`), and asserts the value
//! that renders into a module's `.tmpl` target. These are the only sites
//! that exercise the two layers `plan()` previously omitted; the per-module
//! and CLI layers are covered by `apply_cli.rs`.

mod common;

use common::Fixture;
use common::code;

/// Overwrite the fixture's root manifest with one that keeps the root
/// marker and adds the given trailing TOML (variable / profile tables).
fn root_manifest_with(f: &Fixture, trailing: &str) {
    let body = format!("[patina]\nroot = true\n\n{trailing}");
    fs_err::write(f.root.join("patina.toml"), body).expect("rewrite root manifest");
}

#[test]
fn root_variable_renders_into_module_template() {
    // A variable declared only in the root `[variables]` table
    // resolves inside a module's `.tmpl` template.
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
    // With profile `work` active, a key present in both the root
    // `[variables]` table and `[profiles.work.variables]` resolves to the
    // profile's value (per-profile shadows repo-shared).
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
    // A key present in both the root `[variables]` table and a module's
    // `[variables]` table resolves to the module value (per-module beats
    // repo-shared), unchanged from the documented precedence order.
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
    // The no-profile fallback (empty profile name) selects no per-profile
    // table: a `[profiles.work.variables]` override is inert when `work` is
    // not the active profile, so the repo-shared value renders.
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

    // Fixture::run/apply already env_remove("PATINA_PROFILE"), so no profile
    // is active here.
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
