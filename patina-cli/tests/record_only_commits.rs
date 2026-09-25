//! Integration tests for the record-only commits that `remove` and `promote`
//! write.

#![cfg(test)]

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;
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
        let before = fx.deployment_snapshot();

        let out = fx.run_ok(&["rollback", "--yes"]);

        assert_eq!(
            fx.deployment_snapshot(),
            before,
            "{command}: rolling back its commit must not write under the home or repository"
        );
        assert!(
            !stderr(&out).contains("kept a copy"),
            "{command}: stderr: {}",
            stderr(&out)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("no files changed"));
        let json = fx.run_ok(&["rollback", "--yes", "--json"]);
        let doc: serde_json::Value = serde_json::from_slice(&json.stdout).expect("rollback JSON");
        assert_eq!(
            doc.get("result").and_then(serde_json::Value::as_str),
            Some("rolled_back")
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
        stderr(&failed).contains("failed to remove file"),
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

#[test]
fn remove_rollback_apply_keeps_the_unmanaged_file() {
    let fx = applied("");
    fx.run_ok(&["remove", "~/.a", "--yes"]);
    fx.run_ok(&["rollback", "--yes"]);
    assert_eq!(status_state(&fx, ".a"), None);
    fx.run_ok(&["apply", "--yes"]);
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read unmanaged file"),
        "A\n"
    );
}

