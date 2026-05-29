#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for the formalized CLI exit-code contract (REQ-022,
//! CHK-036).
//!
//! Each test builds a self-contained tempdir dotfiles repository, points
//! `PATINA_REPO` at it, and isolates the per-machine state directory under
//! the tempdir so the apply never touches the developer's real `$HOME`.
//! The binary runs as a subprocess (stdin therefore is not a TTY) so the
//! observed exit code is the real process status `main` produced through
//! the single `cli::resolve_exit_code` funnel.
//!
//! | Scenario                                   | Expected code |
//! |--------------------------------------------|---------------|
//! | `must_succeed` `pre_apply` hook fails      | 2             |
//! | `must_succeed` `post_apply` hook fails     | 3             |
//! | `patina.toml` has a TOML syntax error      | 1             |
//! | exclusive lock held past the timeout cap   | 4             |
//! | apply succeeds end-to-end                  | 0             |
//!
//! The declined-prompt case (exit 5) needs an interactive TTY, which a
//! subprocess pipe cannot simulate; it is covered by the in-process unit
//! tests on the injected `PromptReader` in `cmd::apply` and `cmd::rollback`.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::HostOs;
use patina_core::LockKind;
use std::process::Command;
use std::process::Output;
use std::time::Duration;
use tempfile::TempDir;

/// A prepared fixture: an isolated repo + state dir, ready to invoke
/// `patina apply` against.
struct Fixture {
    _temp: TempDir,
    root: Utf8PathBuf,
    home: Utf8PathBuf,
    state: Utf8PathBuf,
}

impl Fixture {
    /// Build a fixture with a root manifest and an empty home/state tree.
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        let repo = root.join("repo");
        let home = root.join("home");
        let state = root.join("state");
        fs_err::create_dir_all(&repo).expect("mkdir repo");
        fs_err::create_dir_all(&home).expect("mkdir home");
        fs_err::create_dir_all(&state).expect("mkdir state");
        fs_err::write(repo.join("patina.toml"), "[patina]\nroot = true\n")
            .expect("write root manifest");
        Self {
            _temp: temp,
            root: repo,
            home,
            state,
        }
    }

    /// Write a module directory with the given `patina.toml` body and an
    /// optional source file.
    fn module(&self, name: &str, manifest: &str) -> Utf8PathBuf {
        let dir = self.root.join(name);
        fs_err::create_dir_all(&dir).expect("mkdir module");
        fs_err::write(dir.join("patina.toml"), manifest).expect("write module manifest");
        dir
    }

    /// The per-machine state-directory root the subprocess will resolve,
    /// computed from this fixture's own isolated env values (not the
    /// process environment) so concurrent tests never collide.
    fn state_root(&self) -> Utf8PathBuf {
        patina_core::state_dir::resolve_with_env(HostOs::current(), |name| match name {
            "XDG_STATE_HOME" | "LOCALAPPDATA" => Some(self.state.as_str().to_owned()),
            "HOME" | "USERPROFILE" => Some(self.home.as_str().to_owned()),
            _ => None,
        })
        .expect("resolve fixture state dir")
    }

    /// Invoke `patina apply` with `args`, isolating repo + state + home.
    /// Extra environment pairs are layered on last.
    fn apply_with_env(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let bin = env!("CARGO_BIN_EXE_patina");
        let mut cmd = Command::new(bin);
        cmd.arg("apply")
            .args(args)
            .env("PATINA_REPO", self.root.as_str())
            .env("HOME", self.home.as_str())
            .env("USERPROFILE", self.home.as_str())
            .env("XDG_STATE_HOME", self.state.as_str())
            .env("LOCALAPPDATA", self.state.as_str())
            .env_remove("PATINA_PROFILE");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn patina")
    }

    /// Invoke `patina apply` with `args` and no extra environment.
    fn apply(&self, args: &[&str]) -> Output {
        self.apply_with_env(args, &[])
    }
}

/// The numeric exit code, or a panic if the process was signalled.
fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

/// A module with a single copy `[[file]]` plus a hook of the given event /
/// command. Used to drive the hook-failure exit codes.
fn hook_module(f: &Fixture, event: &str, command: &str) {
    let module = f.module(
        "shell",
        &format!(
            "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n\n\
             [[hook]]\nevent = \"{event}\"\ncommand = \"{command}\"\n"
        ),
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");
}

#[test]
fn pre_apply_hook_failure_exits_2() {
    // CHK-036: a `pre_apply` hook with `command = "false"` and the default
    // `must_succeed = true` aborts before any file operation, exit code 2.
    let f = Fixture::new();
    hook_module(&f, "pre_apply", "false");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        2,
        "a must_succeed pre_apply hook failure must exit 2; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs_err::symlink_metadata(f.home.join(".rc")).is_err(),
        "no file operation may run when a pre_apply hook aborts the apply"
    );
}

#[test]
fn post_apply_hook_failure_exits_3() {
    // A `post_apply` hook returning non-zero under the default
    // `must_succeed = true` rolls back the file operations, exit code 3.
    let f = Fixture::new();
    hook_module(&f, "post_apply", "exit 1");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        3,
        "a must_succeed post_apply hook failure must exit 3; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs_err::symlink_metadata(f.home.join(".rc")).is_err(),
        "the copied file must be reversed on the post_apply rollback"
    );
}

#[test]
fn toml_syntax_error_exits_1_and_names_the_failure() {
    // A `patina.toml` that is not valid TOML is a generic failure: exit 1
    // with the parse error surfaced on stderr.
    let f = Fixture::new();
    // `=` with no value is a TOML syntax error the parser rejects.
    f.module("broken", "[[file]]\nsource =\n");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        1,
        "a TOML syntax error must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.stderr.is_empty(),
        "stderr must carry the parse-failure message"
    );
}

#[test]
fn exclusive_lock_timeout_exits_4() {
    // A second apply that cannot acquire the held exclusive lock within the
    // (test-shrunk) timeout cap exits 4. We hold the lock in-process and
    // shrink the subprocess's cap via PATINA_LOCK_TIMEOUT_MS so the test
    // does not wait the production minute.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "payload\n").expect("write source");

    // Hold the exclusive lock for the whole subprocess window. The engine
    // resolves the lock to `<state>/patina/lock`; match that path here.
    let lock_path = f.state_root().join("lock");
    let _held = patina_core::acquire_lock(&lock_path, LockKind::Exclusive, Duration::from_secs(5))
        .expect("hold the exclusive lock for the duration of the subprocess apply");

    let out = f.apply_with_env(&["--yes"], &[("PATINA_LOCK_TIMEOUT_MS", "200")]);

    assert_eq!(
        code(&out),
        4,
        "an exclusive-lock timeout must exit 4; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn successful_apply_exits_0() {
    // A clean apply that lands its single file exits 0.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "applied\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "a successful apply must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".rc")).expect("target written"),
        "applied\n"
    );
}
