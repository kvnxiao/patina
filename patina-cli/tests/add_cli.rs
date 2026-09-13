//! Integration tests for add cli.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::code;
use common::symlink_dir;
use std::io::BufRead;
use std::io::BufReader;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::Once;
use std::time::Duration;
use std::time::Instant;

#[test]
fn add_preserves_dotfile_name_in_source_and_leaves_target() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(
        &["add", "~/.zshrc", "--module", "zsh", "--symlink", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 0, "add must exit 0; stderr: {}", stderr(&out));

    let staged = fx.root.join("zsh").join(".zshrc");
    assert!(staged.is_file(), "<repo>/zsh/.zshrc must be a regular file");
    assert!(
        !is_symlink(&staged),
        "<repo>/zsh/.zshrc must not be a symlink"
    );
    assert_eq!(
        fs_err::read_to_string(staged.as_std_path()).expect("read staged file"),
        "foo"
    );

    let manifest = fx.root.join("zsh").join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    let parsed: toml::Value = toml::from_str(&body).expect("module manifest parses");
    let entries = parsed
        .get("file")
        .and_then(toml::Value::as_array)
        .expect("a [[file]] array");
    assert_eq!(entries.len(), 1, "exactly one [[file]] entry");
    let entry = entries.first().expect("the single [[file]] entry");
    assert_eq!(
        entry.get("source").and_then(toml::Value::as_str),
        Some(".zshrc")
    );
    assert_eq!(
        entry.get("target").and_then(toml::Value::as_str),
        Some("~/.zshrc")
    );
    assert_eq!(
        entry.get("mode").and_then(toml::Value::as_str),
        Some("symlink")
    );

    assert!(zshrc.is_file(), "~/.zshrc must remain a regular file");
    assert!(!is_symlink(&zshrc), "~/.zshrc must not be a symlink yet");
    assert_eq!(
        fs_err::read_to_string(zshrc.as_std_path()).expect("read ~/.zshrc"),
        "foo"
    );
}

#[test]
fn add_preserves_dotfile_name_for_template_source() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "{{ patina.os }}").expect("seed ~/.zshrc");

    let out = fx.run(
        &["add", "~/.zshrc", "--module", "zsh", "--template", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 0, "add must exit 0; stderr: {}", stderr(&out));

    let staged = fx.root.join("zsh").join(".zshrc.tmpl");
    assert!(
        staged.is_file(),
        "<repo>/zsh/.zshrc.tmpl must be a regular file"
    );
    assert_eq!(manifest_file_field(&fx, "zsh", "source"), ".zshrc.tmpl");
}

#[test]
fn add_refuses_to_overwrite_an_existing_repository_source() {
    let fx = Fixture::new();
    let original_manifest =
        "[[file]]\nsource = \".wslconfig\"\ntarget = \"~/other\"\nmode = \"copy\"\n";
    fx.module("wsl2", original_manifest);
    let repository_source = fx.root.join("wsl2").join(".wslconfig");
    fs_err::write(repository_source.as_std_path(), "existing").expect("seed repository source");
    let wslconfig = fx.home.join(".wslconfig");
    fs_err::write(wslconfig.as_std_path(), "replacement").expect("seed ~/.wslconfig");

    let out = fx.run(
        &["add", "~/.wslconfig", "--module", "wsl2", "--copy", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 1, "add must exit 1; stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("repository source") && stderr(&out).contains("already exists"),
        "stderr must identify the occupied repository source, got: {}",
        stderr(&out)
    );
    assert_eq!(
        fs_err::read_to_string(repository_source.as_std_path()).expect("read repository source"),
        "existing"
    );
    assert_eq!(
        fs_err::read_to_string(fx.root.join("wsl2").join("patina.toml").as_std_path())
            .expect("read module manifest"),
        original_manifest
    );
}

#[test]
fn add_then_apply_materializes_target_as_symlink() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let add = fx.run(
        &["add", "~/.zshrc", "--module", "zsh", "--symlink", "--yes"],
        &[],
    );
    assert_eq!(code(&add), 0, "add must exit 0; stderr: {}", stderr(&add));
    assert!(
        !is_symlink(&zshrc),
        "~/.zshrc must not be a symlink before apply"
    );

    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );

    assert!(
        is_symlink(&zshrc),
        "~/.zshrc must be a symbolic link after apply"
    );
    let link_target = fs_err::read_link(zshrc.as_std_path()).expect("read_link ~/.zshrc");
    let staged = fx.root.join("zsh").join(".zshrc");
    let canonical = fs_err::canonicalize(staged.as_std_path()).expect("canonicalize staged source");
    assert_eq!(
        fs_err::canonicalize(&link_target).expect("canonicalize link target"),
        canonical,
        "the symlink must resolve to the canonical <repo>/zsh/.zshrc"
    );
}

