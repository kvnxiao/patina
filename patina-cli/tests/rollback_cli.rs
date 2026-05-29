#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixtures and asserted output; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for the `patina rollback` CLI surface (REQ-019,
//! CHK-033 / CHK-049).
//!
//! Each test builds a self-contained tempdir dotfiles repository, applies
//! it (`patina apply --yes`), then runs `patina rollback --yes` and asserts
//! the filesystem returns to its pre-apply state and a `<ts>.ROLLED_BACK`
//! sentinel appears in the journal. The per-machine state directory is
//! isolated under the tempdir so neither apply nor rollback touches the
//! developer's real `$HOME`.

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

    fn module(&self, name: &str, manifest: &str) -> Utf8PathBuf {
        let dir = self.root.join(name);
        fs_err::create_dir_all(&dir).expect("mkdir module");
        fs_err::write(dir.join("patina.toml"), manifest).expect("write module manifest");
        dir
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

    fn rollback(&self, args: &[&str]) -> Output {
        self.invoke("rollback", args)
    }

    /// The journal directory under the isolated state dir. The state-dir
    /// resolver nests `patina` under the platform state root; search for the
    /// `journal` directory rather than hard-coding the nesting.
    fn journal_dir(&self) -> Utf8PathBuf {
        find_dir_named(&self.state, "journal").expect("journal dir exists after apply")
    }
}

/// Recursively find the first directory named `name` under `root`.
fn find_dir_named(root: &Utf8Path, name: &str) -> Option<Utf8PathBuf> {
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let entries = fs_err::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = Utf8PathBuf::from_path_buf(entry.path()).ok()?;
            if entry.file_type().ok()?.is_dir() {
                if path.file_name() == Some(name) {
                    return Some(path);
                }
                stack.push(path);
            }
        }
    }
    None
}

/// Whether the journal contains a `<ts>.ROLLED_BACK` sentinel.
fn has_rolled_back_sentinel(journal: &Utf8Path) -> bool {
    fs_err::read_dir(journal)
        .expect("read journal dir")
        .flatten()
        .any(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".ROLLED_BACK"))
        })
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

fn assert_applied(out: &Output) {
    assert_eq!(
        code(out),
        0,
        "apply must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn rollback_deletes_a_symlink_target_and_writes_the_sentinel() {
    // CHK-033 (fresh-creation leg): a ~/.zshrc materialized as a symlink with
    // no pre-existing file, rolled back, is removed (the link had no backup)
    // and a ROLLED_BACK sentinel is written. The pre-existing-regular-file →
    // symlink → restore-to-regular-file leg is exercised end-to-end by
    // `rollback_restores_a_regular_file_replaced_by_a_symlink` below.
    let f = Fixture::new();
    let module = f.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("zshrc"), "export Z=1\n").expect("write source");

    let target = f.home.join(".zshrc");
    assert_applied(&f.apply(&["--yes"]));
    // After apply the target is a symlink.
    assert!(
        fs_err::symlink_metadata(&target)
            .expect("stat after apply")
            .file_type()
            .is_symlink(),
        "apply must materialize a symlink"
    );

    let rolled = f.rollback(&["--yes"]);
    assert_eq!(
        code(&rolled),
        0,
        "rollback must succeed; stderr: {}",
        String::from_utf8_lossy(&rolled.stderr)
    );

    // The fresh link is removed and the sentinel is written.
    assert!(
        fs_err::symlink_metadata(&target).is_err(),
        "rollback must delete the freshly-created symlink"
    );
    assert!(
        has_rolled_back_sentinel(&f.journal_dir()),
        "a ROLLED_BACK sentinel must be written"
    );
}

