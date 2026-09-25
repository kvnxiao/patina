//! Integration tests for hooks force deploy.

#![cfg(test)]

use camino::Utf8PathBuf;
use patina_core::ForceDeploy;
use patina_core::HookOutcome;
use patina_core::PlannedHook;
use patina_core::config::HookEntry;
use patina_core::config::HookEvent;
use patina_core::resolve_shells;
use patina_core::run_hook;
use patina_core::state_dir::HostOs;
use tempfile::TempDir;

fn planned(entries: Vec<HookEntry>) -> Vec<PlannedHook> {
    entries
        .into_iter()
        .map(|entry| PlannedHook::new(entry, 0))
        .collect()
}

fn default_shell() -> &'static str {
    match HostOs::current() {
        HostOs::Windows => "pwsh",
        HostOs::Linux | HostOs::MacOs => "bash",
    }
}

fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
    let td = TempDir::new().expect("create tempdir");
    let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
    let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
    (td, canonical)
}

fn touch_then_fail(marker: &Utf8PathBuf) -> String {
    if matches!(HostOs::current(), HostOs::Windows) {
        format!("New-Item -ItemType File -Force -Path '{marker}' | Out-Null; exit 1")
    } else {
        format!("touch '{marker}'; exit 1")
    }
}

#[test]
fn force_deploy_downgrades_post_apply_failure_to_warning() {
    let (_td, dir) = utf8_tempdir();
    let marker = dir.join("hook-ran.marker");
    let entry = HookEntry {
        event: HookEvent::PostApply,
        command: touch_then_fail(&marker),
        shell: Some(default_shell().to_owned()),
        when: None,
        must_succeed: true,
    };
    let hooks = planned(vec![entry]);
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");

    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::Yes,
    )
    .expect("hook runs");

    assert_eq!(outcome, HookOutcome::Warned);
    assert!(
        marker.exists(),
        "force-deploy must still run the hook; marker {marker} should exist"
    );
}

#[test]
fn same_hook_without_force_deploy_classifies_failed() {
    let (_td, dir) = utf8_tempdir();
    let marker = dir.join("hook-ran.marker");
    let entry = HookEntry {
        event: HookEvent::PostApply,
        command: touch_then_fail(&marker),
        shell: Some(default_shell().to_owned()),
        when: None,
        must_succeed: true,
    };
    let hooks = planned(vec![entry]);
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");

    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::No,
    )
    .expect("hook runs");

    assert_eq!(outcome, HookOutcome::Failed);
    assert!(marker.exists(), "the hook ran before reporting its failure");
}

#[test]
fn force_deploy_leaves_succeeding_hook_succeeded() {
    let entry = HookEntry {
        event: HookEvent::PreApply,
        command: "exit 0".to_owned(),
        shell: Some(default_shell().to_owned()),
        when: None,
        must_succeed: true,
    };
    let hooks = planned(vec![entry]);
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");
    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::Yes,
    )
    .expect("hook runs");
    assert_eq!(outcome, HookOutcome::Succeeded);
}
