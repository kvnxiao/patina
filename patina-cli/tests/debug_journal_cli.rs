#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixtures; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for the `patina debug journal <path>` CLI surface
//! (REQ-020, CHK-034 / CHK-050).
//!
//! The subcommand decodes a binary `<ts>.plan` file and renders it. A
//! committed apply deletes its plan file at commit, so these tests write a
//! plan file directly through the public `Plan::encode` API (the same
//! bytes the engine fsyncs before mutating) and point the CLI at it. That
//! exercises the full binary path the scenario cares about: read the file,
//! check the version envelope, decode the body, render to stdout.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::Disposition;
use patina_core::FILE_MAJOR_VERSION;
use patina_core::Plan;
use patina_core::PlannedOperation;
use std::process::Command;
use std::process::Output;
use tempfile::TempDir;

/// Write `bytes` to `<dir>/<ts>.plan` and return the path.
fn write_plan(dir: &Utf8Path, ts: &str, bytes: &[u8]) -> Utf8PathBuf {
    let path = dir.join(format!("{ts}.plan"));
    fs_err::write(&path, bytes).expect("write plan file");
    path
}

fn invoke(args: &[&str]) -> Output {
    let bin = env!("CARGO_BIN_EXE_patina");
    Command::new(bin).args(args).output().expect("spawn patina")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

#[test]
fn decodes_a_plan_and_prints_its_modes_and_targets() {
    // CHK-034: a plan declaring symlink + copy operations renders with the
    // matching mode words and at least one absolute target on stdout, exit 0.
    let temp = TempDir::new().expect("tempdir");
    let dir = Utf8Path::from_path(temp.path()).expect("utf8 tempdir");
    let plan = Plan::new(vec![
        PlannedOperation::symlink("zsh/zshrc", "/home/u/.zshrc", Disposition::Create),
        PlannedOperation::copy("git/gitconfig", "/home/u/.gitconfig", Disposition::Create),
    ]);
    let path = write_plan(dir, "20260528T120000Z", &plan.encode().expect("encode"));

    let out = invoke(&["debug", "journal", path.as_str()]);
    assert_eq!(
        code(&out),
        0,
        "debug journal must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("symlink") || stdout.contains("copy"),
        "stdout names a declared mode; got: {stdout}"
    );
    assert!(
        stdout.contains("/home/u/.zshrc") || stdout.contains("/home/u/.gitconfig"),
        "stdout names an absolute target; got: {stdout}"
    );
}

#[test]
fn missing_path_exits_one_and_names_the_path_on_stderr() {
    // CHK (REQ-020 done-when): a non-existent path -> exit 1, path on stderr.
    let out = invoke(&["debug", "journal", "/nonexistent/path.plan"]);
    assert_eq!(code(&out), 1, "missing path must exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("/nonexistent/path.plan"),
        "stderr names the missing path; got: {stderr}"
    );
}

#[test]
fn newer_version_envelope_exits_one_and_names_both_versions() {
    // CHK (REQ-020 done-when): a plan whose envelope major is u16::MAX is
    // refused by a binary whose supported major is 1 -> exit 1, both
    // versions plus the word "version" on stderr.
    let temp = TempDir::new().expect("tempdir");
    let dir = Utf8Path::from_path(temp.path()).expect("utf8 tempdir");
    let plan = Plan::new(vec![PlannedOperation::copy(
        "a",
        "/home/u/.a",
        Disposition::Create,
    )]);
    let mut bytes = plan.encode().expect("encode");
    bytes
        .get_mut(..2)
        .expect("envelope")
        .copy_from_slice(&u16::MAX.to_le_bytes());
    let path = write_plan(dir, "20260528T120000Z", &bytes);

    let out = invoke(&["debug", "journal", path.as_str()]);
    assert_eq!(code(&out), 1, "newer-major plan must exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("65535"), "names u16::MAX; got: {stderr}");
    assert!(
        stderr.contains(&FILE_MAJOR_VERSION.to_string()),
        "names the supported major; got: {stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("version"),
        "names the version dimension; got: {stderr}"
    );
}

#[test]
fn debug_help_names_journal_subcommand() {
    // CHK-050: `patina debug --help` names `journal` and exits 0.
    let out = invoke(&["debug", "--help"]);
    assert_eq!(
        code(&out),
        0,
        "debug --help must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("journal"),
        "debug --help names the journal subcommand; got: {stdout}"
    );
}
