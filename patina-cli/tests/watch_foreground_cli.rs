//! Integration tests for watch foreground cli.

#![cfg_attr(
    unix,
    expect(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "integration tests use .expect() on fixtures and a bounded read-buffer slice; allow-*-in-tests covers #[cfg(test)] modules but not the helper methods in tests/*.rs integration crates."
    )
)]

mod common;

use common::Fixture;
#[cfg(unix)]
use common::code;
#[cfg(unix)]
use std::process::Command;

#[test]
fn debounce_ms_key_in_root_manifest_warns() {
    let f = Fixture::new();
    let root_manifest = f.root.join("patina.toml");
    fs_err::write(
        &root_manifest,
        "[patina]\nroot = true\n\n[watcher]\ndebounce_ms = 250\n",
    )
    .expect("rewrite root manifest with [watcher] debounce_ms");

    let out = f.run(&["watch"], &[]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("debounce_ms"),
        "stderr must warn about the ignored debounce_ms key, got: {stderr}"
    );
    assert!(
        stderr.contains("500"),
        "the warning must include the fixed 500ms window, got: {stderr}"
    );
}

#[test]
fn no_watcher_table_does_not_warn() {
    let f = Fixture::new();
    let out = f.run(&["watch"], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("debounce_ms"),
        "a manifest without [watcher] must not warn about debounce_ms, got: {stderr}"
    );
}

#[cfg(unix)]
mod foreground {
    use super::*;
    use std::io::Read;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;
    use std::time::Instant;

    struct Watcher {
        child: std::process::Child,
        stderr: Arc<Mutex<String>>,
    }

    impl Watcher {
        fn spawn(f: &Fixture) -> Self {
            Self::spawn_with_log(f, "patina_core=info")
        }

