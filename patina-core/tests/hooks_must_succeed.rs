//! Integration coverage for hook `must_succeed` semantics.
//!
//! These exercise the public `patina_core::apply::hooks` surface against
//! the real host shell: shells resolve up front, `when` predicates filter,
//! and a hook command's exit status classifies into a [`HookOutcome`]. The
//! orchestration into the apply pipeline (which maps `Failed` to an abort
//! or a rollback and to CLI exit codes) lands in later tasks; this task
//! owns the classification primitives those tasks consume.

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

/// Build a `must_succeed = true` hook pinned to the host's default shell
/// so the command runs under a shell guaranteed present on the test host.
fn hook_on_default_shell(event: HookEvent, command: &str) -> HookEntry {
    HookEntry {
        event,
        command: command.to_owned(),
        shell: Some(default_shell().to_owned()),
        when: None,
        must_succeed: true,
    }
}

/// The host default shell name (`bash` on macOS / Linux, `pwsh` on
/// Windows). `exit N` is valid in both, so commands stay portable.
fn default_shell() -> &'static str {
    match HostOs::current() {
        HostOs::Windows => "pwsh",
        HostOs::Linux | HostOs::MacOs => "bash",
    }
}

fn resolver() -> Resolver {
    Resolver::new(Builtins::for_tests())
}

/// Whether `c` is safe inside a single-quoted `MiniJinja` string literal:
/// no backslash (escape lead-in) and no single quote (delimiter).
fn is_clean_literal_char(c: char) -> bool {
    c != '\\' && c != '\''
}

#[tokio::test]
async fn pre_apply_failure_with_must_succeed_classifies_failed() {
    // A `pre_apply` hook returning non-zero with
    // the default `must_succeed = true`. The orchestrator maps
    // this `Failed` classification to exit code 2 and runs no file op.
    let hooks = vec![hook_on_default_shell(HookEvent::PreApply, "exit 1")];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");
    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::No,
    )
    .await
    .expect("hook runs");
    assert_eq!(outcome, HookOutcome::Failed);
    // The failing hook command is recoverable from the entry the
    // orchestrator surfaces on stderr.
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
    // A `post_apply` failure under `must_succeed = true` is the rollback
    // trigger (it maps to exit 3). The event on the entry tells the
    // orchestrator it is the post-apply branch.
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
    // A hook declaring a shell that does not exist on
    // PATH. `resolve_shells` errors before producing any runnable hook,
    // so the orchestrator never spawns a command or touches a file.
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
    // A hook gated on an OS the host is not.
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
    // `when = "patina.env.CI == 'true'"` evaluated against
    // the live process environment. The workspace forbids `unsafe`, so the
    // test cannot mutate the environment to inject `CI`; instead it picks an
    // env var already set in the process and compares against its live
    // value. This exercises the same `patina.env.*` lookup the `CI` scenario
    // relies on: a `when` predicate referencing a set env var resolves to
    // that var's value and the hook runs when it matches.
    //
    // The chosen var must have a name that is a bare identifier and a value
    // free of backslashes / single quotes, so it embeds as a clean MiniJinja
    // string literal (Windows `PATH` carries backslashes Jinja would read as
    // escapes, hence the selection rather than a hard-coded var).
    let (name, value) = std::env::vars()
        .find(|(k, v)| {
            // The name must be a bare identifier so `patina.env.NAME`
            // parses as dotted access (Windows has names like
            // `ProgramFiles(x86)` that would not), and the value must be a
            // clean string literal.
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
