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
            let bin = env!("CARGO_BIN_EXE_patina");
            let mut child = Command::new(bin)
                .args(["watch", "--foreground"])
                .env("PATINA_REPO", f.root.as_str())
                .env("HOME", f.home.as_str())
                .env("USERPROFILE", f.home.as_str())
                .env("XDG_STATE_HOME", f.state.as_str())
                .env("LOCALAPPDATA", f.state.as_str())
                .env("RUST_LOG", "patina_core=info")
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
}