#[test]
fn add_two_mode_flags_is_a_usage_error() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(&["add", "~/.zshrc", "--symlink", "--copy"], &[]);
    assert_eq!(
        code(&out),
        2,
        "two mode flags must be a clap usage error (exit 2)"
    );
    let stderr = stderr(&out);
    assert!(
        stderr.contains("--symlink") && stderr.contains("--copy"),
        "stderr must include the conflicting flags, got: {stderr}"
    );
}

#[test]
fn add_non_tty_without_module_exits_1() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(&["add", "~/.zshrc", "--symlink", "--yes"], &[]);
    assert_eq!(code(&out), 1, "non-TTY add without --module must exit 1");
    assert!(
        stderr(&out).contains("--module"),
        "stderr must include the missing --module flag, got: {}",
        stderr(&out)
    );

    assert!(zshrc.is_file(), "~/.zshrc must be untouched on refusal");
    assert!(
        !fx.root.join("zsh").exists(),
        "no module directory should be created on refusal"
    );
}

#[test]
fn add_already_managed_path_exits_1() {
    let fx = Fixture::new();
    fx.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(fx.root.join("zsh").join("zshrc").as_std_path(), "old").expect("seed source");
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(
        &["add", "~/.zshrc", "--module", "other", "--copy", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 1, "adding an already-managed path must exit 1");
    let stderr = stderr(&out);
    assert!(
        stderr.contains("already managed") && stderr.contains("zsh"),
        "stderr must say the path is already managed and name the module, got: {stderr}"
    );
    assert!(zshrc.is_file(), "~/.zshrc must be untouched on refusal");
}

#[test]
fn add_json_emits_one_document_with_target_module_and_mode() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(
        &[
            "add",
            "~/.zshrc",
            "--module",
            "zsh",
            "--symlink",
            "--json",
            "--yes",
        ],
        &[],
    );
    assert_eq!(
        code(&out),
        0,
        "add --json must exit 0; stderr: {}",
        stderr(&out)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is one JSON doc");
    assert_eq!(
        doc.get("added").and_then(serde_json::Value::as_str),
        Some("~/.zshrc")
    );
    assert_eq!(
        doc.get("module").and_then(serde_json::Value::as_str),
        Some("zsh")
    );
    assert_eq!(
        doc.get("mode").and_then(serde_json::Value::as_str),
        Some("symlink")
    );
}

#[test]
fn add_serializes_behind_a_held_exclusive_lock() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let state_dir = fx.state_root();
    fs_err::create_dir_all(state_dir.join("journal").as_std_path()).expect("mkdir journal");

    let helper = lock_helper_path();
    ensure_lock_helper_built();

    let hold = Duration::from_secs(2);
    let mut holder = spawn_holder(&helper, &state_dir, hold);
    wait_for_acquired(&mut holder);
    let released_after = Instant::now();

    let started = Instant::now();
    let out = fx.run(
        &["add", "~/.zshrc", "--module", "zsh", "--symlink", "--yes"],
        &[("PATINA_LOCK_TIMEOUT_MS", "30000")],
    );
    let waited = started.elapsed();

    holder.wait().expect("holder exits");

    assert_eq!(
        code(&out),
        0,
        "add must complete once the lock frees; stderr: {}",
        stderr(&out)
    );
    assert!(
        waited >= Duration::from_secs(1),
        "add should have blocked on the held lock (waited {waited:?})"
    );
    assert!(
        released_after.elapsed() >= Duration::from_secs(1),
        "the holder should have been holding the lock while add waited"
    );

    let moved = fx.root.join("zsh").join(".zshrc");
    assert!(
        moved.is_file(),
        "the moved source must exist after the wait"
    );
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn is_symlink(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path.as_std_path()).is_ok_and(|m| m.file_type().is_symlink())
}

