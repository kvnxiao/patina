//! Integration tests for promote cli.

#![cfg(test)]

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::Origin;
use common::code;
use patina_core::ApplyRecord;
use patina_core::ExpectedTarget;
use patina_core::HostOs;
use patina_core::content_hash;
use patina_core::read_latest_commit;

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

#[test]
fn promote_copy_target_rewrites_source_and_rejournals() {
    let fx = applied_copy_fixture();
    let gitconfig = fx.home.join(".gitconfig");
    let source = fx.root.join("git").join("gitconfig");

    fs_err::write(gitconfig.as_std_path(), NEW_GITCONFIG).expect("overwrite target");

    let out = fx.run(&["promote", "~/.gitconfig", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "promote must exit 0; stderr: {}",
        stderr(&out)
    );

    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read repo source"),
        NEW_GITCONFIG,
        "the repository source must contain the promoted bytes"
    );

    let record = commit_record(&fx);
    let entry = entry_for(&record, "/.gitconfig");
    assert_eq!(
        content_hash_of(entry),
        content_hash(NEW_GITCONFIG.as_bytes()),
        "the re-journaled expected hash must be the blake3 hash of the new bytes"
    );

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
        "stderr must include the .tmpl source and the word template, got: {stderr}"
    );

    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read template source"),
        before,
        "the template source must be unchanged after a refused promote"
    );
}

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
        "stderr must include the target and explain symlink targets share their source, got: {stderr}"
    );

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
        "stderr must include the path and say it is not managed, got: {stderr}"
    );
    assert_eq!(
        fs_err::read_to_string(bashrc.as_std_path()).expect("read ~/.bashrc"),
        "untouched",
        "the unmanaged file must be unchanged"
    );
}

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
        "the JSON document must include the repository source, got: {doc}"
    );
}

#[test]
fn an_unreadable_target_names_its_path_once() {
    let fx = applied_copy_fixture();
    let gitconfig = fx.home.join(".gitconfig");
    let recorded = commit_record(&fx)
        .targets
        .first()
        .expect("the copy entry recorded its target")
        .target()
        .to_owned();
    fs_err::remove_file(gitconfig.as_std_path()).expect("remove the target");
    fs_err::create_dir(gitconfig.as_std_path()).expect("replace the target with a directory");

    let out = fx.run(&["promote", "~/.gitconfig", "--yes"], &[]);

    let stderr = stderr(&out);
    assert_eq!(
        code(&out),
        1,
        "a directory cannot be promoted; stderr: {stderr}"
    );
    assert_eq!(stderr.matches(recorded.as_str()).count(), 1, "{stderr}");
}

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

fn entry_for<'r>(record: &'r ApplyRecord, suffix: &str) -> &'r ExpectedTarget {
    record
        .targets
        .iter()
        .find(|t| t.target().replace('\\', "/").ends_with(suffix))
        .unwrap_or_else(|| panic!("no recorded target ending in `{suffix}`"))
}

fn content_hash_of(entry: &ExpectedTarget) -> [u8; 32] {
    match entry {
        ExpectedTarget::Content { hash, .. } => *hash,
        _ => panic!("expected a Content target, got {entry:?}"),
    }
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn is_symlink(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path.as_std_path()).is_ok_and(|m| m.file_type().is_symlink())
}

#[test]
fn promote_resolves_a_relative_path_against_the_working_directory() {
    let fx = applied_copy_fixture();
    let gitconfig = fx.home.join(".gitconfig");
    let source = fx.root.join("git").join("gitconfig");

    fs_err::write(gitconfig.as_std_path(), NEW_GITCONFIG).expect("overwrite target");

    let out = fx.run_in(&fx.home, &["promote", ".gitconfig", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "promote must exit 0; stderr: {}",
        stderr(&out)
    );

    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read repo source"),
        NEW_GITCONFIG,
        "the repository source must hold the promoted bytes"
    );
}

fn remote_backed_fixture() -> (Fixture, Utf8PathBuf, Utf8PathBuf) {
    let fx = Fixture::new();
    let origin = Origin::new(&fx, "humanizer", 1_700_000_000);
    let rev = origin.commit_files(&[("skills/tone.md", "upstream\n")], 1_700_000_000);
    fx.declare_remote("humanizer", &origin.url(), Some("main"));
    fx.module(
        "agents",
        "[[directory]]\nsource = \"skills\"\nremote = \"humanizer\"\n\
         target = \"~/.claude/skills\"\nmode = \"copy\"\n",
    );
    fs_err::write(
        fx.root.join("patina.lock").as_std_path(),
        format!(
            "version = 1\n\n[remotes.humanizer]\nurl = \"{}\"\nref = \"main\"\n\
             rev = \"{rev}\"\nupdated_at = \"2026-08-11T14:00:00Z\"\n",
            origin.url()
        ),
    )
    .expect("write patina.lock");

    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "the remote-backed apply must exit 0; stderr: {}",
        stderr(&applied)
    );
    let target = fx.home.join(".claude").join("skills").join("tone.md");
    assert_eq!(
        fs_err::read_to_string(target.as_std_path()).expect("read deployed leaf"),
        "upstream\n"
    );
    let checkout_source =
        patina_core::remote::cache::checkout_dir(&fx.state_root(), &remote_name("humanizer"), &rev)
            .join("skills")
            .join("tone.md");
    (fx, target, checkout_source)
}

fn remote_name(spelling: &str) -> patina_core::RemoteName {
    patina_core::RemoteName::parse(spelling).expect("a legal remote name")
}

#[test]
fn promote_refuses_a_remote_backed_target_and_leaves_the_checkout_intact() {
    let (fx, target, checkout_source) = remote_backed_fixture();
    fs_err::write(target.as_std_path(), "edited locally\n").expect("edit the target");

    let out = fx.run(&["promote", "~/.claude/skills/tone.md", "--yes"], &[]);

    assert_eq!(
        code(&out),
        1,
        "promoting into an immutable checkout must be refused; stderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("humanizer"),
        "the refusal must name the remote; stderr: {}",
        stderr(&out)
    );
    assert_eq!(
        fs_err::read_to_string(checkout_source.as_std_path()).expect("read checkout source"),
        "upstream\n",
        "the pinned checkout must still hold the upstream bytes"
    );
    assert_eq!(
        fs_err::read_to_string(target.as_std_path()).expect("read target"),
        "edited locally\n",
        "a refused promote must leave the target as it found it"
    );
}
