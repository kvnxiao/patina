//! Integration tests for watch lock contention.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::HostOs;
use patina_core::LockKind;
use patina_core::acquire_lock;
use std::process::Command;
use std::process::Output;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

static BUILD: Once = Once::new();

fn ensure_probe_built() {
    BUILD.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "--example", "reapply_probe"])
            .arg("--target-dir")
            .arg(target_root().as_str())
            .status()
            .expect("spawn cargo build for reapply_probe example");
        assert!(status.success(), "building reapply_probe example failed");
    });
}

fn target_root() -> Utf8PathBuf {
    let test_exe = std::env::current_exe().expect("current test exe path");
    let root = test_exe
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .expect("derive target root from test exe path");
    Utf8PathBuf::from_path_buf(root.to_owned()).expect("utf8 target root")
}

fn probe_path() -> Utf8PathBuf {
    let test_exe = std::env::current_exe().expect("current test exe path");
    let deps_dir = test_exe.parent().expect("deps dir");
    let profile_dir = deps_dir.parent().expect("profile dir");
    let mut probe = profile_dir.join("examples").join("reapply_probe");
    if cfg!(windows) {
        probe.set_extension("exe");
    }
    Utf8PathBuf::from_path_buf(probe).expect("utf8 probe path")
}

struct Fixture {
    _temp: TempDir,
    repo: Utf8PathBuf,
    home: Utf8PathBuf,
    state_base: Utf8PathBuf,
    state_dir: Utf8PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        let repo = root.join("repo");
        let home = root.join("home");
        let state_base = root.join("state");
        for dir in [&repo, &home, &state_base] {
            fs_err::create_dir_all(dir).expect("mkdir fixture dir");
        }
        fs_err::write(repo.join("patina.toml"), "[patina]\nroot = true\n").expect("root manifest");
        let module = repo.join("shell");
        fs_err::create_dir_all(&module).expect("mkdir module");
        fs_err::write(
            module.join("patina.toml"),
            "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
        )
        .expect("module manifest");
        fs_err::write(module.join("rc"), "reapplied\n").expect("source content");

        let state_base_owned = state_base.clone();
        let home_owned = home.clone();
        let state_dir =
            patina_core::state_dir::resolve_with_env(HostOs::current(), |name| match name {
                "XDG_STATE_HOME" | "LOCALAPPDATA" => Some(state_base_owned.as_str().to_owned()),
                "HOME" | "USERPROFILE" => Some(home_owned.as_str().to_owned()),
                _ => None,
            })
            .expect("resolve state dir");
        fs_err::create_dir_all(&state_dir).expect("create state dir");

        Self {
            _temp: temp,
            repo,
            home,
            state_base,
            state_dir,
        }
    }

    fn run_probe(&self, probe: &Utf8Path) -> Output {
        Command::new(probe.as_std_path())
            .env("PATINA_REPO", self.repo.as_str())
            .env("HOME", self.home.as_str())
            .env("USERPROFILE", self.home.as_str())
            .env("XDG_STATE_HOME", self.state_base.as_str())
            .env("LOCALAPPDATA", self.state_base.as_str())
            .env_remove("PATINA_PROFILE")
            .output()
            .expect("spawn reapply_probe")
    }
}

fn outcome(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .expect("probe stdout is utf8")
        .trim()
        .to_owned()
}

#[test]
fn reapply_skips_without_mutating_while_the_exclusive_lock_is_held() {
    ensure_probe_built();
    let probe = probe_path();
    let fx = Fixture::new();
    let target = fx.home.join(".rc");

    let guard = acquire_lock(
        &fx.state_dir.join("lock"),
        LockKind::Exclusive,
        Duration::from_secs(5),
    )
    .expect("hold exclusive lock");

    let held = fx.run_probe(&probe);
    assert_eq!(
        outcome(&held),
        "SKIPPED",
        "a held exclusive lock must make the watcher re-apply skip; stderr: {}",
        String::from_utf8_lossy(&held.stderr)
    );
    assert!(
        !target.as_std_path().exists(),
        "a skipped re-apply must not create the target"
    );

    drop(guard);
    let released = fx.run_probe(&probe);
    assert_eq!(
        outcome(&released),
        "APPLIED",
        "after the lock is released the re-apply must proceed; stderr: {}",
        String::from_utf8_lossy(&released.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(&target).expect("target written after release"),
        "reapplied\n",
        "the proceeding re-apply materializes the target"
    );
}