static BUILD: Once = Once::new();

fn ensure_lock_helper_built() {
    BUILD.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--quiet",
                "--package",
                "patina-core",
                "--example",
                "lock_helper",
            ])
            .arg("--target-dir")
            .arg(target_root().as_str())
            .status()
            .expect("spawn cargo build for lock_helper example");
        assert!(status.success(), "building lock_helper example failed");
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

fn lock_helper_path() -> Utf8PathBuf {
    let test_exe = std::env::current_exe().expect("current test exe path");
    let deps_dir = test_exe.parent().expect("deps dir");
    let profile_dir = deps_dir.parent().expect("profile dir");
    let mut helper = profile_dir.join("examples").join("lock_helper");
    if cfg!(windows) {
        helper.set_extension("exe");
    }
    Utf8PathBuf::from_path_buf(helper).expect("utf8 helper path")
}

fn spawn_holder(helper: &Utf8Path, state: &Utf8Path, hold: Duration) -> Child {
    Command::new(helper.as_std_path())
        .arg(state.as_str())
        .arg("exclusive")
        .arg(hold.as_millis().to_string())
        .arg("30000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lock_helper holder")
}

fn wait_for_acquired(child: &mut Child) {
    let stdout = child.stdout.take().expect("holder stdout piped");
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read holder stdout");
        assert!(read != 0, "holder exited before printing ACQUIRED marker");
        if line.starts_with("ACQUIRED ") {
            break;
        }
    }
    std::thread::spawn(move || {
        let _drained = std::io::copy(&mut reader, &mut std::io::sink());
    });
}

#[test]
fn add_relative_path_from_home_stores_a_home_relative_target_and_applies_there() {
    let fx = Fixture::new();
    let wslconfig = fx.home.join(".wslconfig");
    fs_err::write(wslconfig.as_std_path(), "foo").expect("seed ~/.wslconfig");

    let added = fx.run_in(
        &fx.home,
        &[
            "add",
            ".wslconfig",
            "--module",
            "wsl2",
            "--symlink",
            "--yes",
        ],
        &[],
    );
    assert_eq!(
        code(&added),
        0,
        "add must exit 0; stderr: {}",
        stderr(&added)
    );
    assert_eq!(manifest_target(&fx, "wsl2"), "~/.wslconfig");

    let applied = fx.run_in(&fx.root, &["apply", "--yes"], &[]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );
    assert!(
        is_symlink(&wslconfig),
        "~/.wslconfig must be a symlink after apply"
    );
    assert_eq!(
        fs_err::canonicalize(&wslconfig).expect("canonicalize link target"),
        fs_err::canonicalize(fx.root.join("wsl2").join(".wslconfig"))
            .expect("canonicalize repo source")
    );
}