#[test]
fn a_future_record_does_not_hide_the_next_apply() {
    let fx = applied("");
    let journal = fx.state_root().join("journal");
    let original = fs_err::read_dir(&journal)
        .expect("read journal")
        .map(|entry| entry.expect("read entry").path())
        .find(|path| path.extension().is_some_and(|ext| ext == "COMMIT"))
        .expect("find commit");
    fs_err::rename(original, journal.join("29990101T000000Z.COMMIT"))
        .expect("move commit into future");
    fs_err::write(fx.root.join("shell/a"), "NEXT\n").expect("edit source");
    let interrupted = fx.run(&["apply", "--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&interrupted), 70, "{}", stderr(&interrupted));
    assert_eq!(
        patina_core::orphan_plans(&journal)
            .expect("find interrupted apply")
            .len(),
        1
    );
    fx.run_ok(&["apply", "--yes"]);
    assert_eq!(status_state(&fx, ".a").as_deref(), Some("clean"));
}

#[test]
fn interrupted_remove_restores_its_manifest_and_can_be_retried() {
    for step in ["0", "1", "2"] {
        for purge in [false, true] {
            let fx = applied("");
            let args = if purge {
                vec!["remove", "~/.a", "--yes", "--purge"]
            } else {
                vec!["remove", "~/.a", "--yes"]
            };
            let interrupted = fx.run(&args, &[("PATINA_TEST_ABORT_REMOVE_AFTER", step)]);
            assert_eq!(
                code(&interrupted),
                99,
                "step {step}: {}",
                stderr(&interrupted)
            );
            assert_eq!(
                patina_core::orphan_plans(fx.state_root().join("journal"))
                    .expect("read pending")
                    .len(),
                1
            );
            fx.run_ok(&args);
            assert_eq!(status_state(&fx, ".a"), None);
            if purge {
                assert!(!fx.home.join(".a").exists());
            } else {
                assert_eq!(
                    fs_err::read_to_string(fx.home.join(".a")).expect("read preserved file"),
                    "A\n"
                );
            }
            fx.run_ok(&["rollback", "--yes"]);
            fx.run_ok(&["apply", "--yes"]);
            assert_eq!(status_state(&fx, ".a"), None);
        }
    }
}

#[test]
fn committed_remove_survives_interruption_before_returning() {
    let fx = applied("");
    let interrupted = fx.run(
        &["remove", "~/.a", "--yes"],
        &[("PATINA_TEST_ABORT_REMOVE_AFTER", "3")],
    );
    assert_eq!(code(&interrupted), 99, "{}", stderr(&interrupted));
    assert!(
        patina_core::orphan_plans(fx.state_root().join("journal"))
            .expect("read pending")
            .is_empty()
    );
    fx.run_ok(&["rollback", "--yes"]);
    fx.run_ok(&["apply", "--yes"]);
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read unmanaged file"),
        "A\n"
    );
    assert_eq!(status_state(&fx, ".a"), None);
}

#[test]
fn interrupted_remove_restores_the_symlink_and_original_manifest() {
    let fx = Fixture::new();
    let manifest = "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"symlink\"\n";
    let module = fx.module("shell", manifest);
    fs_err::write(module.join("a"), "A\n").expect("write source");
    fx.run_ok(&["apply", "--yes"]);
    let link = fs_err::read_link(fx.home.join(".a")).expect("read applied link");
    let interrupted = fx.run(
        &["remove", "~/.a", "--yes"],
        &[("PATINA_TEST_ABORT_REMOVE_AFTER", "2")],
    );
    assert_eq!(code(&interrupted), 99, "{}", stderr(&interrupted));

    patina_core::recover_interrupted(&fx.state_root()).expect("recover interrupted removal");

    assert_eq!(
        fs_err::read_link(fx.home.join(".a")).expect("read restored link"),
        link
    );
    assert_eq!(
        fs_err::read_to_string(module.join("patina.toml")).expect("read restored manifest"),
        manifest
    );
    fx.run_ok(&["remove", "~/.a", "--yes"]);
    assert!(
        fs_err::symlink_metadata(fx.home.join(".a"))
            .expect("stat unmanaged file")
            .is_file()
    );
}

#[test]
fn remove_without_a_declaration_does_not_recover_an_unrelated_apply() {
    let fx = applied("");
    fx.module(
        "shell",
        "[[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n",
    );
    fs_err::write(fx.root.join("shell/b"), "NEXT\n").expect("change remaining source");
    let interrupted = fx.run(&["apply", "--yes"], &[("PATINA_TEST_ABORT_AFTER_OP", "1")]);
    assert_eq!(code(&interrupted), 70, "{}", stderr(&interrupted));
    let before = common::snapshot(&[&fx.home, &fx.root, &fx.state]);

    let refused = fx.run(&["remove", "~/.a", "--yes"], &[]);

    assert_eq!(code(&refused), 1, "{}", stderr(&refused));
    assert_eq!(common::snapshot(&[&fx.home, &fx.root, &fx.state]), before);
    assert_eq!(
        patina_core::orphan_plans(fx.state_root().join("journal"))
            .expect("read pending plan")
            .len(),
        1
    );
}

#[test]
fn record_only_history_is_bounded() {
    let fx = applied("");
    for i in 0..12 {
        fs_err::write(fx.home.join(".a"), format!("edit {i}\n")).expect("edit target");
        fx.run_ok(&["promote", "~/.a", "--yes"]);
    }
    let commits = fs_err::read_dir(fx.state_root().join("journal"))
        .expect("read journal")
        .map(|entry| entry.expect("read entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "COMMIT"))
        .count();
    assert_eq!(commits, patina_core::backups::RETENTION_COUNT);
}

#[test]
fn rollback_crosses_remove_and_promote_without_reverting_their_target() {
    for command in ["remove", "promote"] {
        let fx = applied("");
        fs_err::write(fx.root.join("shell/b"), "B2\n").expect("update unrelated source");
        fx.run_ok(&["apply", "--yes"]);
        if command == "promote" {
            fs_err::write(fx.home.join(".a"), "PROMOTED\n").expect("edit target");
        }
        fx.run_ok(&[command, "~/.a", "--yes"]);
        fx.run_ok(&["rollback", "--yes"]);
        fx.run_ok(&["rollback", "--yes"]);
        assert_eq!(
            fs_err::read_to_string(fx.home.join(".b")).expect("read b"),
            "B\n"
        );
        fx.run_ok(&["rollback", "--yes"]);
        assert!(!fx.home.join(".b").exists());
        assert_eq!(
            fs_err::read_to_string(fx.home.join(".a")).expect("read a"),
            if command == "remove" {
                "A\n"
            } else {
                "PROMOTED\n"
            }
        );
        assert_eq!(
            status_state(&fx, ".a").as_deref(),
            if command == "remove" {
                None
            } else {
                Some("clean")
            }
        );
        let exhausted = fx.run(&["rollback", "--yes"], &[]);
        assert_eq!(code(&exhausted), 1, "{}", stderr(&exhausted));
        fx.run_ok(&["apply", "--yes"]);
        assert_eq!(
            fs_err::read_to_string(fx.home.join(".a")).expect("read preserved a"),
            if command == "remove" {
                "A\n"
            } else {
                "PROMOTED\n"
            }
        );
    }
}

#[test]
fn readded_target_can_be_rolled_back_before_crossing_its_removal() {
    let fx = applied("");
    fx.run_ok(&["remove", "~/.a", "--yes"]);
    fx.module("shell", BOTH);
    fs_err::write(fx.root.join("shell/a"), "NEW\n").expect("change readded source");
    fx.run_ok(&["apply", "--yes"]);
    fx.run_ok(&["rollback", "--yes"]);
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read restored a"),
        "A\n"
    );
    fx.run_ok(&["rollback", "--yes"]);
    fx.run_ok(&["rollback", "--yes"]);
    assert!(!fx.home.join(".b").exists());
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read unmanaged a"),
        "A\n"
    );
}

#[test]
fn consecutive_promotions_keep_the_latest_bytes_across_older_applies() {
    let fx = applied("");
    for bytes in ["FIRST\n", "LATEST\n"] {
        fs_err::write(fx.home.join(".a"), bytes).expect("edit a");
        fx.run_ok(&["promote", "~/.a", "--yes"]);
    }
    for _ in 0..2 {
        let out = fx.run_ok(&["rollback", "--yes", "--json"]);
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("rollback JSON");
        assert_eq!(
            doc.get("result").and_then(serde_json::Value::as_str),
            Some("record_only")
        );
        assert_eq!(status_state(&fx, ".a").as_deref(), Some("clean"));
    }
    fx.run_ok(&["rollback", "--yes"]);
    assert!(!fx.home.join(".b").exists());
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read a"),
        "LATEST\n"
    );
    assert_eq!(
        fs_err::read_to_string(fx.root.join("shell/a")).expect("read source"),
        "LATEST\n"
    );
    assert_eq!(status_state(&fx, ".a").as_deref(), Some("clean"));
}

