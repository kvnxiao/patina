// Both lints are triggered only from the `#[cfg(unix)] mod foreground` helper
// methods below (their `.expect()` calls and the `&buf[..n]` read slice);
// `allow-*-in-tests` covers `#[cfg(test)]` modules but not the helper methods
// in tests/*.rs integration crates. On non-unix targets that module is absent,
// so the expectation would be unfulfilled — gate it to unix to match.
#![cfg_attr(
    unix,
    expect(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "integration tests use .expect() on fixtures and a bounded read-buffer slice; allow-*-in-tests covers #[cfg(test)] modules but not the helper methods in tests/*.rs integration crates."
    )
)]

//! Integration tests for `patina watch --foreground`, covering signal
//! handling and the `[watcher] debounce_ms`
//! forward-compatible warning.
//!
//! The foreground watcher is a long-running process, so these tests cannot use
//! the shared [`Fixture::run`] helper (which blocks on `Output` to completion).
//! Instead they spawn the binary with piped stderr, drain stderr on a reader
//! thread, send a real signal, and assert on the exit status and captured
//! stderr. Signal-sending is POSIX-only (`kill(1)`), so the SIGINT / SIGTERM
//! tests are `#[cfg(unix)]`; the debounce-warning test is cross-platform.

mod common;

use common::Fixture;
// `code` and `Command` are used only by the `#[cfg(unix)] mod foreground`
// (through its `use super::*`); they are unused on non-unix targets.
#[cfg(unix)]
use common::code;
#[cfg(unix)]
use std::process::Command;

/// `patina watch` without `--foreground` reports the not-yet-wired service
/// install and, when the root manifest declares the ignored `[watcher]
/// debounce_ms` key, surfaces the forward-compatible warning. This path runs
/// to completion, so the blocking `Fixture::run`
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
        // A running foreground watcher, SIGINT -> exit 0 within 1s and
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
        // SIGTERM produces the same clean-exit path as SIGINT (exit 0,
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
        // The foreground watcher logs its
        // computed subscription set, naming the watched source path, so a
        // harness can inspect it from stderr.
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
        // A burst of writes to a watched source must coalesce into
        // exactly one `re_apply` event (the 500ms debounce window swallows the
        // burst).
        //
        // The five writes are issued back-to-back with NO inter-write sleep, on
        // purpose. An earlier revision slept 20ms between writes to mimic an
        // editor's multi-event save; under a loaded CI runner those sleeps
        // stretched (a 20ms `sleep` is a yield point and can be descheduled into
        // hundreds of ms), spreading the burst across more than the 500ms
        // window. It then split into several debounce batches and fired one
        // re-apply per straggler — up to one per write — flaking the `== 1`
        // assertion below. A bare `write` loop has no yield point, so the burst
        // stays well inside one window regardless of scheduler load; do not
        // reintroduce a per-write sleep here.
        let f = applied_copy_fixture();
        let watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should start; stderr: {}",
            watcher.stderr_snapshot()
        );

        // Five rapid writes to the watched source, back-to-back.
        let source = f.root.join("git").join("gitconfig");
        for i in 0..5 {
            fs_err::write(source.as_std_path(), format!("[user]\n  name = a{i}\n"))
                .expect("rewrite source");
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
            "the five-touch burst must coalesce into exactly one re_apply; stderr: {}",
            watcher.stderr_snapshot()
        );

        let mut watcher = watcher;
        watcher.signal("TERM");
        let _exit = watcher.wait_exit(Duration::from_secs(2));
    }

    #[test]
    fn a_parallel_cli_apply_triggers_a_journal_rescan() {
        // A parallel `patina apply --yes` writes a new `.plan`/`.COMMIT`
        // under the watched journal dir; the watcher logs a `journal_rescan`
        // event and does not enter an unbounded re-apply loop.
        let f = applied_copy_fixture();
        let watcher = Watcher::spawn(&f);

        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should start; stderr: {}",
            watcher.stderr_snapshot()
        );

        // This scenario models an external `patina apply` that COMMITS new state
        // while the watcher runs: the watcher must detect the new journal
        // record under the watched journal dir and rescan. Since
        // an unchanged re-apply is a full no-op that writes no
        // journal, the parallel apply must change committed state
        // to produce the `.COMMIT` this scenario is about — an idempotent
        // re-apply correctly produces neither a journal nor a rescan.
        //
        // Introduce a brand-new entry AFTER the watcher has subscribed, so it
        // is outside the current subscription set: the parallel apply performs
        // a real Create and commits a fresh journal record, and the watcher
        // reacts to that journal write (not to the new source/target, which it
        // is not yet watching). This isolates the "external apply commits ->
        // journal_rescan" behaviour the loop guard is about.
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

    /// Count drift-cache entries naming `needle`, or 0 when the cache is
    /// absent / undecodable. Reads the on-disk cache through the engine's own
    /// decoder so the assertion sees exactly what `patina debug drift-cache`
    /// would.
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
        // The platform-independent, deterministic slice:
        // a running watcher over an applied copy-mode `~/.gitconfig`, when the
        // target is overwritten with bytes that hash differently, logs a `drift`
        // event, records the divergence in `<state>/drift.cache` with the
        // recorded and observed hashes, and `patina status` then reports the
        // target DRIFTED from its own live re-hash.
        //
        // The notification *count* (the "exactly one") is asserted
        // deterministically by the `patina-core` drift unit tests against the
        // capture sink: the CLI binary always uses the real
        // `notify-rust` sink, which a headless CI runner cannot capture, so the
        // count is not assertable here. This test owns the observable on-disk
        // and status side-effects instead.
        let f = applied_copy_fixture();
        let target = f.home.join(".gitconfig");
        // The fixture applied the source bytes to the target; capture the
        // recorded content so the overwrite is genuinely divergent.
        let applied = fs_err::read_to_string(target.as_std_path()).expect("read applied target");

        let watcher = Watcher::spawn(&f);
        assert!(
            watcher.wait_for_stderr("watch_started", Duration::from_secs(5)),
            "watcher should start; stderr: {}",
            watcher.stderr_snapshot()
        );

        // Overwrite the target out-of-band with content that hashes differently
        // (H2 ≠ H1). This is the "modified outside Patina" edit drift detects.
        let drifted = format!("{applied}; drifted = true\n");
        assert_ne!(drifted, applied, "the overwrite must change the bytes");
        fs_err::write(target.as_std_path(), &drifted).expect("overwrite target");

        // The watcher logs a `drift` event for the divergent target.
        assert!(
            watcher.wait_for_stderr("drift", Duration::from_secs(5)),
            "the external edit must log a drift event; stderr: {}",
            watcher.stderr_snapshot()
        );

        // The drift cache records the divergence (the on-disk surface).
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

        // The watcher must NOT re-apply the divergent target (a re-apply would
        // rewrite it back to source and re-trigger): no re_apply event fired.
        assert_eq!(
            watcher.count_event_lines("patina_core: re_apply re_apply_id"),
            0,
            "a content-target edit must not drive a re-apply; stderr: {}",
            watcher.stderr_snapshot()
        );

        // `patina status --json` reports the target DRIFTED — derived
        // from the live re-hash, independent of the cache.
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
        // The `files` array contains an entry whose path names `.gitconfig`
        // with `state = "drifted"`.
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
        // The aggregate `drifted` counter is at least 1.
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
        // A single watched-source edit drives exactly
        // one watcher re-apply, which commits exactly one new journal record on
        // top of the fixture's initial apply (two COMMITs total). This is the
        // deterministic, single-process slice of the two-committed-plans
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
