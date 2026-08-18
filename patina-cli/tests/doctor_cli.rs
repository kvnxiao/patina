//! Integration tests for doctor cli.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;
use patina_core::canonicalize_path;
use patina_core::is_unc_path;

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout is utf8")
}

#[test]
fn missing_default_repo_reports_info_finding_and_exits_zero() {
    let fx = Fixture::new();
    let out = fx.run(&["doctor", "--json"], &[]);
    assert_eq!(
        code(&out),
        0,
        "doctor with only an info finding must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let doc: serde_json::Value =
        serde_json::from_str(stdout(&out).trim()).expect("doctor --json emits one JSON document");
    let findings = doc
        .get("findings")
        .and_then(serde_json::Value::as_array)
        .expect("findings array present");
    let note = findings
        .iter()
        .find(|f| f.get("code").and_then(serde_json::Value::as_str) == Some("DOC-NO-DEFAULT-REPO"))
        .expect("DOC-NO-DEFAULT-REPO finding present");
    assert_eq!(
        note.get("level").and_then(serde_json::Value::as_str),
        Some("info"),
        "the missing-default-repo finding is info, not warning"
    );

    let message = note
        .get("message")
        .and_then(serde_json::Value::as_str)
        .expect("message field present");
    assert!(
        message.contains("patina doctor --fix"),
        "with a resolved repository the advice must be `patina doctor --fix`, got: {message}"
    );
}

#[test]
fn doctor_json_is_byte_identical_across_runs() {
    let fx = Fixture::new();
    let first = fx.run(&["doctor", "--json"], &[]);
    let second = fx.run(&["doctor", "--json"], &[]);
    assert_eq!(code(&first), 0, "first run exits 0");
    assert_eq!(code(&second), 0, "second run exits 0");
    assert_eq!(
        first.stdout, second.stdout,
        "two doctor --json runs against unchanged state must be byte-identical"
    );
}

#[test]
fn human_mode_keeps_findings_off_stdout() {
    let fx = Fixture::new();
    let out = fx.run(&["doctor"], &[]);
    assert_eq!(code(&out), 0, "human-mode doctor exits 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("DOC-NO-DEFAULT-REPO"),
        "the info finding must surface on stderr in human mode, got stderr: {stderr}"
    );
    assert!(
        !stdout(&out).contains("DOC-NO-DEFAULT-REPO"),
        "findings must not pollute stdout in human mode, got stdout: {}",
        stdout(&out)
    );
}

#[test]
fn fix_yes_writes_default_repo_from_cwd_and_exits_zero() {
    let fx = Fixture::new();
    let out = fx.run_in(&fx.root, &["doctor", "--fix", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "doctor --fix --yes must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let pointer = fx.state_root().join("default_repo");
    let written = fs_err::read_to_string(pointer.as_std_path())
        .expect("the default_repo pointer must be written");
    let expected = canonicalize_path(&fx.root).expect("canonicalize the repo root");
    assert_eq!(
        written.trim(),
        expected.as_str(),
        "the pointer must hold the CWD's canonical absolute path"
    );
}

#[test]
fn fix_yes_from_non_repo_cwd_exits_one_and_writes_no_pointer() {
    let fx = Fixture::new();
    let not_a_repo = fx.home.join("not_a_repo");
    fs_err::create_dir_all(not_a_repo.as_std_path()).expect("mkdir non-repo cwd");

    let out = fx.run_in(&not_a_repo, &["doctor", "--fix", "--yes"], &[]);
    assert_eq!(
        code(&out),
        1,
        "doctor --fix --yes from a non-repo CWD must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a valid Patina repository"),
        "the refusal must explain the CWD is not a repository, got stderr: {stderr}"
    );

    let pointer = fx.state_root().join("default_repo");
    assert!(
        !pointer.as_std_path().exists(),
        "a non-repo CWD must not have the default_repo pointer written"
    );
}

#[test]
fn fix_without_yes_in_non_tty_exits_one_identifying_yes() {
    let fx = Fixture::new();
    let out = fx.run(&["doctor", "--fix"], &[]);
    assert_eq!(
        code(&out),
        1,
        "non-TTY --fix without --yes must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--yes"),
        "the refusal must include the missing --yes flag, got stderr: {stderr}"
    );

    let pointer = fx.state_root().join("default_repo");
    assert!(
        !pointer.as_std_path().exists(),
        "the non-TTY refusal must not write the pointer"
    );
}

#[test]
#[cfg(windows)]
#[ignore = "requires a Windows host with Developer Mode OFF and interactive UAC"]
fn windows_fix_enables_dev_mode_and_exits_zero() {
    use patina_core::DevModeStatus;
    use patina_core::dev_mode_status;

    let fx = Fixture::new();
    fx.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(
        fx.root.join("zsh").join("zshrc").as_std_path(),
        "export A=1\n",
    )
    .expect("seed repo source");

    let out = fx.run(&["doctor", "--fix", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "doctor --fix must exit 0 after enabling Developer Mode; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        dev_mode_status(),
        DevModeStatus::Enabled,
        "the registry value AllowDevelopmentWithoutDevLicense must read 1 afterward"
    );
}

#[test]
fn unc_predicate_distinguishes_unc_from_posix_repo_paths() {
    assert!(
        is_unc_path(Utf8Path::new(r"\\fileserver\share\dotfiles")),
        "a UNC repository path must be detected"
    );
    assert!(
        !is_unc_path(Utf8Path::new("/home/u/dotfiles")),
        "a POSIX repository path must not be flagged UNC"
    );
}

#[test]
#[cfg(windows)]
#[ignore = "requires a Windows host with Developer Mode OFF"]
fn windows_devmode_off_with_symlink_repo_warns() {
    let fx = Fixture::new();
    fx.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(
        fx.root.join("zsh").join("zshrc").as_std_path(),
        "export A=1\n",
    )
    .expect("seed repo source");

    let out = fx.run(&["doctor", "--json"], &[]);
    let doc: serde_json::Value =
        serde_json::from_str(stdout(&out).trim()).expect("doctor --json emits one JSON document");
    let findings = doc
        .get("findings")
        .and_then(serde_json::Value::as_array)
        .expect("findings array present");
    let devmode = findings
        .iter()
        .find(|f| f.get("code").and_then(serde_json::Value::as_str) == Some("DOC-WIN-DEVMODE"))
        .expect("DOC-WIN-DEVMODE finding present");
    assert_eq!(
        devmode.get("level").and_then(serde_json::Value::as_str),
        Some("warning")
    );
    let message = devmode
        .get("message")
        .and_then(serde_json::Value::as_str)
        .expect("message field present");
    assert!(
        message.contains("Developer Mode") && message.contains(patina_core::DEV_MODE_REGISTRY_PATH),
        "the DOC-WIN-DEVMODE message must include Developer Mode and the registry path, got: {message}"
    );
}
