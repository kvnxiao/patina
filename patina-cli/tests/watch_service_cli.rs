//! Integration tests for the `patina watch` background-service lifecycle CLI.
//!
//! These tests exercise the deterministic, supervisor-free surface of the
//! lifecycle commands: the not-installed no-op paths and the `status` object
//! shape. They deliberately do **not** drive a real `launchctl bootstrap` /
//! `systemctl --user enable`: registering a live per-user service from a test
//! would escape the fixture's tempdir isolation (the supervisor domain is the
//! developer's real session) and is unsafe in CI. The plist rendering and the
//! `launchctl print` parsing, the host-specific install/status internals, are
//! gated by the `patina-core` unit tests (`render_plist`,
//! `parse_launchctl_print`) instead.
//!
//! Every test runs against the fixture's isolated HOME / state tree, where no
//! service descriptor exists, so the backend's `is_installed()` is false and
//! the lifecycle calls short-circuit before touching any supervisor.

mod common;

use common::Fixture;
use common::code;

/// `patina watch` has no default action, so with neither a lifecycle
/// subcommand nor `--foreground` it reports the usage hint and exits non-zero.
#[test]
fn watch_with_no_mode_reports_the_usage_hint() {
    let f = Fixture::new();
    let out = f.run(&["watch"], &[]);
    assert_eq!(
        code(&out),
        1,
        "watch with no mode must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--foreground") && stderr.contains("install"),
        "the hint must include both modes, got: {stderr}"
    );
}

/// `patina watch start` with no installed service exits 1 with a clear
/// message rather than a spurious supervisor error.
///
/// On macOS the launchd backend finds no plist (`is_installed()` false) and
/// returns the not-installed no-op; on a host with no implemented backend the
/// factory's unsupported stub returns the foreground-escape-hatch error. Each
/// is exit 1 with an actionable stderr message.
#[test]
fn start_with_no_installed_service_exits_one_with_a_clear_message() {
    let f = Fixture::new();
    let out = f.run(&["watch", "start"], &[]);
    assert_eq!(
        code(&out),
        1,
        "start on a not-installed service must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        not_installed_or_unsupported(&stderr),
        "the not-installed message must point at install, or the unsupported \
         backend at the foreground hatch, got: {stderr}"
    );
}

/// Whether a lifecycle command's stderr is one of the valid not-installed
/// outcomes. A backend with an installed-service supervisor (launchd on
/// macOS, `systemd --user` on a systemd Linux host, the Scheduled Task on
/// Windows) reports the "service not installed; run `patina watch install`"
/// no-op; a host with no reachable backend (non-systemd Linux) reports the
/// unsupported `--foreground` escape hatch. The test host's OS and Linux
/// `systemd --user` availability select the message, so the lifecycle tests
/// accept either.
fn not_installed_or_unsupported(stderr: &str) -> bool {
    let not_installed =
        stderr.contains("service not installed") && stderr.contains("patina watch install");
    let unsupported = stderr.contains("--foreground");
    not_installed || unsupported
}

/// `patina watch stop` / `restart` / `uninstall` on a not-installed service
/// are likewise no-ops that do not error spuriously. On macOS they exit 1 with
/// the not-installed message; on an unsupported host the stub errors with the
/// foreground hint. Each leaves the state tree untouched.
#[test]
fn stop_and_uninstall_on_a_not_installed_service_do_not_error_spuriously() {
    let f = Fixture::new();
    for sub in [["watch", "stop"], ["watch", "restart"]] {
        let out = f.run(&sub, &[]);
        // Either the macOS not-installed no-op (exit 1, install hint) or the
        // unsupported-backend error (exit 1, foreground hint) applies, never a
        // panic or supervisor crash. The shared assertion is a clean exit 1.
        assert_eq!(
            code(&out),
            1,
            "{sub:?} on a not-installed service must exit 1; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // uninstall on a not-installed service is also a no-op (no plist to remove).
    let out = f.run(&["watch", "uninstall", "--yes"], &[]);
    assert_eq!(
        code(&out),
        1,
        "uninstall on a not-installed service must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `patina watch status --json` on a not-installed service emits a clean JSON
/// object reporting `installed = false`, `running = false`, and the remaining
/// status fields, then exits 0. A counter is null when the watcher has never
/// logged.
#[test]
fn status_json_on_a_not_installed_service_reports_a_clean_object() {
    let f = Fixture::new();
    let out = f.run(&["watch", "status", "--json"], &[]);
    assert_eq!(
        code(&out),
        0,
        "status --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("status --json emits one JSON document");

    assert_eq!(
        doc.get("installed"),
        Some(&serde_json::Value::Bool(false)),
        "a not-installed service reports installed=false; got: {stdout}"
    );
    assert_eq!(
        doc.get("running"),
        Some(&serde_json::Value::Bool(false)),
        "a not-installed service reports running=false; got: {stdout}"
    );
    // `subscriptions_count` and `re_applies_since_start` are present as JSON
    // null when the watcher has never logged under this isolated state tree.
    for field in [
        "last_fired_at",
        "last_exit_code",
        "subscriptions_count",
        "re_applies_since_start",
    ] {
        assert!(
            doc.get(field).is_some_and(serde_json::Value::is_null),
            "field `{field}` must be present and null on a never-run service; got: {stdout}"
        );
    }
}

/// `patina watch status` (human mode) on a not-installed service prints the
/// summary and exits 0 with the installed / running state.
#[test]
fn status_human_on_a_not_installed_service_prints_a_summary() {
    let f = Fixture::new();
    let out = f.run(&["watch", "status"], &[]);
    assert_eq!(
        code(&out),
        0,
        "status must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("installed:") && stdout.contains("running:"),
        "the human summary must include the installed / running state, got: {stdout}"
    );
}
