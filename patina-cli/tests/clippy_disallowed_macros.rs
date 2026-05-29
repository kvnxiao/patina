#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration test for the workspace `disallowed-macros` clippy gate
//! (REQ-026, CHK-053 / CHK-054).
//!
//! REQ-026 makes the `output::Reporter` abstraction the only sanctioned site
//! for user-facing prints: `println!`, `eprintln!`, `print!`, and `eprint!`
//! are denied everywhere else via the workspace `clippy.toml`'s
//! `disallowed-macros` list. This suite proves two halves of that contract:
//!
//! 1. The contract is *declared* — the real workspace `clippy.toml` lists all
//!    four macros under `disallowed-macros` (CHK-053's config half).
//! 2. The contract *bites* — a fresh `println!("hi")` in a non-`output` file
//!    makes clippy fail with a `clippy::disallowed_macros` diagnostic that
//!    names the offending file, while the `tracing`-style macros and a
//!    module-scoped `#[expect(clippy::disallowed_macros, ...)]` carve-out stay
//!    clean (CHK-054 and its sibling scenarios).
//!
//! Rather than mutate the checked-in source tree (which would race with other
//! parallel tests and risk leaving the tree dirty on failure), the "bites"
//! half compiles a throwaway crate in a tempdir that reuses the *real*
//! workspace `clippy.toml` — the artifact under test — so the assertion
//! exercises the same config CI enforces.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde_json::Value;
use std::process::Command;
use tempfile::TempDir;

/// Absolute path to the workspace `clippy.toml` — the artifact under test.
/// `CARGO_MANIFEST_DIR` is the `patina-cli` crate dir; the workspace root is
/// its parent.
fn workspace_clippy_toml() -> Utf8PathBuf {
    let manifest_dir = Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .expect("patina-cli has a workspace-root parent");
    root.join("clippy.toml")
}

#[test]
fn clippy_toml_lists_all_four_print_macros() {
    // CHK-053 (config half): the workspace clippy.toml must declare every raw
    // print macro under `disallowed-macros`. Parse the real file as TOML and
    // assert each entry is present as a literal string. Missing any one would
    // leave a hole an `eprint!` (etc.) could slip through.
    let path = workspace_clippy_toml();
    let body = fs_err::read_to_string(&path).expect("read workspace clippy.toml");
    let parsed: toml::Value = body.parse().expect("clippy.toml parses as TOML");

    let entries: Vec<&str> = parsed
        .get("disallowed-macros")
        .and_then(toml::Value::as_array)
        .expect("clippy.toml has a disallowed-macros array")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();

    for expected in ["std::println", "std::eprintln", "std::print", "std::eprint"] {
        assert!(
            entries.contains(&expected),
            "disallowed-macros is missing {expected}; found {entries:?}"
        );
    }
}

