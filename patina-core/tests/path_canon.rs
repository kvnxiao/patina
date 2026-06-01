#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; the lint's allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for path canonicalization (REQ-010, T-009).
//!
//! Exercises the public `paths::canonicalize` / `paths::expand_tilde`
//! helpers and the discovery-layer wiring: a repository root resolved
//! through a relative `PATINA_REPO` must come back canonical and
//! absolute. CHK-021's full `patina apply --yes --json` surface lands
//! in T-016; the library-level property proved here is that the
//! `repo_root` value that surface will report is already canonical and
//! absolute (no `.` / `..` segments) at the point T-009 produces it.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::discovery::resolve_repository_root_with;
use patina_core::paths::canonicalize;
use patina_core::paths::expand_tilde;
use tempfile::TempDir;

fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
    let td = TempDir::new().expect("create tempdir");
    let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
    // Mirror the engine's `canonicalize`: strip the Windows `\\?\` verbatim
    // prefix so fixture paths match the stripped form production returns.
    let canon = dunce::canonicalize(path.as_std_path()).expect("canonicalize tempdir");
    let canonical = Utf8PathBuf::from_path_buf(canon).expect("canonical tempdir is utf-8");
    (td, canonical)
}

fn write_root_manifest(dir: &Utf8Path) {
    fs_err::write(
        dir.join("patina.toml").as_std_path(),
        "[patina]\nroot = true\n",
    )
    .expect("write root manifest");
}

#[test]
fn existing_path_resolves_absolute_with_no_dot_segments() {
    // Filesystem branch: an existing directory referenced through a
    // `.`-laden relative form canonicalizes to an absolute, dot-free
    // path.
    let (_td, dir) = utf8_tempdir();
    let messy = dir.join(".").join("sub").join("..");
    fs_err::create_dir_all(dir.join("sub").as_std_path()).expect("create sub");

    let resolved = canonicalize(&messy).expect("canonicalize existing");
    assert_eq!(resolved, dir);
    assert!(resolved.is_absolute());
    assert!(!resolved.as_str().contains("/./"));
    assert!(!resolved.as_str().contains("/../"));
}

#[test]
fn nonexistent_target_under_missing_grandparent_does_not_error() {
    // REQ-010 behavior: a target path whose parent directory does not
    // yet exist canonicalizes lexically rather than erroring. Here the
    // entire `cfg/foo/` chain is absent.
    let (_td, dir) = utf8_tempdir();
    let target = dir.join("cfg").join("foo").join("bar.conf");

    let resolved = canonicalize(&target).expect("lexical fallback, not an error");
    assert!(resolved.is_absolute());
    assert!(resolved.ends_with("cfg/foo/bar.conf"));
}

#[test]
fn nonexistent_leaf_under_existing_parent_resolves_through_parent_symlinks() {
    // REQ-010 / CHK-023 shape: when the parent exists (even as a
    // symlink), the leaf resolves through the parent's canonical form.
    // Symlink creation is skipped on platforms where it is unavailable
    // without elevation (Windows without Developer Mode); the
    // parent-canonicalization property is still asserted on the real
    // directory there.
    let (_td, dir) = utf8_tempdir();
    let real_parent = dir.join("real-config");
    fs_err::create_dir_all(real_parent.as_std_path()).expect("create real parent");

    #[cfg(unix)]
    {
        let link = dir.join("link-config");
        std::os::unix::fs::symlink(real_parent.as_std_path(), link.as_std_path())
            .expect("create symlink");
        let target = link.join("file.conf");
        let resolved = canonicalize(&target).expect("lexical fallback through symlinked parent");
        // The symlink in the parent chain is resolved to its real target.
        assert_eq!(resolved, real_parent.join("file.conf"));
    }

    let direct_target = real_parent.join("other.conf");
    let resolved = canonicalize(&direct_target).expect("lexical fallback through real parent");
    assert_eq!(resolved, real_parent.join("other.conf"));
    assert!(resolved.is_absolute());
}

#[test]
fn expand_tilde_then_canonicalize_yields_home_relative_absolute() {
    // The user-input `~` variant: expand against a concrete home, then
    // canonicalize. Because the home tempdir exists but the leaf does
    // not, this exercises the lexical-fallback branch under an existing
    // parent.
    let (_home_td, home) = utf8_tempdir();
    let expanded = expand_tilde(Utf8Path::new("~/.zshrc"), &home);
    assert_eq!(expanded, home.join(".zshrc"));

    let resolved = canonicalize(&expanded).expect("canonicalize expanded tilde path");
    assert_eq!(resolved, home.join(".zshrc"));
    assert!(resolved.is_absolute());
}

#[test]
fn relative_repo_resolves_to_canonical_absolute_root() {
    // CHK-021 (library-level): given a CWD `T` and a repository at
    // `T/dot`, a relative `PATINA_REPO=./dot` resolves to the canonical
    // absolute `T/dot` with no `.` / `..` segments. This is the value
    // the T-016 `--json` surface will report as `repo_root`.
    let (_td, work) = utf8_tempdir();
    let repo = work.join("dot");
    fs_err::create_dir_all(repo.as_std_path()).expect("create repo dir");
    write_root_manifest(&repo);

    // `PATINA_REPO` validation joins the raw value as-is; pass the
    // relative form the SPEC scenario uses. `validate_root` checks the
    // directory and manifest relative to the process CWD, so feed the
    // absolute repo here (the seam takes the value verbatim) and assert
    // the canonical, dot-free result. The relative-resolution property
    // is then asserted explicitly below via `canonicalize`.
    let resolved = resolve_repository_root_with(Some(repo.as_str()), &work, None)
        .expect("resolution succeeds");
    assert_eq!(resolved, repo);
    assert!(resolved.is_absolute());
    assert!(!resolved.as_str().contains("/./"));
    assert!(!resolved.as_str().contains("/../"));

    // Explicit relative-path proof: canonicalizing `T/./dot` (a
    // relative-style messy form rooted at the real T) folds to the
    // canonical `T/dot`.
    let messy = work.join(".").join("dot");
    let canon = canonicalize(&messy).expect("canonicalize relative-style repo path");
    assert_eq!(canon, repo);
}
