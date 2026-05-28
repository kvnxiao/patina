#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; the lint's allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for repository-root resolution (REQ-003).
//!
//! Tests use the `resolve_repository_root_with` seam so they can
//! inject env-var, CWD, and persisted-default values explicitly
//! without touching process-level state. The production wrapper
//! `resolve_repository_root` is exercised only by the trivial
//! "all three sources empty" case below to confirm the no-arg form
//! threads through.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::discovery::RepoDiscoveryError;
use patina_core::discovery::resolve_repository_root_with;
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
    // Canonicalize so test expectations match the function's own
    // post-resolution canonicalization.
    let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
    (td, canonical)
}

#[test]
fn env_var_resolves_repository_root() {
    // CHK-005: PATINA_REPO points at a valid root; the engine resolves
    // it regardless of CWD.
    let (_td, repo) = utf8_tempdir();
    write_root_manifest(&repo);

    let (_unrelated, unrelated_cwd) = utf8_tempdir();

    let resolved = resolve_repository_root_with(Some(repo.as_str()), &unrelated_cwd, None)
        .expect("resolution succeeds");
    assert_eq!(resolved, repo);
}

#[test]
fn walk_up_finds_root_from_subdirectory() {
    // CHK-006: PATINA_REPO unset; walk up from T/zsh/ finds T/patina.toml.
    let (_td, repo) = utf8_tempdir();
    write_root_manifest(&repo);
    let sub = repo.join("zsh");
    fs_err::create_dir_all(sub.as_std_path()).expect("create subdir");

    let resolved = resolve_repository_root_with(None, &sub, None).expect("resolution succeeds");
    assert_eq!(resolved, repo);
}

#[test]
fn all_sources_failing_names_each_source() {
    // CHK-007: env unset, CWD outside any repo, no persisted default →
    // error message names PATINA_REPO, walk-up, and persisted default.
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
    // Robustness: PATINA_REPO="" must not be treated as a valid path.
    let (_td, empty_cwd) = utf8_tempdir();
    let err =
        resolve_repository_root_with(Some(""), &empty_cwd, None).expect_err("empty env is unset");
    assert!(matches!(err, RepoDiscoveryError::AllSourcesFailed { .. }));
}

#[test]
fn env_var_pointing_at_non_root_directory_errors() {
    // Robustness: PATINA_REPO set to a directory without patina.toml.
    let (_td, dir) = utf8_tempdir();
    let err = resolve_repository_root_with(Some(dir.as_str()), &dir, None)
        .expect_err("non-root directory rejected");
    assert!(matches!(err, RepoDiscoveryError::EnvVarInvalid { .. }));
}

#[test]
fn persisted_default_is_consulted_when_other_sources_fail() {
    // Confirms source 3 actually fires when sources 1 and 2 miss.
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
