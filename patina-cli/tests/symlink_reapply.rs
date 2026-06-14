#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixtures and asserted output; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Idempotency / migration safety for the default per-file
//! [`Symlink`](patina_core::FileMode::Symlink) mode: applying a `[[file]]`
//! symlink entry must never destroy its repository source — neither on a
//! re-apply over patina's own link, nor on a first apply over a *foreign*
//! tool's pre-existing symlink that already points into this repository (the
//! dotfile-manager migration case).
//!
//! Both reduce to one hazard: the engine must resolve a target by its declared
//! location and must not follow a symbolic link occupying the leaf back to the
//! repository source. If it did, the per-file executor — which removes the
//! target before re-linking — would delete the source and leave a
//! self-referential link.

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::code;

#[cfg(unix)]
fn symlink_file(source: &Utf8Path, link: &Utf8Path) {
    std::os::unix::fs::symlink(source.as_std_path(), link.as_std_path()).expect("create symlink");
}

#[cfg(windows)]
fn symlink_file(source: &Utf8Path, link: &Utf8Path) {
    std::os::windows::fs::symlink_file(source.as_std_path(), link.as_std_path())
        .expect("create symlink");
}

/// Read a symlink's target and canonicalize it so the assertion is independent
/// of the platform's `readlink` representation (Windows returns the verbatim
/// `\\?\` form; Unix returns the plain path).
fn read_link_canonical(target: &Utf8Path) -> Utf8PathBuf {
    let raw = fs_err::read_link(target.as_std_path()).expect("read_link target");
    let link_target = Utf8PathBuf::from_path_buf(raw).expect("link target is utf-8");
    link_target
        .canonicalize_utf8()
        .expect("canonicalize link target")
}

fn canonical(path: &Utf8Path) -> Utf8PathBuf {
    path.canonicalize_utf8().expect("canonicalize path")
}

/// Assert the repository source is still a regular file holding its original
/// bytes — i.e. the apply did not delete or relink it.
fn assert_source_intact(source: &Utf8Path) {
    let meta = fs_err::symlink_metadata(source.as_std_path()).expect("stat source");
    assert!(
        meta.file_type().is_file() && !meta.file_type().is_symlink(),
        "{source} must remain a regular file, not become a symlink"
    );
    assert_eq!(
        fs_err::read(source.as_std_path()).expect("read source"),
        b"managed",
        "the repository source bytes must be preserved"
    );
}

#[test]
fn single_file_symlink_re_apply_preserves_source() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[file]]\nsource = \"foo.conf\"\ntarget = \"~/foo.conf\"\n",
    );
    let source = module.join("foo.conf");
    fs_err::write(source.as_std_path(), b"managed").expect("write source");

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "first apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let target = f.home.join("foo.conf");
    assert_eq!(
        read_link_canonical(&target),
        canonical(&source),
        "the target must link to the source after the first apply"
    );
    assert_source_intact(&source);

    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "re-apply must succeed; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    // The regression: before the fix the re-apply canonicalized the target
    // through its own link to the source, and the executor deleted the source.
    assert_source_intact(&source);
    assert_eq!(
        read_link_canonical(&target),
        canonical(&source),
        "the target must still link to the source after re-apply"
    );
}

#[test]
fn single_file_symlink_apply_over_foreign_symlink_preserves_source() {
    // Migration case: another tool (e.g. dotter) already deployed the target
    // as a symlink pointing into this repository. Patina's first apply must
    // replace that link with its own without destroying the source.
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[file]]\nsource = \"foo.conf\"\ntarget = \"~/foo.conf\"\n",
    );
    let source = module.join("foo.conf");
    fs_err::write(source.as_std_path(), b"managed").expect("write source");

    // Pre-existing foreign symlink: ~/foo.conf -> <repo>/cfg/foo.conf.
    let target = f.home.join("foo.conf");
    symlink_file(&source, &target);

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "apply over a foreign symlink must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_source_intact(&source);
    assert_eq!(
        read_link_canonical(&target),
        canonical(&source),
        "the target must point at the source after apply"
    );
}
