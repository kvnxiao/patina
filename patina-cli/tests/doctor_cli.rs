#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

//! Integration coverage for `patina doctor` (REQ-005, REQ-009, REQ-010).
//!
//! Each test spawns the real `patina` binary against an isolated tempdir
//! repo + state + home (via the shared [`common::Fixture`]). The read-only
//! path (no `--fix`) is the one under test here; T-011 covers `--fix`.
//! Because the binary's stdin is not a TTY and doctor is read-only, no prompt
//! ever appears.

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;
use patina_core::is_unc_path;

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout is utf8")
}

/// Task scenario 1 (DOC-NO-DEFAULT-REPO): a tempdir state directory with no
/// `default_repo` and a valid repository yields a `findings` entry with
/// `code = DOC-NO-DEFAULT-REPO`, `level = info`, and the process exits 0.
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
}

/// Task scenario 2 / CHK-018 (REQ-010): two `doctor --json` runs against the
/// same unchanged state produce byte-identical stdout.
#[test]
fn doctor_json_is_byte_identical_across_runs() {
    let fx = Fixture::new();
    let first = fx.run(&["doctor", "--json"], &[]);
    let second = fx.run(&["doctor", "--json"], &[]);
    assert_eq!(code(&first), 0, "first run exits 0");
    assert_eq!(code(&second), 0, "second run exits 0");
    assert_eq!(
        first.stdout, second.stdout,
        "two doctor --json runs against unchanged state must be byte-identical (CHK-018)"
    );
}

/// REQ-010: doctor routes its findings to stderr, never to stdout, in human
/// (non-`--json`) mode — stdout stays clean for piping.
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

/// The DOC-WIN-UNC finding rests on the cross-platform [`is_unc_path`]
/// predicate (T-007). Assert the predicate the finding depends on
/// distinguishes a UNC repository path from a POSIX one, so the finding's
/// trigger is exercised on the macOS/Linux CI where a real UNC mount is
/// unavailable.
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

/// CHK-010 (REQ-005): on a Windows test host with Developer Mode OFF and a
/// repository declaring at least one `mode = "symlink"` entry, `doctor --json`
/// emits a `DOC-WIN-DEVMODE` warning whose message names Developer Mode and
/// the registry path. Gated to Windows and `#[ignore]` because it depends on
/// the host's real Developer Mode registry state.
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
        "the DOC-WIN-DEVMODE message must name Developer Mode and the registry path, got: {message}"
    );
}
