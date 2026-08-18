//! Integration tests for repo discovery.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; the lint's allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::discovery::RepoDiscoveryError;
use patina_core::discovery::default_repo_pointer_path;
use patina_core::discovery::persisted_default_present;
use patina_core::discovery::resolve_repository_root_with;
use patina_core::discovery::write_persisted_default;
use tempfile::TempDir;

fn write_root_manifest(dir: &Utf8Path) {
    fs_err::write(
        dir.join("patina.toml").as_std_path(),
        "[patina]\nroot = true\n",
    )
    .expect("write root manifest");
}

fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
    let td = TempDir::new().expect("create tempdir");
    let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
    let canon = dunce::canonicalize(path.as_std_path()).expect("canonicalize tempdir");
    let canonical = Utf8PathBuf::from_path_buf(canon).expect("canonical tempdir is utf-8");
    (td, canonical)
}

#[test]
fn env_var_resolves_repository_root() {
    let (_td, repo) = utf8_tempdir();
    write_root_manifest(&repo);

    let (_unrelated, unrelated_cwd) = utf8_tempdir();

    let resolved = resolve_repository_root_with(Some(repo.as_str()), &unrelated_cwd, None)
        .expect("resolution succeeds");
    assert_eq!(resolved, repo);
}

#[test]
fn walk_up_finds_root_from_subdirectory() {
    let (_td, repo) = utf8_tempdir();
    write_root_manifest(&repo);
    let sub = repo.join("zsh");
    fs_err::create_dir_all(sub.as_std_path()).expect("create subdir");

    let resolved = resolve_repository_root_with(None, &sub, None).expect("resolution succeeds");
    assert_eq!(resolved, repo);
}

#[test]
fn all_sources_failing_names_each_source() {
    let (_td, empty_cwd) = utf8_tempdir();

    let err = resolve_repository_root_with(None, &empty_cwd, None).expect_err("resolution fails");
    let rendered = err.to_string();

    assert!(matches!(err, RepoDiscoveryError::AllSourcesFailed { .. }));
    assert!(
        rendered.contains("PATINA_REPO"),
        "error must name PATINA_REPO; got: {rendered}"
    );
    assert!(
        rendered.contains("walk-up"),
        "error must name walk-up; got: {rendered}"
    );
    assert!(
        rendered.contains("persisted default"),
        "error must name persisted default; got: {rendered}"
    );
}

#[test]
fn empty_env_var_is_treated_as_unset() {
    let (_td, empty_cwd) = utf8_tempdir();
    let err =
        resolve_repository_root_with(Some(""), &empty_cwd, None).expect_err("empty env is unset");
    assert!(matches!(err, RepoDiscoveryError::AllSourcesFailed { .. }));
}

#[test]
fn env_var_pointing_at_non_root_directory_errors() {
    let (_td, dir) = utf8_tempdir();
    let err = resolve_repository_root_with(Some(dir.as_str()), &dir, None)
        .expect_err("non-root directory rejected");
    assert!(matches!(err, RepoDiscoveryError::EnvVarInvalid { .. }));
}

#[test]
fn persisted_default_is_consulted_when_other_sources_fail() {
    let (_repo_td, repo) = utf8_tempdir();
    write_root_manifest(&repo);

    let (_state_td, state_dir) = utf8_tempdir();
    let persisted = state_dir.join("default_repo");
    fs_err::write(persisted.as_std_path(), repo.as_str()).expect("write persisted default");

    let (_cwd_td, empty_cwd) = utf8_tempdir();

    let resolved = resolve_repository_root_with(None, &empty_cwd, Some(persisted.as_path()))
        .expect("persisted default resolves");
    assert_eq!(resolved, repo);
}

#[test]
fn write_persisted_default_round_trips_through_read_path() {
    let (_repo_td, repo) = utf8_tempdir();
    write_root_manifest(&repo);

    let (_state_td, state_dir) = utf8_tempdir();

    write_persisted_default(&state_dir, &repo).expect("write persisted default");

    let pointer = default_repo_pointer_path(&state_dir);
    assert!(pointer.exists(), "pointer file must exist after write");

    let contents = fs_err::read_to_string(pointer.as_std_path()).expect("read pointer");
    assert_eq!(contents.trim(), repo.as_str());

    assert!(
        persisted_default_present(&state_dir),
        "presence check must report the pointer as present"
    );

    let (_cwd_td, empty_cwd) = utf8_tempdir();
    let resolved = resolve_repository_root_with(None, &empty_cwd, Some(pointer.as_path()))
        .expect("persisted default resolves through read path");
    assert_eq!(resolved, repo);
}

#[test]
fn persisted_default_present_is_false_without_pointer() {
    let (_state_td, state_dir) = utf8_tempdir();
    assert!(
        !persisted_default_present(&state_dir),
        "no pointer file means present() must be false"
    );
}
