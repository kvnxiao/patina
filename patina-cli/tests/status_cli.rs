#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use .expect()/panic! on fixtures and asserted JSON; allow-*-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for the `patina status` CLI surface.
//!
//! Each test builds a self-contained tempdir dotfiles repository, applies
//! it (`patina apply --yes`), optionally perturbs a materialized target,
//! then runs `patina status --json` and asserts the classification. The
//! per-machine state directory is isolated under the tempdir so neither
//! the apply nor the status touches the developer's real `$HOME`.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::process::Command;
use std::process::Output;
use tempfile::TempDir;

/// A prepared fixture: an isolated repo + state dir + home.
struct Fixture {
    _temp: TempDir,
    root: Utf8PathBuf,
    home: Utf8PathBuf,
    state: Utf8PathBuf,
}

impl Fixture {
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

    /// Write a module directory with the given `patina.toml` body.
    fn module(&self, name: &str, manifest: &str) -> Utf8PathBuf {
        let dir = self.root.join(name);
        fs_err::create_dir_all(&dir).expect("mkdir module");
        fs_err::write(dir.join("patina.toml"), manifest).expect("write module manifest");
        dir
    }

    /// Remove a module directory so the current plan no longer manages its
    /// targets (used to exercise ORPHANED classification).
    fn remove_module(&self, name: &str) {
        fs_err::remove_dir_all(self.root.join(name)).expect("remove module");
    }

    fn invoke(&self, subcommand: &str, args: &[&str]) -> Output {
        let bin = env!("CARGO_BIN_EXE_patina");
        Command::new(bin)
            .arg(subcommand)
            .args(args)
            .env("PATINA_REPO", self.root.as_str())
            .env("HOME", self.home.as_str())
            .env("USERPROFILE", self.home.as_str())
            .env("XDG_STATE_HOME", self.state.as_str())
            .env("LOCALAPPDATA", self.state.as_str())
            .env_remove("PATINA_PROFILE")
            .output()
            .expect("spawn patina")
    }

    fn apply(&self, args: &[&str]) -> Output {
        self.invoke("apply", args)
    }

