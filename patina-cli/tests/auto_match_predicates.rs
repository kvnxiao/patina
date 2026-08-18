//! Integration tests for `auto_match_predicates`.
#![expect(
    clippy::expect_used,
    reason = "Integration tests use .expect() for fixture setup and assertions outside #[cfg(test)] modules; allow-expect-in-tests does not cover integration-crate roots."
)]

mod common;

use common::Fixture;
use common::code;

fn current_os_family() -> &'static str {
    std::env::consts::OS
}

fn write_root(f: &Fixture, body: &str) {
    fs_err::write(f.root.join("patina.toml"), body).expect("write root manifest");
}

#[test]
fn auto_match_rule_on_os_resolves_its_profile() {
    let f = Fixture::new();
    write_root(
        &f,
        &format!(
            "[patina]\nroot = true\n\n[[auto_match]]\nwhen = \"patina.os == '{}'\"\nprofile = \"p\"\n",
            current_os_family()
        ),
    );

    let out = f.apply(&["--json", "--yes"]);
    assert_eq!(
        code(&out),
        0,
        "an OS-matching auto_match rule must apply cleanly; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be a single JSON document");
    assert_eq!(
        doc.get("profile").and_then(serde_json::Value::as_str),
        Some("p"),
        "the auto_match rule must resolve profile `p`, got: {doc:?}"
    );
}

#[test]
fn file_inequality_predicate_materializes_target() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"copy\"\n\
         when = \"patina.os != 'definitely-not-this-os'\"\n",
    );
    fs_err::write(module.join("zshrc"), "export EDITOR=vim\n").expect("write source");

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "an inequality `when` that is true must apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.home.join(".zshrc").exists(),
        "the entry's target must be materialized when its `!=` predicate is true"
    );
}

#[test]
fn file_misspelled_builtin_fails_and_includes_the_variable() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"copy\"\n\
         when = \"patina.oss == 'windows'\"\n",
    );
    fs_err::write(module.join("zshrc"), "export EDITOR=vim\n").expect("write source");

    let out = f.apply(&["--yes"]);
    assert_ne!(
        code(&out),
        0,
        "a `when` referencing an undefined variable must fail the apply, not silently drop the entry"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("patina.oss"),
        "stderr must include the undefined variable `patina.oss`, got: {stderr}"
    );
    assert!(
        !f.home.join(".zshrc").exists(),
        "the entry's target must not be materialized when its `when` errors"
    );
}

#[test]
fn auto_match_referencing_patina_profile_fails_and_includes_it() {
    let f = Fixture::new();
    write_root(
        &f,
        "[patina]\nroot = true\n\n[[auto_match]]\nwhen = \"patina.profile == 'work'\"\nprofile = \"p\"\n",
    );

    let out = f.apply(&["--yes"]);
    assert_ne!(
        code(&out),
        0,
        "an auto_match `when` referencing the unresolved `patina.profile` must fail profile resolution"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("patina.profile"),
        "stderr must include the undefined variable `patina.profile`, got: {stderr}"
    );
}