#[test]
fn add_relative_path_outside_home_stores_an_absolute_target() {
    let fx = Fixture::new();
    let outside = fx.root.parent().expect("fixture parent").join("outside");
    fs_err::create_dir_all(outside.as_std_path()).expect("mkdir outside");
    let conf = outside.join("tool.conf");
    fs_err::write(conf.as_std_path(), "bar").expect("seed the outside-home file");

    let out = fx.run_in(
        &outside,
        &["add", "tool.conf", "--module", "tool", "--copy", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 0, "add must exit 0; stderr: {}", stderr(&out));

    let stored = manifest_target(&fx, "tool");
    assert!(
        !stored.starts_with('~'),
        "a target outside HOME must stay absolute, got: {stored}"
    );
    assert_eq!(
        Utf8Path::new(&stored)
            .canonicalize_utf8()
            .expect("canonicalize stored target"),
        conf.canonicalize_utf8()
            .expect("canonicalize the seeded file")
    );
}

#[test]
fn add_refuses_a_relative_spelling_of_an_already_managed_target() {
    let fx = Fixture::new();
    fx.module(
        "zsh",
        "[[file]]
source = \"zshrc\"
target = \"~/.zshrc\"
mode = \"symlink\"
",
    );
    fs_err::write(fx.root.join("zsh").join("zshrc").as_std_path(), "old").expect("seed source");
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run_in(
        &fx.home,
        &["add", ".zshrc", "--module", "other", "--copy", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 1, "a relative spelling must refuse too");
    let stderr = stderr(&out);
    assert!(
        stderr.contains("already managed") && stderr.contains("zsh"),
        "stderr must say the path is already managed and name the module, got: {stderr}"
    );
    assert!(
        !fx.root.join("other").exists(),
        "no module directory should be created on refusal"
    );
}

#[test]
fn add_contracts_home_when_the_environment_spells_it_indirectly() {
    let fx = Fixture::new();
    let wslconfig = fx.home.join(".wslconfig");
    fs_err::write(wslconfig.as_std_path(), "foo").expect("seed ~/.wslconfig");
    // This lexical indirection reproduces a home spelling that differs from
    // `getcwd` without creating a symlink.
    let indirect = format!("{}/../home", fx.home);

    let out = fx.run_in(
        &fx.home,
        &["add", ".wslconfig", "--module", "wsl2", "--copy", "--yes"],
        &[
            ("HOME", indirect.as_str()),
            ("USERPROFILE", indirect.as_str()),
        ],
    );
    assert_eq!(code(&out), 0, "add must exit 0; stderr: {}", stderr(&out));
    assert_eq!(manifest_target(&fx, "wsl2"), "~/.wslconfig");
}

#[test]
fn add_dot_from_a_directory_holding_the_repository_is_refused() {
    let fx = Fixture::new();
    let parent = fx.root.parent().expect("the repository's parent");

    let out = fx.run_in(
        parent,
        &["add", ".", "--module", "everything", "--copy", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 1, "adding the repository's parent must exit 1");
    let stderr = stderr(&out);
    assert!(
        stderr.contains("copy the repository into itself"),
        "stderr must name the self-copy hazard, got: {stderr}"
    );
    assert!(
        !fx.root.join("everything").exists(),
        "no module directory should be created on refusal"
    );
}

#[test]
fn add_dot_from_the_home_directory_is_refused() {
    let fx = Fixture::new();

    let out = fx.run_in(
        &fx.home,
        &["add", ".", "--module", "everything", "--copy", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 1, "adding the home directory must exit 1");
    let stderr = stderr(&out);
    assert!(
        stderr.contains("home directory"),
        "stderr must name the home directory, got: {stderr}"
    );
    assert!(
        !fx.root.join("everything").exists(),
        "no module directory should be created on refusal"
    );
}

#[test]
fn add_refuses_a_symlink_to_the_repository() {
    let fx = Fixture::new();
    let link = fx.home.join("repository-link");
    symlink_dir(&fx.root, &link);

    let out = fx.run(
        &[
            "add",
            link.as_str(),
            "--module",
            "everything",
            "--copy",
            "--yes",
        ],
        &[],
    );
    assert_eq!(code(&out), 1, "adding a repository symlink must exit 1");
    assert!(
        stderr(&out).contains("copy the repository into itself"),
        "stderr must name the self-copy hazard, got: {}",
        stderr(&out)
    );
    assert!(
        !fx.root.join("everything").exists(),
        "no module directory should be created on refusal"
    );
}

fn manifest_target(fx: &Fixture, module: &str) -> String {
    manifest_file_field(fx, module, "target")
}

fn manifest_file_field(fx: &Fixture, module: &str, field: &str) -> String {
    let manifest = fx.root.join(module).join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    let parsed: toml::Value = toml::from_str(&body).expect("module manifest parses");
    parsed
        .get("file")
        .and_then(toml::Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(|entry| entry.get(field))
        .and_then(toml::Value::as_str)
        .expect("the single [[file]] entry has the requested field")
        .to_owned()
}
