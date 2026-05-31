#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]
#![expect(
    clippy::panic,
    reason = "integration tests panic! on unexpected fixture/record shapes; allow-*-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration coverage for `patina promote` (REQ-004, REQ-009, REQ-010).
//!
//! Each test spawns the real `patina` binary against an isolated tempdir
//! repo + state + home (via the shared [`common::Fixture`]). A fixture first
//! `patina apply`s a module so a committed `<ts>.COMMIT` record exists for
//! `promote` to read; the binary's stdin is not a TTY, so the `--yes`-driven
//! non-interactive path is the one under test.

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;
use patina_core::ApplyRecord;
use patina_core::ExpectedTarget;
use patina_core::HostOs;
use patina_core::content_hash;
use patina_core::read_latest_commit;

/// Seed a `git` module whose `[[file]]` copies `~/.gitconfig` from
/// `<repo>/git/gitconfig` (content `OLD_GITCONFIG`), then `patina apply --yes`
/// so a committed copy-mode record exists. Returns the applied fixture.
const OLD_GITCONFIG: &str = "[user]\nemail = old@example.com";
const NEW_GITCONFIG: &str = "[user]\nemail = new@example.com";

fn applied_copy_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(
        fx.root.join("git").join("gitconfig").as_std_path(),
        OLD_GITCONFIG,
    )
    .expect("seed repo source");

    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );
    let gitconfig = fx.home.join(".gitconfig");
    assert_eq!(
        fs_err::read_to_string(gitconfig.as_std_path()).expect("read applied target"),
        OLD_GITCONFIG,
        "the copy-mode target must hold the source bytes after apply"
    );
    fx
}

/// CHK-007: promoting an externally-edited copy-mode target copies the new
/// bytes back into the repository source, the latest journal record's expected
/// hash for the target equals the blake3 hash of the new bytes, and a
/// subsequent `patina status` reports the target CLEAN.
#[test]
fn promote_copy_target_rewrites_source_and_rejournals() {
    let fx = applied_copy_fixture();
    let gitconfig = fx.home.join(".gitconfig");
    let source = fx.root.join("git").join("gitconfig");

    // The user edits the target outside Patina.
    fs_err::write(gitconfig.as_std_path(), NEW_GITCONFIG).expect("overwrite target");

    let out = fx.run(&["promote", "~/.gitconfig", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "promote must exit 0; stderr: {}",
        stderr(&out)
    );

    // The repository source now holds the new bytes.
    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read repo source"),
        NEW_GITCONFIG,
        "the repository source must hold the promoted bytes"
    );

    // The most recent journal record's expected hash for ~/.gitconfig equals
    // the blake3 hash of the new content.
    let record = commit_record(&fx);
    let entry = entry_for(&record, "/.gitconfig");
    assert_eq!(
        content_hash_of(entry),
        content_hash(NEW_GITCONFIG.as_bytes()),
        "the re-journaled expected hash must be the blake3 hash of the new bytes"
    );

    // A subsequent `patina status` classifies the target CLEAN.
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
    let gitconfig_state = files
        .iter()
        .find(|f| {
            f.get("path")
                .and_then(serde_json::Value::as_str)
                .and_then(|p| Utf8Path::new(p).file_name())
                .is_some_and(|name| name == ".gitconfig")
        })
        .and_then(|f| f.get("state"))
        .and_then(serde_json::Value::as_str)
        .expect("status must list ~/.gitconfig with a state");
    assert_eq!(
        gitconfig_state.to_ascii_uppercase(),
        "CLEAN",
        "the promoted target must be CLEAN, got: {gitconfig_state}"
    );
}

/// CHK-008: promoting a template-rendered target mutates nothing, names the
/// `.tmpl` source and the word `template` on stderr, and exits 1.
#[test]
fn promote_template_target_refuses() {
    let fx = Fixture::new();
    fx.module(
        "git",
        "[[file]]\nsource = \"gitconfig.tmpl\"\ntarget = \"~/.gitconfig\"\n",
    );
    let source = fx.root.join("git").join("gitconfig.tmpl");
    fs_err::write(source.as_std_path(), OLD_GITCONFIG).expect("seed template source");

    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );

    let before = fs_err::read_to_string(source.as_std_path()).expect("read template source");

    let out = fx.run(&["promote", "~/.gitconfig", "--yes"], &[]);
    assert_eq!(
        code(&out),
        1,
        "promoting a template target must exit 1; stderr: {}",
        stderr(&out)
    );
    let stderr = stderr(&out);
    assert!(
        stderr.contains("gitconfig.tmpl") && stderr.contains("template"),
        "stderr must name the .tmpl source and the word template, got: {stderr}"
    );

    // Nothing was mutated: the template source is unchanged.
    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read template source"),
        before,
        "the template source must be unchanged after a refused promote"
    );
}

