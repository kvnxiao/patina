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
//! Each test builds a self-contained tempdir dotfiles repository, points
//! `PATINA_REPO` at it, and isolates the per-machine state directory under
//! the tempdir so the apply never touches the developer's real `$HOME`.
//! The binary is invoked as a subprocess (its stdin is therefore not a
//! TTY, exercising the non-interactive path).

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::HostOs;
use std::process::Command;
use std::process::Output;
use tempfile::TempDir;

/// A prepared fixture: an isolated repo + state dir, ready to invoke
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

    /// Invoke `patina` with an arbitrary `args` vector, isolating repo +
    /// state + home the same way every subcommand requires. The caller
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
            // Windows resolves the state dir from LOCALAPPDATA; isolate it
            // per-test so parallel tests never share one journal / lock /
            // backup tree (which would let one test's crash-recovery pass
            // reverse another test's just-applied files).
            .env("LOCALAPPDATA", self.state.as_str())
            .env_remove("PATINA_PROFILE");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn patina")
    }

    /// Invoke `patina apply` with `args`, isolating repo + state + home.
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
