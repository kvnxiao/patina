//! Integration tests for config hook entry.

#![expect(
    clippy::indexing_slicing,
    reason = "integration tests use direct [0] indexing for assertion-only fixture inspection where the vector length is already asserted immediately above; bounds-check panics would be acceptable test signal anyway."
)]

use patina_core::ConfigParseError;
use patina_core::HookEvent;
use patina_core::config::HookEntryError;
use patina_core::config::parse_module_config_str;

#[test]
fn parses_pre_apply_hook_with_must_succeed_default() {
    let toml = r#"
[[hook]]
event = "pre_apply"
command = "echo hi"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.hooks.len(), 1);
    let hook = &config.hooks[0];
    assert_eq!(hook.event, HookEvent::PreApply);
    assert_eq!(hook.command, "echo hi");
    assert!(hook.must_succeed);
}

#[test]
fn must_succeed_defaults_to_true_per_chk_014() {
    let toml = r#"
[[hook]]
event = "pre_apply"
command = "exit 0"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert!(config.hooks[0].must_succeed);
}

#[test]
fn parses_post_apply_with_explicit_must_succeed_false() {
    let toml = r#"
[[hook]]
event = "post_apply"
command = "exit 0"
must_succeed = false
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    let hook = &config.hooks[0];
    assert_eq!(hook.event, HookEvent::PostApply);
    assert!(!hook.must_succeed);
}

#[test]
fn rejects_on_change_event_listing_accepted_values() {
    let toml = r#"
[[hook]]
event = "on_change"
command = "echo hi"
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(rendered.contains("on_change"), "rendered: {rendered}");
    assert!(rendered.contains("pre_apply"), "rendered: {rendered}");
    assert!(rendered.contains("post_apply"), "rendered: {rendered}");
    assert!(matches!(
        err,
        ConfigParseError::HookEntry(HookEntryError::UnsupportedEvent { .. })
    ));
}

#[test]
fn rejects_on_drift_event_listing_accepted_values() {
    let toml = r#"
[[hook]]
event = "on_drift"
command = "echo hi"
"#;
    let err = parse_module_config_str(toml).expect_err("parse fails");
    let rendered = err.to_string();
    assert!(rendered.contains("on_drift"), "rendered: {rendered}");
    assert!(rendered.contains("pre_apply"), "rendered: {rendered}");
    assert!(rendered.contains("post_apply"), "rendered: {rendered}");
}

#[test]
fn preserves_when_expression_verbatim() {
    let toml = r#"
[[hook]]
event = "pre_apply"
command = "echo hi"
when = "patina.os == 'macos'"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(
        config.hooks[0].when.as_deref(),
        Some("patina.os == 'macos'")
    );
}

#[test]
fn preserves_shell_verbatim() {
    let toml = r#"
[[hook]]
event = "post_apply"
command = "Get-ChildItem"
shell = "pwsh"
"#;
    let config = parse_module_config_str(toml).expect("parse succeeds");
    assert_eq!(config.hooks[0].shell.as_deref(), Some("pwsh"));
}
