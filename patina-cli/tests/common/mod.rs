#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]
#![allow(
    dead_code,
    reason = "this shared fixture module is included by several integration-test crates via `mod common;`; each crate uses a subset of the helpers, so methods unused by one crate would be flagged dead there but are live in another. `allow` (not `expect`) because the set of used helpers differs per including crate, so no single expectation is fulfilled everywhere."
)]

//! Shared test fixture for the `patina apply` integration suites.
//!
//! Each test builds a self-contained tempdir dotfiles repository and points
//! `PATINA_REPO` at it. It isolates the per-machine state directory under
//! the tempdir, so the apply never touches the developer's real `$HOME`.
//! The binary runs as a subprocess, so its stdin is not a TTY and it
//! exercises the non-interactive path.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::HostOs;
use std::process::Command;
use std::process::Output;
use tempfile::TempDir;

/// A prepared fixture with an isolated repo and state dir, ready to invoke
/// `patina apply` against.
pub struct Fixture {
    _temp: TempDir,
    pub root: Utf8PathBuf,
    pub home: Utf8PathBuf,
    pub state: Utf8PathBuf,
}

impl Fixture {
    /// Build a fixture with a root manifest and an empty home/state tree.
    pub fn new() -> Self {
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

    /// Append a `[[remote]]` declaration to the root manifest, which is the
    /// only place a remote is declared.
    pub fn declare_remote(&self, name: &str, url: &str, git_ref: Option<&str>) {
        let manifest = self.root.join("patina.toml");
        let existing = fs_err::read_to_string(&manifest).expect("read root manifest");
        let tracked = git_ref.map_or_else(String::new, |value| format!("ref = \"{value}\"\n"));
        let body = format!("{existing}\n[[remote]]\nname = \"{name}\"\nurl = \"{url}\"\n{tracked}");
        fs_err::write(&manifest, body).expect("write root manifest");
    }

    /// Write a module directory with the given `patina.toml` body and an
    /// optional source file.
    pub fn module(&self, name: &str, manifest: &str) -> Utf8PathBuf {
        let dir = self.root.join(name);
        fs_err::create_dir_all(&dir).expect("mkdir module");
        fs_err::write(dir.join("patina.toml"), manifest).expect("write module manifest");
        dir
    }

    /// The per-machine state-directory root the subprocess will resolve,
    /// computed from this fixture's own isolated env values (not the
    /// process environment) so concurrent tests never collide.
    pub fn state_root(&self) -> Utf8PathBuf {
        patina_core::state_dir::resolve_with_env(HostOs::current(), |name| match name {
            "XDG_STATE_HOME" | "LOCALAPPDATA" => Some(self.state.as_str().to_owned()),
            "HOME" | "USERPROFILE" => Some(self.home.as_str().to_owned()),
            _ => None,
        })
        .expect("resolve fixture state dir")
    }

    /// Invoke `patina` with an arbitrary `args` vector, isolating repo,
    /// state, and home the same way every subcommand requires. The caller
    /// supplies the subcommand and its flags as the leading elements of
    /// `args`; extra environment pairs are layered on last.
    pub fn run(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let bin = env!("CARGO_BIN_EXE_patina");
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .env("PATINA_REPO", self.root.as_str())
            .env("HOME", self.home.as_str())
            .env("USERPROFILE", self.home.as_str())
            .env("XDG_STATE_HOME", self.state.as_str())
            // Windows resolves the state dir from `LOCALAPPDATA`. Isolate it
            // per test so parallel tests never share one journal, lock, or
            // backup tree. A shared tree would let one test's
            // crash-recovery pass reverse another test's just-applied
            // files.
            .env("LOCALAPPDATA", self.state.as_str())
            .env_remove("PATINA_PROFILE");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn patina")
    }

    /// Invoke `patina` with `args` and a working directory of `cwd`,
    /// isolating repo, state, and home the same way [`Fixture::run`] does.
    /// Commands whose behaviour depends on the process CWD use this (e.g.
    /// `doctor --fix` records the CWD as the default repository).
    pub fn run_in(&self, cwd: &Utf8Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let bin = env!("CARGO_BIN_EXE_patina");
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .current_dir(cwd.as_std_path())
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

    /// Invoke `patina apply` with `args`, isolating repo, state, and home.
    /// Extra environment pairs are layered on last. Delegates to
    /// [`Fixture::run`] with `apply` prepended.
    pub fn apply_with_env(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut full = Vec::with_capacity(args.len() + 1);
        full.push("apply");
        full.extend_from_slice(args);
        self.run(&full, extra)
    }

    /// Invoke `patina apply` with `args` and no extra environment.
    pub fn apply(&self, args: &[&str]) -> Output {
        self.apply_with_env(args, &[])
    }
}

/// The numeric exit code, or a panic if the process was signalled.
pub fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

/// Create a file symlink with the right platform primitive.
#[cfg(unix)]
pub fn symlink_file(source: &Utf8Path, link: &Utf8Path) {
    std::os::unix::fs::symlink(source.as_std_path(), link.as_std_path()).expect("create symlink");
}

/// Create a file symlink with the right platform primitive.
#[cfg(windows)]
pub fn symlink_file(source: &Utf8Path, link: &Utf8Path) {
    std::os::windows::fs::symlink_file(source.as_std_path(), link.as_std_path())
        .expect("create symlink");
}

/// Run `git` in `cwd` with a pinned identity and committer/author date,
/// independent of the developer's global git config, so fixture commits have
/// stable, clock-independent SHAs.
pub fn git_in(cwd: &Utf8Path, epoch: i64, args: &[&str]) -> String {
    let date = format!("{epoch} +0000");
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd.as_std_path())
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A throwaway origin repository living outside the dotfiles repo, so module
/// discovery never sees it.
pub struct Origin {
    pub dir: Utf8PathBuf,
}

impl Origin {
    pub fn new(f: &Fixture, name: &str, epoch: i64) -> Self {
        let dir = f.home.join(".origins").join(name);
        fs_err::create_dir_all(dir.as_std_path()).expect("mkdir origin");
        git_in(&dir, epoch, &["init", "--quiet", "-b", "main"]);
        Self { dir }
    }

    /// The origin path spelled for embedding in a TOML basic string. On
    /// Windows a native path's backslashes would read as escape sequences,
    /// but git accepts the forward-slash form of a Windows path.
    pub fn url(&self) -> String {
        self.dir.as_str().replace('\\', "/")
    }

    /// Write `files` into the origin and commit them at `epoch`, returning the
    /// commit SHA.
    pub fn commit_files(&self, files: &[(&str, &str)], epoch: i64) -> String {
        for (path, body) in files {
            let full = self.dir.join(path);
            if let Some(parent) = full.parent() {
                fs_err::create_dir_all(parent.as_std_path()).expect("mkdir origin subdir");
            }
            fs_err::write(full.as_std_path(), body).expect("write origin file");
        }
        git_in(&self.dir, epoch, &["add", "-A"]);
        git_in(&self.dir, epoch, &["commit", "--quiet", "-m", "fixture"]);
        git_in(&self.dir, epoch, &["rev-parse", "HEAD"])
    }
}