#[test]
fn later_apply_to_promoted_target_can_be_reversed() {
    let fx = applied("");
    fs_err::write(fx.home.join(".a"), "PROMOTED\n").expect("edit a");
    fx.run_ok(&["promote", "~/.a", "--yes"]);
    fs_err::write(fx.root.join("shell/a"), "NEW\n").expect("change source");
    fx.run_ok(&["apply", "--yes"]);
    fx.run_ok(&["rollback", "--yes"]);
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read a"),
        "PROMOTED\n"
    );
    fx.run_ok(&["rollback", "--yes"]);
    fx.run_ok(&["rollback", "--yes"]);
    assert!(!fx.home.join(".b").exists());
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read preserved a"),
        "PROMOTED\n"
    );
}

#[test]
fn purge_remains_absent_when_rollback_crosses_earlier_applies() {
    let fx = applied("");
    fx.run_ok(&["remove", "~/.a", "--purge", "--yes"]);
    fx.run_ok(&["rollback", "--yes"]);
    fx.run_ok(&["rollback", "--yes"]);
    assert!(!fx.home.join(".a").exists());
    assert!(!fx.home.join(".b").exists());
    assert_eq!(status_state(&fx, ".a"), None);
}

#[test]
fn pruning_a_promotion_does_not_forget_its_target_after_rollback() {
    let fx = applied("");
    fs_err::write(fx.home.join(".a"), "PROMOTED-A\n").expect("edit a");
    fx.run_ok(&["promote", "~/.a", "--yes"]);
    for i in 0..patina_core::backups::RETENTION_COUNT {
        fs_err::write(fx.home.join(".b"), format!("PROMOTED-B-{i}\n")).expect("edit b");
        fx.run_ok(&["promote", "~/.b", "--yes"]);
    }
    for _ in 0..patina_core::backups::RETENTION_COUNT {
        fx.run_ok(&["rollback", "--yes"]);
    }
    assert_eq!(status_state(&fx, ".a").as_deref(), Some("clean"));
    assert_eq!(status_state(&fx, ".b").as_deref(), Some("clean"));
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read a"),
        "PROMOTED-A\n"
    );
}

#[test]
fn applies_carry_promoted_ownership_past_history_retention() {
    let fx = applied("");
    fs_err::write(fx.home.join(".a"), "PROMOTED-A\n").expect("edit a");
    fx.run_ok(&["promote", "~/.a", "--yes"]);
    for i in 0..patina_core::backups::RETENTION_COUNT {
        fs_err::write(fx.root.join("shell/b"), format!("NEW-B-{i}\n")).expect("edit source b");
        fx.run_ok(&["apply", "--yes"]);
    }
    for _ in 0..patina_core::backups::RETENTION_COUNT {
        fx.run_ok(&["rollback", "--yes"]);
    }
    assert_eq!(status_state(&fx, ".a").as_deref(), Some("clean"));
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".a")).expect("read a"),
        "PROMOTED-A\n"
    );
    assert_eq!(
        fs_err::read_to_string(fx.home.join(".b")).expect("read b"),
        "B\n"
    );
}
