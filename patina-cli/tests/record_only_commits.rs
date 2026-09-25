//! Integration tests for the record-only commits that `remove` and `promote`
//! write.

#![cfg(test)]

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;
use common::snapshot;
use common::stderr;

const BOTH: &str = "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n\n\
                    [[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n";

const C_ENTRY: &str = "\n[[file]]\nsource = \"c\"\ntarget = \"~/.c\"\nmode = \"copy\"\n";

const HOOK_MARKER: &str = "hook-ran";

fn hooks() -> String {
    let write = if cfg!(windows) {
        format!(
            "Set-Content -NoNewline -Path (Join-Path $env:USERPROFILE {HOOK_MARKER}) -Value HOOK"
        )
    } else {
        format!("printf HOOK > \"$HOME/{HOOK_MARKER}\"")
    };
    format!(
        "\n[[hook]]\nevent = \"pre_apply\"\ncommand = '{write}'\n\n\
         [[hook]]\nevent = \"post_apply\"\ncommand = '{write}'\n"
    )
}

fn applied(extra_manifest: &str) -> Fixture {
    let fx = Fixture::new();
    let module = fx.module("shell", &format!("{BOTH}{extra_manifest}"));
    fs_err::write(module.join("a"), "A\n").expect("write source a");
    fs_err::write(module.join("b"), "B\n").expect("write source b");
    fx.run_ok(&["apply", "--yes"]);
    fx
}

fn status_files(fx: &Fixture) -> Vec<serde_json::Value> {
    let out = fx.run_ok(&["status", "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status JSON");
    doc.get("files")
        .and_then(serde_json::Value::as_array)
        .expect("a files array")
        .clone()
}

fn status_state(fx: &Fixture, name: &str) -> Option<String> {
    status_files(fx)
        .into_iter()
        .find(|file| {
            file.get("path")
                .and_then(serde_json::Value::as_str)
                .and_then(|path| Utf8Path::new(path).file_name())
                == Some(name)
        })
        .and_then(|file| {
            file.get("state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_ascii_lowercase)
        })
}

#[test]
fn remove_and_promote_leave_other_targets_alone_and_run_no_hook() {
    for command in ["remove", "promote"] {
        let fx = applied(&hooks());
        let marker = fx.home.join(HOOK_MARKER);
        assert!(marker.exists(), "{command}: the apply's hooks must run");
        fs_err::remove_file(&marker).expect("clear the hook marker");
        if command == "promote" {
            fs_err::write(fx.home.join(".a"), "EDITED-A\n").expect("edit ~/.a outside patina");
        }
        fs_err::write(fx.home.join(".b"), "DRIFTED-B\n").expect("edit ~/.b outside patina");
        let module = fx.module("shell", &format!("{BOTH}{C_ENTRY}{}", hooks()));
        fs_err::write(module.join("c"), "C\n").expect("write source c");

        fx.run_ok(&[command, "~/.a", "--yes"]);

        assert_eq!(
            fs_err::read_to_string(fx.home.join(".b")).expect("read ~/.b"),
            "DRIFTED-B\n",
            "{command} must not rewrite another drifted target"
        );
        assert!(
            fs_err::symlink_metadata(fx.home.join(".c")).is_err(),
            "{command} must not create a newly declared target"
        );
        assert!(!marker.exists(), "{command} must not run a hook");
        assert_eq!(
            status_state(&fx, ".b").as_deref(),
            Some("drifted"),
            "{command}: status must still report the drift"
        );
    }
}

#[test]
fn promote_records_the_promoted_bytes_as_clean() {
    let fx = applied("");
    fs_err::write(fx.home.join(".a"), "EDITED-A\n").expect("edit ~/.a outside patina");

    fx.run_ok(&["promote", "~/.a", "--yes"]);

    assert_eq!(status_state(&fx, ".a").as_deref(), Some("clean"));
    assert_eq!(status_state(&fx, ".b").as_deref(), Some("clean"));
}

#[test]
fn a_rollback_right_after_remove_or_promote_changes_no_file() {
    for command in ["remove", "promote"] {
        let fx = applied("");
        if command == "promote" {
            fs_err::write(fx.home.join(".a"), "EDITED-A\n").expect("edit ~/.a outside patina");
        }
        fs_err::write(fx.home.join(".b"), "DRIFTED-B\n").expect("edit ~/.b outside patina");
        fx.run_ok(&[command, "~/.a", "--yes"]);
        let before = snapshot(&[&fx.home, &fx.root]);

        let out = fx.run_ok(&["rollback", "--yes"]);

        assert_eq!(
            snapshot(&[&fx.home, &fx.root]),
            before,
            "{command}: rolling back its commit must not write under the home or repository"
        );
        assert!(
            !stderr(&out).contains("kept a copy"),
            "{command}: stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn a_remove_that_fails_after_its_commit_can_be_retried() {
    let fx = applied("");
    let a = fx.home.join(".a");
    fs_err::remove_file(&a).expect("remove the applied ~/.a");
    fs_err::create_dir(&a).expect("put a directory where ~/.a was");

    let failed = fx.run(&["remove", "~/.a", "--yes"], &[]);

    assert_eq!(code(&failed), 1, "stderr: {}", stderr(&failed));
    assert!(
        stderr(&failed).contains("failed to remove the existing target"),
        "the remove must fail while replacing the target; stderr: {}",
        stderr(&failed)
    );
    assert!(
        status_state(&fx, ".a").is_some(),
        "the failed remove must leave ~/.a recorded"
    );
    fs_err::remove_dir(&a).expect("clear the directory");
    fx.run_ok(&["remove", "~/.a", "--yes"]);
    assert_eq!(fs_err::read_to_string(&a).expect("read ~/.a"), "A\n");
    assert_eq!(status_state(&fx, ".a"), None, "the retry unmanages ~/.a");
}
