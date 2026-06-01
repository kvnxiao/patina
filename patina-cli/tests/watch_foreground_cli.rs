#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "integration tests use .expect() on fixtures and a bounded read-buffer slice; allow-*-in-tests covers #[cfg(test)] modules but not the helper methods in tests/*.rs integration crates."
)]

//! Integration tests for `patina watch --foreground` (SPEC-0003 REQ-004 /
//! REQ-006; CHK-008, CHK-009 surface, and the `[watcher] debounce_ms`
//! forward-compatible warning).
//!
//! The foreground watcher is a long-running process, so these tests cannot use
//! the shared [`Fixture::run`] helper (which blocks on `Output` to completion).
//! Instead they spawn the binary with piped stderr, drain stderr on a reader
//! thread, send a real signal, and assert on the exit status and captured
//! stderr. Signal-sending is POSIX-only (`kill(1)`), so the SIGINT / SIGTERM
//! tests are `#[cfg(unix)]`; the debounce-warning test is cross-platform.

mod common;

use common::Fixture;
use common::code;
use std::process::Command;

/// `patina watch` without `--foreground` reports the not-yet-wired service
/// install and, when the root manifest declares the ignored `[watcher]
/// debounce_ms` key, surfaces the forward-compatible warning (REQ-006 /
/// DEC-002). This path runs to completion, so the blocking `Fixture::run`
/// helper is fine.
#[test]
fn debounce_ms_key_in_root_manifest_warns() {
    let f = Fixture::new();
    // Append the ignored watcher knob to the root manifest the fixture wrote.
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
    // The 500ms window is hardcoded; the warning must name it.
    assert!(
        stderr.contains("500"),
        "the warning must name the fixed 500ms window, got: {stderr}"
    );
}