        fn spawn_with_log(f: &Fixture, rust_log: &str) -> Self {
            let bin = env!("CARGO_BIN_EXE_patina");
            let mut child = Command::new(bin)
                .args(["watch", "--foreground"])
                .env("PATINA_REPO", f.root.as_str())
                .env("HOME", f.home.as_str())
                .env("USERPROFILE", f.home.as_str())
                .env("XDG_STATE_HOME", f.state.as_str())
                .env("LOCALAPPDATA", f.state.as_str())
                .env("RUST_LOG", rust_log)
                .env_remove("PATINA_PROFILE")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn patina watch --foreground");

            let stderr = Arc::new(Mutex::new(String::new()));
            let mut pipe = child.stderr.take().expect("piped stderr");
            let sink = Arc::clone(&stderr);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match pipe.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                            if let Ok(mut guard) = sink.lock() {
                                guard.push_str(&chunk);
                            }
                        }
                    }
                }
            });

            Self { child, stderr }
        }

        fn stderr_snapshot(&self) -> String {
            self.stderr.lock().expect("stderr lock").clone()
        }

        fn wait_for_stderr(&self, needle: &str, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if self.stderr_snapshot().contains(needle) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            self.stderr_snapshot().contains(needle)
        }

        fn count_event_lines(&self, needle: &str) -> usize {
            self.stderr_snapshot()
                .lines()
                .filter(|line| line.contains(needle))
                .count()
        }

        fn signal(&self, name: &str) {
            let pid = self.child.id().to_string();
            let status = Command::new("kill")
                .args([&format!("-{name}"), &pid])
                .status()
                .expect("run kill");
            assert!(status.success(), "kill -{name} {pid} failed");
        }

        fn wait_exit(&mut self, timeout: Duration) -> Option<i32> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                match self.child.try_wait().expect("try_wait") {
                    Some(status) => return status.code(),
                    None => std::thread::sleep(Duration::from_millis(25)),
                }
            }
            let _killed = self.child.kill();
            None
        }
    }

    fn applied_fixture() -> Fixture {
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
            "fixture apply must succeed; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        f
    }

    #[test]
    fn sigint_shuts_down_cleanly_and_exits_zero() {
        let f = applied_fixture();
        let mut watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should log startup; stderr: {}",
            watcher.stderr_snapshot()
        );

        watcher.signal("INT");

        let exit = watcher.wait_exit(Duration::from_secs(1));
        assert_eq!(
            exit,
            Some(0),
            "SIGINT must exit 0 within 1s; stderr: {}",
            watcher.stderr_snapshot()
        );
        assert!(
            watcher.stderr_snapshot().contains("shutdown"),
            "stderr must contain `shutdown`; got: {}",
            watcher.stderr_snapshot()
        );
    }

    #[test]
    fn sigterm_follows_the_same_clean_exit_path_as_sigint() {
        let f = applied_fixture();
        let mut watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should log startup; stderr: {}",
            watcher.stderr_snapshot()
        );

        watcher.signal("TERM");

        let exit = watcher.wait_exit(Duration::from_secs(1));
        assert_eq!(
            exit,
            Some(0),
            "SIGTERM must exit 0 within 1s; stderr: {}",
            watcher.stderr_snapshot()
        );
        assert!(
            watcher.stderr_snapshot().contains("shutdown"),
            "stderr must contain `shutdown`; got: {}",
            watcher.stderr_snapshot()
        );
    }

    #[test]
    fn logs_its_subscription_set_on_startup() {
        let f = applied_fixture();
        let watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_subscriptions", Duration::from_secs(5)),
            "watcher should log its subscription set; stderr: {}",
            watcher.stderr_snapshot()
        );
        let stderr = watcher.stderr_snapshot();
        assert!(
            stderr.contains("rc"),
            "the logged subscription set must include the watched source `rc`; got: {stderr}"
        );

        let mut watcher = watcher;
        watcher.signal("TERM");
        let _exit = watcher.wait_exit(Duration::from_secs(2));
    }

    fn applied_copy_fixture() -> Fixture {
        let f = Fixture::new();
        let module = f.module(
            "git",
            "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
        );
        fs_err::write(module.join("gitconfig"), "[user]\n  name = a\n").expect("write source");
        let out = f.apply(&["--yes"]);
        assert_eq!(
            code(&out),
            0,
            "fixture apply must succeed; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        f
    }

    fn commit_count(f: &Fixture) -> usize {
        let journal = f.state_root().join("journal");
        let Ok(entries) = fs_err::read_dir(journal.as_std_path()) else {
            return 0;
        };
        entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(".COMMIT"))
            })
            .count()
    }

    #[test]
    fn five_touches_within_the_debounce_window_coalesce_to_one_reapply() {
        let f = applied_copy_fixture();
        let watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should start; stderr: {}",
            watcher.stderr_snapshot()
        );

        let source = f.root.join("git").join("gitconfig");
        for i in 0..5 {
            fs_err::write(source.as_std_path(), format!("[user]\n  name = a{i}\n"))
                .expect("rewrite source");
        }

        assert!(
            watcher.wait_for_stderr("re_apply", Duration::from_secs(5)),
            "the coalesced burst should drive one re_apply; stderr: {}",
            watcher.stderr_snapshot()
        );
        std::thread::sleep(Duration::from_secs(1));

        let reapplies = watcher.count_event_lines("patina_core: re_apply re_apply_id");
        assert_eq!(
            watcher.count_event_lines("re_apply_failed"),
            0,
            "no re-apply should fail; stderr: {}",
            watcher.stderr_snapshot()
        );
        assert_eq!(
            reapplies,
            1,
            "the five-touch burst must coalesce into exactly one re_apply; stderr: {}",
            watcher.stderr_snapshot()
        );

        let mut watcher = watcher;
        watcher.signal("TERM");
        let _exit = watcher.wait_exit(Duration::from_secs(2));
    }

    #[test]
    fn a_parallel_cli_apply_triggers_a_journal_rescan() {
        let f = applied_copy_fixture();
        let watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should start; stderr: {}",
            watcher.stderr_snapshot()
        );

        let extra = f.module(
            "extra",
            "[[file]]\nsource = \"extra_src\"\ntarget = \"~/extra_out\"\nmode = \"copy\"\n",
        );
        fs_err::write(extra.join("extra_src"), b"extra\n").expect("write extra source");

        let out = f.apply(&["--yes"]);
        assert_eq!(code(&out), 0, "parallel CLI apply must succeed");

        assert!(
            watcher.wait_for_stderr("journal_rescan", Duration::from_secs(5)),
            "the watcher must rescan on the CLI's new journal; stderr: {}",
            watcher.stderr_snapshot()
        );

        std::thread::sleep(Duration::from_secs(1));
        let rescans = watcher.count_event_lines("journal_rescan");
        assert!(
            rescans < 10,
            "a single CLI apply must not drive an unbounded rescan loop, saw {rescans}; stderr: {}",
            watcher.stderr_snapshot()
        );

        let mut watcher = watcher;
        watcher.signal("TERM");
        let _exit = watcher.wait_exit(Duration::from_secs(2));
    }

    fn drift_entries_for(f: &Fixture, needle: &str) -> usize {
        let path = f.state_root().join("drift.cache");
        match patina_core::load_drift_cache_file(&path) {
            Ok(cache) => cache
                .entries
                .iter()
                .filter(|e| e.target.as_str().contains(needle))
                .count(),
            Err(_) => 0,
        }
    }

    #[test]
    fn an_external_target_edit_logs_drift_and_populates_the_cache() {
        let f = applied_copy_fixture();
        let target = f.home.join(".gitconfig");
        let applied = fs_err::read_to_string(target.as_std_path()).expect("read applied target");

        let watcher = Watcher::spawn(&f);
        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should start; stderr: {}",
            watcher.stderr_snapshot()
        );

        let drifted = format!("{applied}; drifted = true\n");
        assert_ne!(drifted, applied, "the overwrite must change the bytes");
        // On macOS, FSEvents arms its stream asynchronously after `watch()`
        // returns, so a single write landing in the startup gap is lost, not
        // delayed. Drift detection is idempotent over repeated identical
        // writes (no re-apply, no journal write), so re-write until the
        // armed stream observes one.
        let drift_logged = {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                fs_err::write(target.as_std_path(), &drifted).expect("overwrite target");
                if watcher.wait_for_stderr("drift", Duration::from_secs(1)) {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
            }
        };
        assert!(
            drift_logged,
            "the external edit must log a drift event; stderr: {}",
            watcher.stderr_snapshot()
        );

        let cache_populated = {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if drift_entries_for(&f, ".gitconfig") >= 1 {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        };
        assert!(
            cache_populated,
            "the drift cache must hold an entry for .gitconfig; stderr: {}",
            watcher.stderr_snapshot()
        );

        assert_eq!(
            watcher.count_event_lines("patina_core: re_apply re_apply_id"),
            0,
            "a content-target edit must not drive a re-apply; stderr: {}",
            watcher.stderr_snapshot()
        );

        let status = f.run(&["status", "--json"], &[]);
        assert_eq!(
            code(&status),
            0,
            "status --json must succeed; stderr: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        let stdout = String::from_utf8_lossy(&status.stdout);
        let doc: serde_json::Value =
            serde_json::from_str(stdout.trim()).expect("status --json emits one JSON document");
        let drifted_gitconfig = doc
            .get("files")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|files| {
                files.iter().any(|entry| {
                    let path = entry.get("path").and_then(serde_json::Value::as_str);
                    let state = entry.get("state").and_then(serde_json::Value::as_str);
                    path.is_some_and(|p| p.contains(".gitconfig")) && state == Some("drifted")
                })
            });
        assert!(
            drifted_gitconfig,
            "status JSON must report .gitconfig as drifted from its own live re-hash; got: {stdout}"
        );
        let drifted_count = doc.get("drifted").and_then(serde_json::Value::as_u64);
        assert!(
            drifted_count.is_some_and(|n| n >= 1),
            "the aggregate drifted counter must be >= 1; got: {drifted_count:?}"
        );

        let mut watcher = watcher;
        watcher.signal("TERM");
        let _exit = watcher.wait_exit(Duration::from_secs(2));
    }

    #[test]
    fn a_watcher_reapply_commits_exactly_one_new_journal_record() {
        let f = applied_copy_fixture();
        assert_eq!(
            commit_count(&f),
            1,
            "the fixture's initial apply commits one record"
        );

        let watcher = Watcher::spawn(&f);
        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should start; stderr: {}",
            watcher.stderr_snapshot()
        );

        std::thread::sleep(Duration::from_millis(1100));

        let source = f.root.join("git").join("gitconfig");
        fs_err::write(source.as_std_path(), "[user]\n  name = changed\n").expect("rewrite source");

        assert!(
            watcher.wait_for_stderr("patina_core: re_apply re_apply_id", Duration::from_secs(5)),
            "the source edit must drive a re_apply; stderr: {}",
            watcher.stderr_snapshot()
        );

        let two_commits = {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if commit_count(&f) >= 2 {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        };

        assert_eq!(
            watcher.count_event_lines("lock_contention_skip"),
            0,
            "an uncontended watcher re-apply must not log a contention skip; stderr: {}",
            watcher.stderr_snapshot()
        );
        assert!(
            two_commits,
            "the watcher's re-apply commits exactly one new record on top of the initial apply, \
             saw {} COMMIT(s); stderr: {}",
            commit_count(&f),
            watcher.stderr_snapshot()
        );

        let mut watcher = watcher;
        watcher.signal("TERM");
        let _exit = watcher.wait_exit(Duration::from_secs(2));
    }
}
