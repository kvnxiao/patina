//! Integration tests for clippy disallowed macros.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde_json::Value;
use std::process::Command;
use tempfile::TempDir;

fn workspace_clippy_toml() -> Utf8PathBuf {
    let manifest_dir = Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .expect("patina package has a workspace-root parent");
    root.join("clippy.toml")
}

fn scratch_crate(temp: &TempDir, body: &str) -> Utf8PathBuf {
    let root = Utf8Path::from_path(temp.path())
        .expect("utf8 temp path")
        .to_owned();
    fs_err::create_dir_all(root.join("src")).expect("mkdir src");
    fs_err::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"scratch\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .expect("write Cargo.toml");
    fs_err::write(root.join("src/plan.rs"), body).expect("write plan.rs");
    fs_err::write(root.join("src/lib.rs"), "pub mod plan;\n").expect("write lib.rs");
    let clippy_toml = fs_err::read_to_string(workspace_clippy_toml()).expect("read clippy.toml");
    fs_err::write(root.join("clippy.toml"), clippy_toml).expect("write scratch clippy.toml");
    root
}

fn run_clippy(crate_root: &Utf8Path) -> (bool, Vec<String>) {
    let output = Command::new(env!("CARGO"))
        .args(["clippy", "--message-format=json", "--", "-D", "warnings"])
        .current_dir(crate_root)
        .env("CARGO_TARGET_DIR", crate_root.join("target").as_str())
        .output()
        .expect("spawn cargo clippy");

    let stdout = String::from_utf8(output.stdout).expect("clippy stdout is utf8");
    let mut files = Vec::new();
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
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
fn each_raw_print_macro_outside_output_module_fails_clippy_for_the_file() {
    let temp = TempDir::new().expect("tempdir");
    let crate_root = scratch_crate(
        &temp,
        "pub fn shout() {\n    println!(\"a\");\n    eprintln!(\"b\");\n    print!(\"c\");\n    \
         eprint!(\"d\");\n}\n",
    );

    let (success, files) = run_clippy(&crate_root);

    assert!(
        !success,
        "clippy must reject the raw print macros outside the output module"
    );
    let in_plan = files
        .iter()
        .filter(|f| f.replace('\\', "/").ends_with("src/plan.rs"))
        .count();
    assert_eq!(
        in_plan, 4,
        "each of println!, eprintln!, print!, and eprint! must raise its own \
         disallowed_macros diagnostic for plan.rs; got {files:?}"
    );
}

#[test]
fn tracing_macro_and_scoped_expect_stay_clean() {
    let temp = TempDir::new().expect("tempdir");
    let crate_root = scratch_crate(
        &temp,
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
