//! Integration tests for apply cli.

#![cfg(test)]

mod common;

use common::Fixture;
use common::code;

#[test]
fn non_tty_apply_previews_without_mutating() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("rc"), "export A=1\n").expect("write source");

    let out = f.apply(&[]);

    assert_eq!(code(&out), 0, "non-TTY preview must exit 0");
    let target = f.home.join(".rc");
    assert!(
        fs_err::symlink_metadata(&target).is_err(),
        "no symlink may be created on a preview"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(".rc"),
        "stdout must contain the rendered diff including the target, got: {stdout}"
    );
}

#[test]
fn post_apply_hook_failure_rolls_back_and_exits_3() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n\n\
         [[hook]]\nevent = \"post_apply\"\ncommand = \"exit 1\"\n",
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        3,
        "a must_succeed post_apply hook failure must exit 3; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let target = f.home.join(".rc");
    assert!(
        fs_err::symlink_metadata(&target).is_err(),
        "the copied file must be reversed on rollback"
    );
}

#[test]
fn force_deploy_downgrades_hook_failure_and_exits_0() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n\n\
         [[hook]]\nevent = \"post_apply\"\ncommand = \"exit 1\"\n",
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");

    let out = f.apply(&["--yes", "--force-deploy"]);

    assert_eq!(
        code(&out),
        0,
        "--force-deploy must downgrade the hook failure to a warning; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let target = f.home.join(".rc");
    assert_eq!(
        fs_err::read_to_string(&target).expect("the copied file must survive"),
        "payload\n",
        "file ops must NOT be rolled back under --force-deploy"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("exit 1"),
        "stderr must warn and include the failed hook, got: {stderr}"
    );
}

#[test]
fn hook_output_reaches_the_apply_stdout_and_stderr() {
    let f = Fixture::new();
    let command = if cfg!(windows) {
        "Write-Output ('hook-out-' + (1 + 1)); [Console]::Error.WriteLine('hook-err-' + (1 + 1))"
    } else {
        "echo hook-out-$((1 + 1)); echo hook-err-$((1 + 1)) >&2"
    };
    let module = f.module(
        "shell",
        &format!(
            "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n\n\
             [[hook]]\nevent = \"pre_apply\"\ncommand = \"{command}\"\n"
        ),
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");

    let out = f.apply(&["--yes"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(code(&out), 0, "the hook exits zero; stderr: {stderr}");
    assert!(
        stdout.contains("hook-out-2"),
        "the hook's stdout must reach the apply's stdout, got: {stdout}"
    );
    assert!(
        stderr.contains("hook-err-2"),
        "the hook's stderr must reach the apply's stderr, got: {stderr}"
    );
}

#[test]
fn a_template_that_fails_to_render_at_plan_time_names_its_source() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc.tmpl\"\ntarget = \"~/.rc\"\n",
    );
    fs_err::write(module.join("rc.tmpl"), "email = {{ undefined_email }}\n")
        .expect("write template");

    let out = f.apply(&["--yes"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(code(&out), 1, "stderr: {stderr}");
    assert!(stderr.contains("rc.tmpl"), "{stderr}");
    assert!(
        fs_err::symlink_metadata(f.home.join(".rc")).is_err(),
        "a plan that failed to render must not write the target"
    );
}

#[test]
fn json_without_yes_previews_and_does_not_mutate() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("rc"), "x\n").expect("write source");

    let out = f.apply(&["--json"]);

    assert_eq!(code(&out), 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be a single JSON document");
    assert_eq!(
        doc.get("result").and_then(serde_json::Value::as_str),
        Some("previewed")
    );
    assert!(
        fs_err::symlink_metadata(f.home.join(".rc")).is_err(),
        "no mutation may occur on a JSON preview"
    );
}

#[test]
fn json_with_yes_applies_and_reports_applied() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "applied-content\n").expect("write source");

    let out = f.apply(&["--json", "--yes"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be a single JSON document");
    assert_eq!(
        doc.get("result").and_then(serde_json::Value::as_str),
        Some("applied")
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".rc")).expect("target written"),
        "applied-content\n"
    );
}

#[test]
fn cli_variable_override_renders_into_template() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"gitconfig.tmpl\"\ntarget = \"~/.gitconfig\"\n",
    );
    fs_err::write(module.join("gitconfig.tmpl"), "email = {{ email }}\n").expect("write tmpl");

    let out = f.apply(&["--yes", "-v", "email=cli@example.com"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rendered = fs_err::read_to_string(f.home.join(".gitconfig")).expect("target written");
    assert!(
        rendered.contains("email = cli@example.com"),
        "rendered target must contain the CLI-overridden value, got: {rendered}"
    );
}

#[cfg(not(windows))]
#[test]
fn non_windows_symlink_apply_skips_dev_mode_flow() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("rc"), "export A=1\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "non-Windows symlink apply must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let target = f.home.join(".rc");
    let meta = fs_err::symlink_metadata(&target).expect("symlink target must exist");
    assert!(
        meta.file_type().is_symlink(),
        "the target must be a symbolic link, proving the apply mutated"
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("patina-elevate"),
        "the elevation helper must never be named on non-Windows, got: {combined}"
    );
    assert!(
        !combined.contains("Developer Mode"),
        "the Developer Mode flow must not run on non-Windows, got: {combined}"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "requires a Windows host with Developer Mode off and a declined UAC dialog"]
fn windows_declined_uac_exits_5_and_creates_no_symlink() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("rc"), "export A=1\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(code(&out), 5, "declined UAC must exit 5");
    let target = f.home.join(".rc");
    assert!(
        fs_err::symlink_metadata(&target).is_err(),
        "no symbolic link may be created when elevation is declined"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Developer Mode") && stderr.contains("patina doctor --fix"),
        "stderr must include Developer Mode and `patina doctor --fix`, got: {stderr}"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "requires a Windows host with Developer Mode enabled"]
fn windows_dev_mode_on_creates_symlink_without_prompt() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("rc"), "export A=1\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "Developer Mode ON must apply cleanly; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let target = f.home.join(".rc");
    let meta = fs_err::symlink_metadata(&target).expect("symlink target must exist");
    assert!(
        meta.file_type().is_symlink(),
        "the symbolic link must be created when Developer Mode is on"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("patina-elevate"),
        "no elevation helper may be spawned when Developer Mode is on, got: {combined}"
    );
}