#[test]
fn rollback_restores_a_regular_file_replaced_by_a_symlink() {
    // CHK-033 (literal scenario, REQ-019): a pre-existing ~/.zshrc with
    // content "original" is replaced by apply with a symlink (the symlink
    // executor backs up then clears the pre-existing file before linking);
    // `rollback --yes` restores it to a regular file with content "original"
    // and writes a ROLLED_BACK sentinel.
    let f = Fixture::new();
    let module = f.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(module.join("zshrc"), "export Z=1\n").expect("write source");

    // Given: ~/.zshrc pre-exists as a regular file with content "original".
    let target = f.home.join(".zshrc");
    fs_err::write(&target, "original").expect("write pre-existing zshrc");

    // When: apply replaces it with a symlink.
    assert_applied(&f.apply(&["--yes"]));
    assert!(
        fs_err::symlink_metadata(&target)
            .expect("stat after apply")
            .file_type()
            .is_symlink(),
        "apply must replace the pre-existing file with a symlink"
    );

    // When: rollback --yes.
    let rolled = f.rollback(&["--yes"]);
    assert_eq!(
        code(&rolled),
        0,
        "rollback must succeed; stderr: {}",
        String::from_utf8_lossy(&rolled.stderr)
    );

    // Then: ~/.zshrc is a regular file again with content "original".
    let meta = fs_err::symlink_metadata(&target).expect("stat after rollback");
    assert!(
        meta.file_type().is_file(),
        "rollback must restore a regular file, not a link"
    );
    assert_eq!(
        fs_err::read_to_string(&target).expect("read restored zshrc"),
        "original",
        "the pre-existing file must be restored to its original content"
    );
    assert!(
        has_rolled_back_sentinel(&f.journal_dir()),
        "a ROLLED_BACK sentinel must be written"
    );
}

#[test]
fn rollback_deletes_a_freshly_created_target() {
    // A target that did not exist before apply is deleted by rollback.
    let f = Fixture::new();
    let module = f.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("gitconfig"), "[user]\n").expect("write source");

    assert_applied(&f.apply(&["--yes"]));
    let target = f.home.join(".gitconfig");
    assert!(target.exists(), "apply must create the target");

    assert_eq!(code(&f.rollback(&["--yes"])), 0);
    assert!(
        !target.exists(),
        "a freshly-created target must be deleted by rollback"
    );
}

#[test]
fn rollback_with_no_prior_apply_exits_one_and_names_no_prior_apply() {
    // CHK / REQ-019: no apply has committed -> exit 1, stderr names
    // "no prior apply found".
    let f = Fixture::new();
    f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    // No apply run; roll back against a fresh state directory.
    let out = f.rollback(&["--yes"]);
    assert_eq!(code(&out), 1, "no prior apply must exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no prior apply found"),
        "stderr must name the failure, got: {stderr}"
    );
}

#[test]
fn multi_target_copy_entry_rolls_back_to_pre_apply_state() {
    // CHK-049: a [[file]] copy entry with two targets — one pre-existing
    // (~/.claude/agent.toml = "old"), one fresh (~/.codex/agent.toml) — is
    // rolled back so the pre-existing target is restored to "old" and the
    // fresh one is deleted, with a ROLLED_BACK sentinel written.
    let f = Fixture::new();
    let module = f.module(
        "agent",
        "[[file]]\nsource = \"agent.toml\"\n\
         targets = [\"~/.claude/agent.toml\", \"~/.codex/agent.toml\"]\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("agent.toml"), "new").expect("write source");

    // Pre-create only the claude target with "old"; codex does not exist.
    let claude = f.home.join(".claude").join("agent.toml");
    let codex = f.home.join(".codex").join("agent.toml");
    fs_err::create_dir_all(claude.parent().expect("claude parent")).expect("mkdir claude");
    fs_err::write(&claude, "old").expect("write pre-existing claude");

    assert_applied(&f.apply(&["--yes"]));
    // After apply both carry "new".
    assert_eq!(fs_err::read_to_string(&claude).expect("read claude"), "new");
    assert_eq!(fs_err::read_to_string(&codex).expect("read codex"), "new");

    let rolled = f.rollback(&["--yes"]);
    assert_eq!(
        code(&rolled),
        0,
        "rollback must succeed; stderr: {}",
        String::from_utf8_lossy(&rolled.stderr)
    );

    assert_eq!(
        fs_err::read_to_string(&claude).expect("read claude after rollback"),
        "old",
        "the pre-existing target must be restored from its backup"
    );
    assert!(
        !codex.exists(),
        "the freshly-created target must be deleted"
    );
    assert!(
        has_rolled_back_sentinel(&f.journal_dir()),
        "a ROLLED_BACK sentinel must be written"
    );
}

#[test]
fn rolled_back_apply_drops_out_of_status_last_apply() {
    // After rollback the apply is excluded from status's last-apply
    // computation: status reports no last_apply.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "x\n").expect("write source");

    assert_applied(&f.apply(&["--yes"]));
    assert_eq!(code(&f.rollback(&["--yes"])), 0);

    let status = f.invoke("status", &["--json"]);
    assert_eq!(code(&status), 0);
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&status.stdout)).expect("status JSON");
    assert!(
        doc.get("last_apply").expect("last_apply key").is_null(),
        "a rolled-back apply must not be the last apply; {doc}"
    );
}