/// A root manifest without the `[watcher]` table produces no debounce warning.
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

    /// A spawned foreground watcher with a background stderr reader.
    struct Watcher {
        child: std::process::Child,
        stderr: Arc<Mutex<String>>,
    }

    impl Watcher {
        /// Spawn `patina watch --foreground` against the fixture's isolated
        /// repo / state / home, with `RUST_LOG=patina_core=info` and piped
        /// stderr drained on a reader thread.
        fn spawn(f: &Fixture) -> Self {
            Self::spawn_with_log(f, "patina_core=info")
        }

        /// Spawn the foreground watcher with an explicit `RUST_LOG` filter, so
        /// a test that asserts on a debug-level event (e.g.
        /// `lock_contention_skip`) can raise the watcher to
        /// `patina_core=debug`.
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

        /// The stderr captured so far.
        fn stderr_snapshot(&self) -> String {
            self.stderr.lock().expect("stderr lock").clone()
        }

        /// Block until `needle` appears in stderr or `timeout` elapses.
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

        /// The number of stderr *lines* containing `needle`. Counts log events
        /// rather than raw substrings: a single structured event line repeats
        /// its event name across its field names (`re_apply re_apply_id=…`), so
        /// a substring count would over-count one event many times.
        fn count_event_lines(&self, needle: &str) -> usize {
            self.stderr_snapshot()
                .lines()
                .filter(|line| line.contains(needle))
                .count()
        }

        /// Send a POSIX signal by name via `kill(1)` to the child's pid.
        fn signal(&self, name: &str) {
            let pid = self.child.id().to_string();
            let status = Command::new("kill")
                .args([&format!("-{name}"), &pid])
                .status()
                .expect("run kill");
            assert!(status.success(), "kill -{name} {pid} failed");
        }

        /// Wait for the child to exit, up to `timeout`. Returns the exit code,
        /// or `None` if it had not exited in time (after which it is killed).
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

    /// Build a fixture with one applied symlink module so the watcher has a
    /// committed journal to compute subscriptions from.
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
        // CHK-008: a running foreground watcher, SIGINT -> exit 0 within 1s and
        // stderr contains `shutdown`.
        let f = applied_fixture();
        let mut watcher = Watcher::spawn(&f);

        // Wait until the loop has started before signalling, so the signal hits
        // the select-loop rather than racing process startup.
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
        // REQ-004: SIGTERM produces the same clean-exit path as SIGINT (exit 0,
        // `shutdown` logged).
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
        // CHK-009 surface (REQ-005 / T-008): the foreground watcher logs its
        // computed subscription set, naming the watched source path, so a
        // harness can inspect it from stderr. The mutating re-apply is T-009.
        let f = applied_fixture();
        let watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_subscriptions", Duration::from_secs(5)),
            "watcher should log its subscription set; stderr: {}",
            watcher.stderr_snapshot()
        );
        let stderr = watcher.stderr_snapshot();
        // The applied module's source `rc` is a watched source path; its path
        // must appear in the logged subscription set.
        assert!(
            stderr.contains("rc"),
            "the logged subscription set must name the watched source `rc`; got: {stderr}"
        );

        // Clean up the long-running child.
        let mut watcher = watcher;
        watcher.signal("TERM");
        let _exit = watcher.wait_exit(Duration::from_secs(2));
    }

    /// Build a fixture with one applied copy-mode module so the watcher's
    /// re-apply has a content target to re-materialize and the source is a
    /// watched path a test can edit to trigger a re-apply.
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

    /// Count the live `<ts>.COMMIT` sentinels in the fixture's journal dir.
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
        // CHK-010: five touches of a watched source within a 100ms burst, then
        // a wait, must coalesce into exactly one `re_apply` event (the 500ms
        // debounce window swallowed the burst).
        let f = applied_copy_fixture();
        let watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should start; stderr: {}",
            watcher.stderr_snapshot()
        );

        // Five rapid writes to the watched source within ~100ms.
        let source = f.root.join("git").join("gitconfig");
        for i in 0..5 {
            fs_err::write(source.as_std_path(), format!("[user]\n  name = a{i}\n"))
                .expect("rewrite source");
            std::thread::sleep(Duration::from_millis(20));
        }

        // Wait for the single coalesced re-apply to fire and settle.
        assert!(
            watcher.wait_for_stderr("re_apply", Duration::from_secs(5)),
            "the coalesced burst should drive one re_apply; stderr: {}",
            watcher.stderr_snapshot()
        );
        std::thread::sleep(Duration::from_secs(1));

        // The success event line is `patina_core: re_apply re_apply_id=…`; the
        // trailing space distinguishes it from the `re_apply_failed` event.
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
            "the five-touch burst must coalesce into exactly one re_apply (CHK-010); stderr: {}",
            watcher.stderr_snapshot()
        );

        let mut watcher = watcher;
        watcher.signal("TERM");
        let _exit = watcher.wait_exit(Duration::from_secs(2));
    }

    #[test]
    fn a_parallel_cli_apply_triggers_a_journal_rescan() {
        // CHK-017: a parallel `patina apply --yes` writes a new `.plan`/`.COMMIT`
        // under the watched journal dir; the watcher logs a `journal_rescan`
        // event and does not enter an unbounded re-apply loop.
        let f = applied_copy_fixture();
        let watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should start; stderr: {}",
            watcher.stderr_snapshot()
        );

        // A parallel CLI apply writes a fresh journal record (no source edit
        // needed — re-applying unchanged source still commits a new COMMIT).
        let out = f.apply(&["--yes"]);
        assert_eq!(code(&out), 0, "parallel CLI apply must succeed");

        assert!(
            watcher.wait_for_stderr("journal_rescan", Duration::from_secs(5)),
            "the watcher must rescan on the CLI's new journal (CHK-017); stderr: {}",
            watcher.stderr_snapshot()
        );

        // No unbounded loop: after settling, the rescan count stays bounded
        // (a single CLI apply drives a small, finite number of rescans, not a
        // runaway). A runaway loop would push this into the dozens.
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

    #[test]
    fn a_watcher_reapply_commits_exactly_one_new_journal_record() {
        // REQ-006 / CHK-013 surface: a single watched-source edit drives exactly
        // one watcher re-apply, which commits exactly one new journal record on
        // top of the fixture's initial apply (two COMMITs total). This is the
        // deterministic, single-process slice of CHK-013's two-committed-plans
        // contract; the full concurrent-CLI race is exercised by the engine's
        // `NonBlocking` unit tests.
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

        // The journal `<ts>` is keyed to whole-second granularity (the hoisted
        // `current_timestamp`), so a re-apply landing in the same wall-clock
        // second as the fixture apply would reuse its timestamp and overwrite
        // the COMMIT rather than add a second one. Wait past the current second
        // boundary before editing so the re-apply gets a distinct timestamp —
        // the same separation a real user edit (≥500ms debounce after any prior
        // apply) gets in practice.
        std::thread::sleep(Duration::from_millis(1100));

        let source = f.root.join("git").join("gitconfig");
        fs_err::write(source.as_std_path(), "[user]\n  name = changed\n").expect("rewrite source");

        assert!(
            watcher.wait_for_stderr("patina_core: re_apply re_apply_id", Duration::from_secs(5)),
            "the source edit must drive a re_apply; stderr: {}",
            watcher.stderr_snapshot()
        );

        // Poll for the new COMMIT rather than sleeping a fixed interval: the
        // re_apply event is logged after the engine commits, so the COMMIT is
        // already on disk, but polling absorbs scheduler jitter under parallel
        // test load.
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

        // The watcher held the lock for its own re-apply and committed one new
        // record; no contention skip occurred (no competing holder).
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
