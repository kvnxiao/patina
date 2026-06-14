#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration coverage for the `--force-deploy` hook override.
//!
//! `--force-deploy` ([`ForceDeploy::Yes`]) overrides every hook in the
//! invocation to behave as `must_succeed = false`, so a non-zero exit can
//! only ever degrade to a warning — a `post_apply` failure under
//! force-deploy classifies [`HookOutcome::Warned`], never
//! [`HookOutcome::Failed`], so the orchestrator fires no rollback
//! and the CLI exits 0. These tests also prove the hook command
//! genuinely executes under force-deploy (its filesystem side effect lands)
//! rather than being skipped.

use camino::Utf8PathBuf;
use patina_core::ForceDeploy;
use patina_core::HookOutcome;
use patina_core::config::HookEntry;
use patina_core::config::HookEvent;
use patina_core::resolve_shells;
use patina_core::run_hook;
use patina_core::state_dir::HostOs;
use tempfile::TempDir;

/// The host default shell name (`bash` on macOS / Linux, `pwsh` on
/// Windows).
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

/// A command that creates `marker` and then exits non-zero, written in the
/// host default shell's dialect. The side effect proves the hook ran; the
/// non-zero exit is what `--force-deploy` must downgrade to a warning.
fn touch_then_fail(marker: &Utf8PathBuf) -> String {
    if matches!(HostOs::current(), HostOs::Windows) {
        // PowerShell: create the file, then exit 1.
        format!("New-Item -ItemType File -Force -Path '{marker}' | Out-Null; exit 1")
    } else {
        format!("touch '{marker}'; exit 1")
    }
}

#[tokio::test]
async fn force_deploy_downgrades_post_apply_failure_to_warning() {
    let (_td, dir) = utf8_tempdir();
    let marker = dir.join("hook-ran.marker");
    let entry = HookEntry {
        event: HookEvent::PostApply,
        command: touch_then_fail(&marker),
        shell: Some(default_shell().to_owned()),
        when: None,
        must_succeed: true,
    };
    let hooks = vec![entry];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");

    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::Yes,
    )
    .await
    .expect("hook runs");

    // No rollback would fire: the must_succeed=true failure degraded to a
    // warning under force-deploy.
    assert_eq!(outcome, HookOutcome::Warned);
    // The hook genuinely executed its side effect (it was not skipped).
    assert!(
        marker.exists(),
        "force-deploy must still run the hook; marker {marker} should exist"
    );
}

#[tokio::test]
async fn same_hook_without_force_deploy_classifies_failed() {
    // The contrast case for the scenario: the identical fixture without
    // `--force-deploy` keeps `must_succeed = true`, so the post_apply
    // failure classifies `Failed` (the rollback / exit-3 trigger).
    let (_td, dir) = utf8_tempdir();
    let marker = dir.join("hook-ran.marker");
    let entry = HookEntry {
        event: HookEvent::PostApply,
        command: touch_then_fail(&marker),
        shell: Some(default_shell().to_owned()),
        when: None,
        must_succeed: true,
    };
    let hooks = vec![entry];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");

    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::No,
    )
    .await
    .expect("hook runs");

    assert_eq!(outcome, HookOutcome::Failed);
    assert!(marker.exists(), "the hook ran before reporting its failure");
}

#[tokio::test]
async fn force_deploy_leaves_succeeding_hook_succeeded() {
    // Force-deploy only downgrades failures; a zero-exit hook is still
    // Succeeded under force-deploy.
    let entry = HookEntry {
        event: HookEvent::PreApply,
        command: "exit 0".to_owned(),
        shell: Some(default_shell().to_owned()),
        when: None,
        must_succeed: true,
    };
    let hooks = vec![entry];
    let resolved = resolve_shells(&hooks, HostOs::current()).expect("shells resolve");
    let outcome = run_hook(
        resolved.first().expect("one resolved hook"),
        ForceDeploy::Yes,
    )
    .await
    .expect("hook runs");
    assert_eq!(outcome, HookOutcome::Succeeded);
}
