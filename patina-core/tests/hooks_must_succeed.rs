//! Integration tests for hooks must succeed.

use patina_core::ForceDeploy;
use patina_core::HookError;
use patina_core::HookOutcome;
use patina_core::TemplateEngine;
use patina_core::config::HookEntry;
use patina_core::config::HookEvent;
use patina_core::resolve_shells;
use patina_core::run_hook;
use patina_core::should_run;
use patina_core::state_dir::HostOs;
use patina_core::variables::Builtins;
use patina_core::variables::Resolver;

fn hook_on_default_shell(event: HookEvent, command: &str) -> HookEntry {
    HookEntry {
        event,
        command: command.to_owned(),
        shell: Some(default_shell().to_owned()),
        when: None,
        must_succeed: true,
    }
}

fn default_shell() -> &'static str {
    match HostOs::current() {
        HostOs::Windows => "pwsh",
        HostOs::Linux | HostOs::MacOs => "bash",
    }
}

fn resolver() -> Resolver {
    Resolver::new(Builtins::for_tests())
}

fn is_clean_literal_char(c: char) -> bool {
    c != '\\' && c != '\''
}

#[tokio::test]
async fn pre_apply_failure_with_must_succeed_classifies_failed() {
    let hooks = vec![hook_on_default_shell(HookEvent::PreApply, "exit 1")];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");
    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::No,
    )
    .await
    .expect("hook runs");
    assert_eq!(outcome, HookOutcome::Failed);
    assert_eq!(
        resolved.first().expect("one resolved hook").entry.command,
        "exit 1"
    );
    assert_eq!(
        resolved.first().expect("one resolved hook").entry.event,
        HookEvent::PreApply
    );
}

#[tokio::test]
async fn post_apply_failure_with_must_succeed_classifies_failed() {
    let hooks = vec![hook_on_default_shell(HookEvent::PostApply, "exit 1")];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");
    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::No,
    )
    .await
    .expect("hook runs");
    assert_eq!(outcome, HookOutcome::Failed);
    assert_eq!(
        resolved.first().expect("one resolved hook").entry.event,
        HookEvent::PostApply
    );
}

#[tokio::test]
async fn zero_exit_classifies_succeeded() {
    let hooks = vec![hook_on_default_shell(HookEvent::PreApply, "exit 0")];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");
    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::No,
    )
    .await
    .expect("hook runs");
    assert_eq!(outcome, HookOutcome::Succeeded);
}

#[tokio::test]
async fn non_must_succeed_failure_only_warns() {
    let mut entry = hook_on_default_shell(HookEvent::PreApply, "exit 1");
    entry.must_succeed = false;
    let hooks = vec![entry];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");
    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::No,
    )
    .await
    .expect("hook runs");
    assert_eq!(outcome, HookOutcome::Warned);
}

#[test]
fn unresolved_explicit_shell_errors_before_any_hook_runs() {
    let entry = HookEntry {
        event: HookEvent::PreApply,
        command: "exit 0".to_owned(),
        shell: Some("nonexistent-shell-xyz".to_owned()),
        when: None,
        must_succeed: true,
    };
    let err = resolve_shells(std::slice::from_ref(&entry), HostOs::current())
        .expect_err("unresolved shell must error");
    assert!(
        matches!(&err, HookError::ShellNotFound { shell } if shell == "nonexistent-shell-xyz"),
        "expected ShellNotFound naming the binary, got {err:?}"
    );
}

#[test]
fn when_predicate_filters_out_non_matching_host() {
    let r = resolver();
    let os = r.get("patina.os").expect("patina.os resolves");
    let other = if os == "macos" { "linux" } else { "macos" };
    let mut entry = hook_on_default_shell(HookEvent::PreApply, "exit 0");
    entry.when = Some(format!("patina.os == '{other}'"));
    let hooks = vec![entry];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");
    let runs = should_run(
        resolved.first().expect("one resolved hook"),
        &TemplateEngine::new(),
        &r,
    )
    .expect("eval");
    assert!(!runs, "hook gated on a foreign OS must be filtered out");
}

#[test]
fn when_predicate_runs_on_matching_env_var() {
    let (name, value) = std::env::vars()
        .find(|(k, v)| {
            !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !v.is_empty()
                && v.chars().all(is_clean_literal_char)
        })
        .expect("the test host exposes at least one cleanly-quotable env var");
    let r = resolver();
    let mut entry = hook_on_default_shell(HookEvent::PreApply, "exit 0");
    entry.when = Some(format!("patina.env.{name} == '{value}'"));
    let hooks = vec![entry];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");
    let runs = should_run(
        resolved.first().expect("one resolved hook"),
        &TemplateEngine::new(),
        &r,
    )
    .expect("eval");
    assert!(
        runs,
        "hook gated on a matching live env var must run (the CI=true case)"
    );
}