    fn status(&self, args: &[&str]) -> Output {
        self.invoke("status", args)
    }
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

fn status_json(out: &Output) -> serde_json::Value {
    assert_eq!(
        code(out),
        0,
        "status must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).expect("status stdout must be a single JSON document")
}

fn counter(doc: &serde_json::Value, key: &str) -> u64 {
    doc.get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("missing counter `{key}` in {doc}"))
}

/// Find the `files[]` entry whose path ends with `suffix` and return its
/// `state`.
fn state_for(doc: &serde_json::Value, suffix: &str) -> String {
    let files = doc
        .get("files")
        .and_then(serde_json::Value::as_array)
        .expect("files array");
    for entry in files {
        let path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if path.replace('\\', "/").ends_with(suffix) {
            return entry
                .get("state")
                .and_then(serde_json::Value::as_str)
                .expect("state string")
                .to_owned();
        }
    }
    panic!("no files entry ending in `{suffix}` in {doc}");
}

#[test]
fn three_clean_operations_report_clean_counter_three() {
    // Three file operations applied, no subsequent change ->
    // clean = 3, drifted/missing/orphaned = 0.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n\n\
         [[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n\n\
         [[file]]\nsource = \"c\"\ntarget = \"~/.c\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("a"), "aaa\n").expect("write a");
    fs_err::write(module.join("b"), "bbb\n").expect("write b");
    fs_err::write(module.join("c"), "ccc\n").expect("write c");

    let applied = f.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must succeed; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    let doc = status_json(&f.status(&["--json"]));
    assert_eq!(counter(&doc, "clean"), 3, "{doc}");
    assert_eq!(counter(&doc, "drifted"), 0, "{doc}");
    assert_eq!(counter(&doc, "missing"), 0, "{doc}");
    assert_eq!(counter(&doc, "orphaned"), 0, "{doc}");
    // last_apply.at must be an RFC 3339 timestamp.
    let at = doc
        .pointer("/last_apply/at")
        .and_then(serde_json::Value::as_str)
        .expect("last_apply.at present");
    assert!(
        at.contains('-') && at.ends_with('Z'),
        "last_apply.at must be RFC 3339, got {at}"
    );
}

#[test]
fn edited_copy_target_reports_drifted() {
    // A copy-mode target edited after apply -> drifted = 1 and the
    // files entry for that path has state = drifted.
    let f = Fixture::new();
    let module = f.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("gitconfig"), "[user]\n").expect("write source");

    assert_eq!(code(&f.apply(&["--yes"])), 0);

    // Append bytes to the materialized target to drift it.
    let target = f.home.join(".gitconfig");
    let mut content = fs_err::read_to_string(&target).expect("read target");
    content.push_str("email = x@y.z\n");
    fs_err::write(&target, content).expect("edit target");

    let doc = status_json(&f.status(&["--json"]));
    assert_eq!(counter(&doc, "drifted"), 1, "{doc}");
    assert_eq!(state_for(&doc, "/.gitconfig"), "drifted", "{doc}");
}

#[test]
fn multi_target_entry_reports_one_entry_per_target() {
    // A [[file]] with two copy targets; one edited externally ->
    // two files entries, one clean and one drifted, clean >= 1, drifted >= 1.
    let f = Fixture::new();
    let module = f.module(
        "agent",
        "[[file]]\nsource = \"agent.toml\"\n\
         targets = [\"~/.claude/agent.toml\", \"~/.codex/agent.toml\"]\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("agent.toml"), "name = patina\n").expect("write source");

    assert_eq!(code(&f.apply(&["--yes"])), 0);

    // Overwrite only the codex target so it drifts; the claude one stays
    // clean.
    fs_err::write(f.home.join(".codex").join("agent.toml"), "name = changed\n")
        .expect("overwrite codex target");

    let doc = status_json(&f.status(&["--json"]));
    let files = doc
        .get("files")
        .and_then(serde_json::Value::as_array)
        .expect("files array");
    assert_eq!(files.len(), 2, "two targets -> two entries; {doc}");
    assert_eq!(state_for(&doc, "/.claude/agent.toml"), "clean", "{doc}");
    assert_eq!(state_for(&doc, "/.codex/agent.toml"), "drifted", "{doc}");
    assert!(counter(&doc, "clean") >= 1, "{doc}");
    assert!(counter(&doc, "drifted") >= 1, "{doc}");
}

#[test]
fn deleted_target_reports_missing() {
    // A materialized target deleted on disk -> missing = 1.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "x\n").expect("write source");

    assert_eq!(code(&f.apply(&["--yes"])), 0);
    fs_err::remove_file(f.home.join(".rc")).expect("delete target");

    let doc = status_json(&f.status(&["--json"]));
    assert_eq!(counter(&doc, "missing"), 1, "{doc}");
    assert_eq!(state_for(&doc, "/.rc"), "missing", "{doc}");
}

#[test]
fn removed_entry_with_surviving_target_reports_orphaned() {
    // A target applied, then the [[file]] entry removed from the repo while
    // the materialized file survives -> orphaned = 1.
    let f = Fixture::new();
    let module = f.module(
        "old",
        "[[file]]\nsource = \"oldconfig\"\ntarget = \"~/.oldconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("oldconfig"), "legacy\n").expect("write source");

    assert_eq!(code(&f.apply(&["--yes"])), 0);
    // Drop the module so the current plan no longer manages ~/.oldconfig,
    // but leave the materialized file in place.
    f.remove_module("old");
    assert!(
        f.home.join(".oldconfig").exists(),
        "the orphaned file must still be on disk"
    );

    let doc = status_json(&f.status(&["--json"]));
    assert_eq!(counter(&doc, "orphaned"), 1, "{doc}");
    assert_eq!(state_for(&doc, "/.oldconfig"), "orphaned", "{doc}");
}

#[test]
fn symlink_target_reports_clean_then_drifts_when_replaced() {
    // A symlink target is clean while it points at the source, and drifts
    // when replaced by a regular file.
    let f = Fixture::new();
    let module = f.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("zshrc"), "export Z=1\n").expect("write source");

    let applied = f.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must succeed; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    let doc = status_json(&f.status(&["--json"]));
    assert_eq!(state_for(&doc, "/.zshrc"), "clean", "{doc}");

    // Replace the link with a regular file -> drift.
    let target = f.home.join(".zshrc");
    fs_err::remove_file(&target).expect("remove link");
    fs_err::write(&target, "not a link\n").expect("write regular file");

    let doc = status_json(&f.status(&["--json"]));
    assert_eq!(state_for(&doc, "/.zshrc"), "drifted", "{doc}");
}
