//! Integration tests for `watch_service_cli`.
mod common;

use common::Fixture;
use common::code;

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

fn not_installed_or_unsupported(stderr: &str) -> bool {
    let not_installed =
        stderr.contains("service not installed") && stderr.contains("patina watch install");
    let unsupported = stderr.contains("--foreground");
    not_installed || unsupported
}

#[test]
fn stop_and_uninstall_on_a_not_installed_service_do_not_error_spuriously() {
    let f = Fixture::new();
    for sub in [["watch", "stop"], ["watch", "restart"]] {
        let out = f.run(&sub, &[]);
        assert_eq!(
            code(&out),
            1,
            "{sub:?} on a not-installed service must exit 1; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let out = f.run(&["watch", "uninstall", "--yes"], &[]);
    assert_eq!(
        code(&out),
        1,
        "uninstall on a not-installed service must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

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
