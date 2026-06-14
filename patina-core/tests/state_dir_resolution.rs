#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; the lint's allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for per-machine state directory resolution.
//!
//! These tests exercise `state_dir::resolve_with_env`, the testable
//! core that takes an explicit [`HostOs`] and an environment-lookup
//! closure. Tests run against real tempdir filesystems so the
//! idempotent-creation contract is exercised end-to-end, but the OS
//! family is supplied explicitly so every layout branch runs on
//! every CI host.

use camino::Utf8PathBuf;
use patina_core::HostOs;
use patina_core::StateDirError;
use patina_core::state_dir::resolve_with_env;
use tempfile::TempDir;

fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
    let td = TempDir::new().expect("create tempdir");
    let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
    let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
    (td, canonical)
}

fn env_map(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> + use<> {
    move |name| {
        pairs
            .iter()
            .find_map(|(k, v)| (*k == name).then(|| v.clone()))
            .filter(|v| !v.is_empty())
    }
}

#[test]
fn linux_xdg_state_home_creates_tree_and_is_idempotent() {
    // Scenario 1: Linux + XDG_STATE_HOME=T → T/patina/ with journal/
    // and backups/ subdirs; a second call is a no-op.
    let (_keep, t) = utf8_tempdir();
    let env = env_map(vec![("XDG_STATE_HOME", t.to_string())]);

    let first = resolve_with_env(HostOs::Linux, &env).expect("first resolve");
    assert_eq!(first, t.join("patina"));
    assert!(first.is_dir(), "patina root must exist");
    assert!(first.join("journal").is_dir(), "journal/ must exist");
    assert!(first.join("backups").is_dir(), "backups/ must exist");

    // Idempotency: second call returns same path and does not error
    // even though the directories already exist.
    let second = resolve_with_env(HostOs::Linux, &env).expect("second resolve");
    assert_eq!(first, second);
}

#[test]
fn linux_without_xdg_resolves_under_home_dot_local_state() {
    // Scenario 2: Linux + XDG_STATE_HOME unset + HOME=H →
    // H/.local/state/patina/.
    let (_keep, h) = utf8_tempdir();
    let env = env_map(vec![("HOME", h.to_string())]);

    let root = resolve_with_env(HostOs::Linux, &env).expect("resolve");
    assert_eq!(root, h.join(".local").join("state").join("patina"));
    assert!(root.is_dir());
    assert!(root.join("journal").is_dir());
    assert!(root.join("backups").is_dir());
}

#[test]
fn macos_resolves_under_application_support() {
    // Scenario 3: macOS + HOME=H → H/Library/Application Support/patina/.
    let (_keep, h) = utf8_tempdir();
    let env = env_map(vec![("HOME", h.to_string())]);

    let root = resolve_with_env(HostOs::MacOs, &env).expect("resolve");
    assert_eq!(
        root,
        h.join("Library").join("Application Support").join("patina")
    );
    assert!(root.is_dir());
    assert!(root.join("journal").is_dir());
    assert!(root.join("backups").is_dir());
}

#[test]
fn windows_resolves_under_localappdata() {
    // Scenario 4: Windows + LOCALAPPDATA=L → L\patina\.
    let (_keep, l) = utf8_tempdir();
    let env = env_map(vec![("LOCALAPPDATA", l.to_string())]);

    let root = resolve_with_env(HostOs::Windows, &env).expect("resolve");
    assert_eq!(root, l.join("patina"));
    assert!(root.is_dir());
    assert!(root.join("journal").is_dir());
    assert!(root.join("backups").is_dir());
}

#[test]
fn resolve_does_not_create_lazy_files() {
    // The owners of `profile`, `default_repo`, and
    // `lock` create those files lazily. `resolve` must only
    // create the directory tree, not the files.
    let (_keep, t) = utf8_tempdir();
    let env = env_map(vec![("XDG_STATE_HOME", t.to_string())]);
    let root = resolve_with_env(HostOs::Linux, &env).expect("resolve");

    assert!(!root.join("profile").exists(), "profile must be lazy");
    assert!(
        !root.join("default_repo").exists(),
        "default_repo must be lazy"
    );
    assert!(!root.join("lock").exists(), "lock must be lazy");
}

#[test]
fn resolve_does_not_write_to_external_repository() {
    // Given a tempdir repository alongside a resolved state directory,
    // when resolve runs, then no file under the repository directory
    // was modified by the engine. (The dotfiles repository is
    // never written to.)
    let (_keep_state, t) = utf8_tempdir();
    let (_keep_repo, repo) = utf8_tempdir();

    // Seed a file in the repo and snapshot its mtime + content.
    let seed = repo.join("patina.toml");
    fs_err::write(seed.as_std_path(), b"# seed\n").expect("seed write");
    let mtime_before = fs_err::metadata(seed.as_std_path())
        .expect("stat before")
        .modified()
        .expect("mtime before");
    let content_before = fs_err::read(seed.as_std_path()).expect("read before");

    let env = env_map(vec![("XDG_STATE_HOME", t.to_string())]);
    let root = resolve_with_env(HostOs::Linux, &env).expect("resolve");
    assert_eq!(root, t.join("patina"));

    let mtime_after = fs_err::metadata(seed.as_std_path())
        .expect("stat after")
        .modified()
        .expect("mtime after");
    let content_after = fs_err::read(seed.as_std_path()).expect("read after");
    assert_eq!(mtime_before, mtime_after, "repo file mtime must not change");
    assert_eq!(
        content_before, content_after,
        "repo file content must not change"
    );

    // The repo directory must contain only the seeded file.
    let entries: Vec<_> = fs_err::read_dir(repo.as_std_path())
        .expect("read repo dir")
        .collect::<Result<Vec<_>, _>>()
        .expect("read entries");
    assert_eq!(entries.len(), 1, "repo must contain only the seeded file");
}

#[test]
fn linux_missing_home_and_xdg_errors_with_home_named() {
    let env = env_map(vec![]);
    let err = resolve_with_env(HostOs::Linux, &env).expect_err("must error");
    assert!(
        matches!(err, StateDirError::MissingEnv { name: "HOME" }),
        "got {err:?}"
    );
}

#[test]
fn windows_missing_localappdata_errors_with_localappdata_named() {
    let env = env_map(vec![]);
    let err = resolve_with_env(HostOs::Windows, &env).expect_err("must error");
    assert!(
        matches!(
            err,
            StateDirError::MissingEnv {
                name: "LOCALAPPDATA"
            }
        ),
        "got {err:?}"
    );
}