/// A throwaway single-file crate that reuses the workspace `clippy.toml`, with
/// `body` as the entire contents of `src/plan.rs`. Returns the crate root.
fn scratch_crate(temp: &TempDir, body: &str) -> Utf8PathBuf {
    let root = Utf8Path::from_path(temp.path())
        .expect("utf8 temp path")
        .to_owned();
    fs_err::create_dir_all(root.join("src")).expect("mkdir src");
    fs_err::write(
        root.join("Cargo.toml"),
        // A leaf crate, deliberately not part of the patina workspace
        // (`[workspace]` makes it its own root), so clippy resolves the
        // clippy.toml we copy beside it rather than the repo's.
        "[package]\nname = \"scratch\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .expect("write Cargo.toml");
    // Name the file `plan.rs` to mirror the SPEC scenario's
    // `patina-core/src/plan.rs`; the assertion checks the diagnostic names it.
    fs_err::write(root.join("src/plan.rs"), body).expect("write plan.rs");
    // `pub mod` so the fixture's `pub fn`s are reachable crate API; a private
    // `mod` would make them dead code and `-D warnings` would fail for that
    // reason instead of the one under test.
    fs_err::write(root.join("src/lib.rs"), "pub mod plan;\n").expect("write lib.rs");
    // Reuse the real workspace clippy.toml verbatim — this is the artifact
    // whose behaviour we are asserting.
    let clippy_toml = fs_err::read_to_string(workspace_clippy_toml()).expect("read clippy.toml");
    fs_err::write(root.join("clippy.toml"), clippy_toml).expect("write scratch clippy.toml");
    root
}

/// Run `cargo clippy --message-format=json -- -D warnings` in `crate_root` and
/// return `(success, disallowed_macros_files)` where the second element holds
/// the `file_name` of every `clippy::disallowed_macros` diagnostic span.
fn run_clippy(crate_root: &Utf8Path) -> (bool, Vec<String>) {
    let output = Command::new(env!("CARGO"))
        .args(["clippy", "--message-format=json", "--", "-D", "warnings"])
        .current_dir(crate_root)
        // A fresh target dir under the tempdir keeps the run hermetic and lets
        // the OS reclaim it with the tempdir.
        .env("CARGO_TARGET_DIR", crate_root.join("target").as_str())
        .output()
        .expect("spawn cargo clippy");

    let stdout = String::from_utf8(output.stdout).expect("clippy stdout is utf8");
    let mut files = Vec::new();
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // Compiler messages carry `reason == "compiler-message"` and a nested
        // `message.code.code` naming the lint that fired.
        if value.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let lint = message
            .get("code")
            .and_then(|code| code.get("code"))
            .and_then(Value::as_str);
        if lint != Some("clippy::disallowed_macros") {
            continue;
        }
        let spans = message.get("spans").and_then(Value::as_array);
        for span in spans.into_iter().flatten() {
            if let Some(name) = span.get("file_name").and_then(Value::as_str) {
                files.push(name.to_owned());
            }
        }
    }
    (output.status.success(), files)
}

#[test]
fn raw_println_outside_output_module_fails_clippy_naming_the_file() {
    // CHK-054: a fresh `println!("hi")` in a non-`output` file (here `plan.rs`,
    // mirroring the SPEC scenario's `patina-core/src/plan.rs`) makes clippy
    // exit non-zero with a `disallowed_macros` diagnostic that names the file.
    let temp = TempDir::new().expect("tempdir");
    let crate_root = scratch_crate(&temp, "pub fn shout() {\n    println!(\"hi\");\n}\n");

    let (success, files) = run_clippy(&crate_root);

    assert!(
        !success,
        "clippy must reject a raw println! outside the output module"
    );
    assert!(
        files
            .iter()
            .any(|f| f.replace('\\', "/").ends_with("src/plan.rs")),
        "the disallowed_macros diagnostic must name plan.rs; named {files:?}"
    );
}

#[test]
fn tracing_macro_and_scoped_expect_stay_clean() {
    // Sibling scenarios: replacing the offending line with a non-listed macro
    // (a `tracing`-style `info!`, stubbed locally so the scratch crate needs no
    // dependency) does not fire the lint, and a module-scoped
    // `#[expect(clippy::disallowed_macros, ...)]` carve-out — the same shape
    // the `output` module / lock_helper example use — suppresses it cleanly
    // without leaving an unfulfilled-expectation warning.
    let temp = TempDir::new().expect("tempdir");
    let crate_root = scratch_crate(
        &temp,
        // `info!` is a local macro_rules stub: the point is that a macro NOT in
        // the disallowed list never fires the lint. The second fn carries the
        // scoped expect over a genuine `println!`, so the expectation is
        // fulfilled (no unfulfilled_lint_expectations warning under -D warnings).
        "macro_rules! info {\n    ($($t:tt)*) => {{ let _ = format!($($t)*); }};\n}\n\
         pub fn logged() {\n    info!(\"hi\");\n}\n\n\
         #[expect(clippy::disallowed_macros, reason = \"carve-out under test\")]\n\
         pub fn carved() {\n    println!(\"hi\");\n}\n",
    );

    let (success, files) = run_clippy(&crate_root);

    assert!(
        success,
        "tracing-style macros and a scoped #[expect] carve-out must pass clippy; \
         unexpected disallowed_macros spans: {files:?}"
    );
    assert!(
        files.is_empty(),
        "no disallowed_macros diagnostic should survive the carve-out; got {files:?}"
    );
}