/// REQ-004: promoting a symbolic-link target mutates nothing, names the target
/// and explains symlink targets share content with their source, and exits 1.
#[test]
fn promote_symlink_target_refuses() {
    let fx = Fixture::new();
    fx.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    let source = fx.root.join("zsh").join("zshrc");
    fs_err::write(source.as_std_path(), "shell-config").expect("seed symlink source");

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

    let out = fx.run(&["promote", "~/.zshrc", "--yes"], &[]);
    assert_eq!(
        code(&out),
        1,
        "promoting a symlink target must exit 1; stderr: {}",
        stderr(&out)
    );
    let stderr = stderr(&out);
    assert!(
        stderr.contains("~/.zshrc") && stderr.contains("symbolic-link"),
        "stderr must name the target and explain symlink targets share their source, got: {stderr}"
    );

    // Nothing was mutated: the target is still a symlink, the source unchanged.
    assert!(
        is_symlink(&zshrc),
        "~/.zshrc must still be a symlink after a refused promote"
    );
    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read repo source"),
        "shell-config",
        "the repository source must be unchanged after a refused promote"
    );
}

/// REQ-004: promoting a path that is not managed exits 1 and mutates nothing.
#[test]
fn promote_unmanaged_path_exits_1() {
    let fx = applied_copy_fixture();
    let bashrc = fx.home.join(".bashrc");
    fs_err::write(bashrc.as_std_path(), "untouched").expect("seed ~/.bashrc");

    let out = fx.run(&["promote", "~/.bashrc", "--yes"], &[]);
    assert_eq!(
        code(&out),
        1,
        "promoting an unmanaged path must exit 1; stderr: {}",
        stderr(&out)
    );
    let stderr = stderr(&out);
    assert!(
        stderr.contains("~/.bashrc") && stderr.contains("not managed"),
        "stderr must name the path and say it is not managed, got: {stderr}"
    );
    assert_eq!(
        fs_err::read_to_string(bashrc.as_std_path()).expect("read ~/.bashrc"),
        "untouched",
        "the unmanaged file must be unchanged"
    );
}

/// REQ-010: `promote --json --yes` emits a single deterministic JSON document
/// on stdout naming the promoted target and its repository source.
#[test]
fn promote_json_emits_document() {
    let fx = applied_copy_fixture();
    let gitconfig = fx.home.join(".gitconfig");
    fs_err::write(gitconfig.as_std_path(), NEW_GITCONFIG).expect("overwrite target");

    let out = fx.run(&["promote", "~/.gitconfig", "--json", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "promote --json must exit 0; stderr: {}",
        stderr(&out)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is one JSON doc");
    assert_eq!(
        doc.get("promoted").and_then(serde_json::Value::as_str),
        Some("~/.gitconfig")
    );
    assert!(
        doc.get("source")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| s.replace('\\', "/").ends_with("/git/gitconfig")),
        "the JSON document must name the repository source, got: {doc}"
    );
}

/// Decode the single COMMIT record produced by the last apply, resolving the
/// journal directory from this fixture's isolated env (matching the path the
/// subprocess actually wrote).
fn commit_record(fx: &Fixture) -> ApplyRecord {
    let journal_dir =
        patina_core::state_dir::resolve_with_env(HostOs::current(), |name| match name {
            "XDG_STATE_HOME" | "LOCALAPPDATA" => Some(fx.state.as_str().to_owned()),
            "HOME" | "USERPROFILE" => Some(fx.home.as_str().to_owned()),
            _ => None,
        })
        .expect("resolve fixture state dir")
        .join("journal");
    read_latest_commit(&journal_dir)
        .expect("read COMMIT record")
        .expect("an apply must have written a COMMIT record")
}

/// The recorded entry whose target path ends with `suffix`.
fn entry_for<'r>(record: &'r ApplyRecord, suffix: &str) -> &'r ExpectedTarget {
    record
        .targets
        .iter()
        .find(|t| t.target().replace('\\', "/").ends_with(suffix))
        .unwrap_or_else(|| panic!("no recorded target ending in `{suffix}`"))
}

/// The recorded blake3 hash of a content target. `ExpectedTarget` is
/// `#[non_exhaustive]`, so the match needs a wildcard arm in this downstream
/// crate.
fn content_hash_of(entry: &ExpectedTarget) -> [u8; 32] {
    match entry {
        ExpectedTarget::Content { hash, .. } => *hash,
        _ => panic!("expected a Content target, got {entry:?}"),
    }
}

/// Decode stderr to a lossless string for assertions.
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Whether `path` is a symbolic link (without following it).
fn is_symlink(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path.as_std_path()).is_ok_and(|m| m.file_type().is_symlink())
}
