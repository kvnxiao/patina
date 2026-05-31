#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

//! Integration coverage for `patina remove` (REQ-003, REQ-009, REQ-010).
//!
//! Each test spawns the real `patina` binary against an isolated tempdir
//! repo + state + home (via the shared [`common::Fixture`]). The fixture
//! first `patina apply`s a symlink module so a committed `<ts>.COMMIT`
//! record exists for `remove` to read; the binary's stdin is not a TTY, so
//! the `--yes`-driven non-interactive path is the one under test.

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;

/// Seed a `zsh` module whose `[[file]]` symlinks `~/.zshrc` to
/// `<repo>/zsh/zshrc` (content `shell-config`), then `patina apply --yes` so
/// a committed record exists. Returns the fixture with the applied symlink.
fn applied_symlink_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(
        fx.root.join("zsh").join("zshrc").as_std_path(),
        "shell-config",
    )
    .expect("seed repo source");

    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );
    let zshrc = fx.home.join(".zshrc");
    assert!(
        is_symlink(&zshrc),
        "~/.zshrc must be a symlink after apply; stderr: {}",
        stderr(&applied)
    );
    fx
}

/// CHK-005: `patina remove ~/.zshrc --yes` replaces the symlink with a
/// regular file holding the last-applied content, removes the `[[file]]`
/// entry, leaves the repository source unchanged, and a subsequent
/// `patina status --json` no longer lists the target.
#[test]
fn remove_replaces_target_drops_entry_and_status_omits_it() {
    let fx = applied_symlink_fixture();
    let zshrc = fx.home.join(".zshrc");
    let source = fx.root.join("zsh").join("zshrc");

    let out = fx.run(&["remove", "~/.zshrc", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "remove must exit 0; stderr: {}",
        stderr(&out)
    );

    // The target is now a regular file with the last-applied content.
    assert!(!is_symlink(&zshrc), "~/.zshrc must no longer be a symlink");
    assert!(zshrc.is_file(), "~/.zshrc must be a regular file");
    assert_eq!(
        fs_err::read_to_string(zshrc.as_std_path()).expect("read ~/.zshrc"),
        "shell-config"
    );

    // The `[[file]]` entry is gone from the module manifest.
    let manifest = fx.root.join("zsh").join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    assert!(
        !body.contains("[[file]]"),
        "the [[file]] entry must be removed, got: {body}"
    );

    // The repository source file is untouched.
    assert!(source.is_file(), "<repo>/zsh/zshrc must still exist");
    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read repo source"),
        "shell-config",
        "the repository source must be unchanged"
    );

    // A subsequent `patina status --json` no longer lists the target.
    let status = fx.run(&["status", "--json"], &[]);
    assert_eq!(
        code(&status),
        0,
        "status must exit 0; stderr: {}",
        stderr(&status)
    );
    let stdout = String::from_utf8(status.stdout).expect("utf8 status stdout");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("status is one JSON doc");
    let files = doc
        .get("files")
        .and_then(serde_json::Value::as_array)
        .expect("a files array");
    assert!(
        !files.iter().any(|f| {
            f.get("path")
                .and_then(serde_json::Value::as_str)
                .and_then(|p| Utf8Path::new(p).file_name())
                .is_some_and(|name| name == ".zshrc")
        }),
        "status must not list the removed target, got files: {files:?}"
    );
}

/// CHK-006: `patina remove ~/.zshrc --purge --yes` deletes the target from
/// disk entirely, removes the `[[file]]` entry, and leaves the repository
/// source unchanged.
#[test]
fn remove_purge_deletes_target_and_drops_entry() {
    let fx = applied_symlink_fixture();
    let zshrc = fx.home.join(".zshrc");
    let source = fx.root.join("zsh").join("zshrc");

    let out = fx.run(&["remove", "~/.zshrc", "--purge", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "remove --purge must exit 0; stderr: {}",
        stderr(&out)
    );

    // The target does not exist on disk (not as a symlink, not as a file).
    assert!(
        fs_err::symlink_metadata(zshrc.as_std_path()).is_err(),
        "~/.zshrc must not exist after --purge"
    );

    // The entry is gone; the repository source is unchanged.
    let manifest = fx.root.join("zsh").join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    assert!(
        !body.contains("[[file]]"),
        "the [[file]] entry must be removed, got: {body}"
    );
    assert!(source.is_file(), "<repo>/zsh/zshrc must still exist");
    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read repo source"),
        "shell-config",
        "the repository source must be unchanged"
    );
}

/// REQ-003: removing a path that is not currently managed exits 1, names the
/// path and the three discovery sources, and mutates nothing.
#[test]
fn remove_unmanaged_path_exits_1() {
    let fx = applied_symlink_fixture();
    let bashrc = fx.home.join(".bashrc");
    fs_err::write(bashrc.as_std_path(), "untouched").expect("seed ~/.bashrc");

    let out = fx.run(&["remove", "~/.bashrc", "--yes"], &[]);
    assert_eq!(
        code(&out),
        1,
        "removing an unmanaged path must exit 1; stderr: {}",
        stderr(&out)
    );
    let stderr = stderr(&out);
    assert!(
        stderr.contains("~/.bashrc") && stderr.contains("not managed"),
        "stderr must name the path and say it is not managed, got: {stderr}"
    );

    // The unmanaged file is untouched.
    assert_eq!(
        fs_err::read_to_string(bashrc.as_std_path()).expect("read ~/.bashrc"),
        "untouched",
        "the unmanaged file must be unchanged"
    );
    // The managed module manifest is untouched (still carries the entry).
    let manifest = fx.root.join("zsh").join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    assert!(
        body.contains("[[file]]"),
        "the managed entry must survive an unmanaged-path refusal, got: {body}"
    );
}

/// REQ-010: `remove --json --yes` emits a single deterministic JSON document
/// on stdout naming the removed target and the purge flag.
#[test]
fn remove_json_emits_document() {
    let fx = applied_symlink_fixture();

    let out = fx.run(&["remove", "~/.zshrc", "--json", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "remove --json must exit 0; stderr: {}",
        stderr(&out)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is one JSON doc");
    assert_eq!(
        doc.get("removed").and_then(serde_json::Value::as_str),
        Some("~/.zshrc")
    );
    assert_eq!(
        doc.get("purged").and_then(serde_json::Value::as_bool),
        Some(false)
    );
}

/// Decode stderr to a lossless string for assertions.
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Whether `path` is a symbolic link (without following it).
fn is_symlink(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path.as_std_path()).is_ok_and(|m| m.file_type().is_symlink())
}
