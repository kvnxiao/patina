#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

//! One shared `MiniJinja` engine evaluates every `when` site, including
//! `[[auto_match]]` profile rules.
//!
//! These end-to-end tests drive `PATINA_REPO=<tempdir> patina apply` over
//! fixture repos and assert these behaviours:
//!
//! - An `[[auto_match]]` rule matching the host's `patina.os` resolves its
//!   profile.
//! - A `[[file]]` `when` using an operator other than `==`, here `!=`,
//!   evaluates true and materializes its target.
//! - A `[[file]]` `when` misspelling a built-in (`patina.oss`) fails the apply
//!   with a typed error that includes the variable.
//! - An `[[auto_match]]` `when` referencing `patina.profile` (unresolved during
//!   profile resolution) fails with a typed undefined-variable error that
//!   includes it, rather than silently failing to match.

mod common;

use common::Fixture;
use common::code;

/// The OS family string the engine's `patina.os` built-in resolves to on
/// this host (`"macos"`, `"linux"`, or `"windows"`). `std::env::consts::OS`
/// matches the value `normalized_os` returns on the three supported
/// platforms, so a `when` built from it is deterministically true here.
fn current_os_family() -> &'static str {
    std::env::consts::OS
}

/// Overwrite the fixture's root manifest body (replacing the default
/// `[patina]\nroot = true\n`). Used to declare `[[auto_match]]` rules,
/// which only the root manifest carries.
fn write_root(f: &Fixture, body: &str) {
    fs_err::write(f.root.join("patina.toml"), body).expect("write root manifest");
}

#[test]
fn auto_match_rule_on_os_resolves_its_profile() {
    // An `[[auto_match]]` rule whose `when` matches the host's
    // `patina.os` selects profile `p`. The `--json` envelope's `profile`
    // field is the observable resolution result.
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
    // A `[[file]]` `when` using `!=` evaluates true and materializes the
    // target. An evaluator narrowed back to single equality would raise
    // `UnsupportedPredicate` on this entry.
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
    // A `[[file]]` `when` misspelling `patina.os` as `patina.oss` accesses
    // an undefined variable.
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
    // An `[[auto_match]]` `when` referencing `patina.profile` accesses a
    // variable that profile resolution itself computes and has not yet
    // resolved, rather than the rule silently failing to match.
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
