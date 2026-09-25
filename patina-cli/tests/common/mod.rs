#![expect(
    dead_code,
    reason = "each integration-test crate that includes this module via `mod common;` uses a subset of its helpers and leaves at least one unused"
)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::HostOs;
use std::process::Command;
use std::process::Output;
use tempfile::TempDir;

/// Provide an isolated repository, home, and state directory.
pub(crate) struct Fixture {
    _temp: TempDir,
    /// Repository root.
    pub root: Utf8PathBuf,
    /// Home directory.
    pub home: Utf8PathBuf,
    /// State directory.
    pub state: Utf8PathBuf,
}

impl Fixture {
    /// Create an isolated repository fixture.
    pub(crate) fn new() -> Self {
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

    /// Append a remote declaration to the root manifest.
    pub(crate) fn declare_remote(&self, name: &str, url: &str, git_ref: Option<&str>) {
        let manifest = self.root.join("patina.toml");
        let existing = fs_err::read_to_string(&manifest).expect("read root manifest");
        let tracked = git_ref.map_or_else(String::new, |value| format!("ref = \"{value}\"\n"));
        let body = format!("{existing}\n[[remote]]\nname = \"{name}\"\nurl = \"{url}\"\n{tracked}");
        fs_err::write(&manifest, body).expect("write root manifest");
    }

    /// Write a module manifest and return its path.
    pub(crate) fn module(&self, name: &str, manifest: &str) -> Utf8PathBuf {
        let dir = self.root.join(name);
        fs_err::create_dir_all(&dir).expect("mkdir module");
        fs_err::write(dir.join("patina.toml"), manifest).expect("write module manifest");
        dir
    }

    /// Resolve the fixture's state directory.
    pub(crate) fn state_root(&self) -> Utf8PathBuf {
        patina_core::state_dir::resolve_with_env(HostOs::current(), |name| match name {
            "XDG_STATE_HOME" | "LOCALAPPDATA" => Some(self.state.as_str().to_owned()),
            "HOME" | "USERPROFILE" => Some(self.home.as_str().to_owned()),
            _ => None,
        })
        .expect("resolve fixture state dir")
    }

    /// Run `patina` with extra environment variables.
    pub(crate) fn run(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let bin = env!("CARGO_BIN_EXE_patina");
        let mut cmd = Command::new(bin);
        cmd.args(args)
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

    /// Run `patina` from a working directory with extra environment variables.
    pub(crate) fn run_in(&self, cwd: &Utf8Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
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

    /// Run `patina apply` with extra environment variables.
    pub(crate) fn apply_with_env(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut full = Vec::with_capacity(args.len() + 1);
        full.push("apply");
        full.extend_from_slice(args);
        self.run(&full, extra)
    }

    /// Run `patina apply`.
    pub(crate) fn apply(&self, args: &[&str]) -> Output {
        self.apply_with_env(args, &[])
    }

    pub(crate) fn run_ok(&self, args: &[&str]) -> Output {
        let out = self.run(args, &[]);
        assert_eq!(code(&out), 0, "{args:?} stderr: {}", stderr(&out));
        out
    }
}

/// Return the process exit code.
pub(crate) fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

pub(crate) fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Snapshot {
    Dir(std::time::SystemTime),
    File(Vec<u8>, std::time::SystemTime),
    Symlink(std::path::PathBuf),
}

/// Map every entry under `roots` to its kind, content, and modification time.
pub(crate) fn snapshot(roots: &[&Utf8Path]) -> std::collections::BTreeMap<Utf8PathBuf, Snapshot> {
    let mut entries = std::collections::BTreeMap::new();
    let mut pending: Vec<Utf8PathBuf> = roots.iter().map(|root| root.to_path_buf()).collect();
    while let Some(path) = pending.pop() {
        let meta = fs_err::symlink_metadata(&path).expect("stat snapshot entry");
        let entry = if meta.file_type().is_symlink() {
            Snapshot::Symlink(fs_err::read_link(&path).expect("read snapshot link"))
        } else if meta.is_dir() {
            for child in fs_err::read_dir(&path).expect("read snapshot dir") {
                let child = child.expect("read snapshot dir entry");
                pending.push(Utf8PathBuf::from_path_buf(child.path()).expect("utf8 snapshot path"));
            }
            Snapshot::Dir(meta.modified().expect("read snapshot mtime"))
        } else {
            Snapshot::File(
                fs_err::read(&path).expect("read snapshot file"),
                meta.modified().expect("read snapshot mtime"),
            )
        };
        entries.insert(path, entry);
    }
    entries
}

/// Journal and backup files are keyed by a one-second timestamp, so two
/// applies inside one second share their `<ts>` files.
pub(crate) fn wait_for_next_second() {
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    };
    let start = now();
    while now() == start {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
/// Create a file symlink using the host platform.
pub(crate) fn symlink_file(source: &Utf8Path, link: &Utf8Path) {
    std::os::unix::fs::symlink(source.as_std_path(), link.as_std_path()).expect("create symlink");
}

#[cfg(windows)]
/// Create a file symlink using the host platform.
pub(crate) fn symlink_file(source: &Utf8Path, link: &Utf8Path) {
    std::os::windows::fs::symlink_file(source.as_std_path(), link.as_std_path())
        .expect("create symlink");
}

#[cfg(unix)]
pub(crate) fn symlink_dir(source: &Utf8Path, link: &Utf8Path) {
    std::os::unix::fs::symlink(source.as_std_path(), link.as_std_path())
        .expect("create dir symlink");
}

#[cfg(windows)]
pub(crate) fn symlink_dir(source: &Utf8Path, link: &Utf8Path) {
    std::os::windows::fs::symlink_dir(source.as_std_path(), link.as_std_path())
        .expect("create dir symlink");
}

/// Run `git` with deterministic identity and timestamps.
pub(crate) fn git_in(cwd: &Utf8Path, epoch: i64, args: &[&str]) -> String {
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

/// Provide a throwaway git origin.
pub(crate) struct Origin {
    /// Origin repository path.
    pub dir: Utf8PathBuf,
}

impl Origin {
    /// Create an origin in the fixture home directory.
    pub(crate) fn new(f: &Fixture, name: &str, epoch: i64) -> Self {
        let dir = f.home.join(".origins").join(name);
        fs_err::create_dir_all(dir.as_std_path()).expect("mkdir origin");
        git_in(&dir, epoch, &["init", "--quiet", "-b", "main"]);
        Self { dir }
    }

    /// Return the origin path in URL form.
    pub(crate) fn url(&self) -> String {
        self.dir.as_str().replace('\\', "/")
    }

    /// Write files to the origin and commit them.
    pub(crate) fn commit_files(&self, files: &[(&str, &str)], epoch: i64) -> String {
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
