//! Integration tests for `symlink_reapply`.
#![expect(
    clippy::expect_used,
    reason = "Integration tests use .expect() for fixtures and asserted output outside #[cfg(test)] modules; allow-expect-in-tests does not cover integration-crate roots."
)]

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

    assert_source_intact(&source);
    assert_eq!(
        read_link_canonical(&target),
        canonical(&source),
        "the target must still link to the source after re-apply"
    );
}

#[test]
fn single_file_symlink_apply_over_foreign_symlink_preserves_source() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[file]]\nsource = \"foo.conf\"\ntarget = \"~/foo.conf\"\n",
    );
    let source = module.join("foo.conf");
    fs_err::write(source.as_std_path(), b"managed").expect("write source");

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
