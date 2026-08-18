//! Integration tests for `apply_idempotency`.
#![expect(
    clippy::expect_used,
    reason = "Integration tests use .expect() for fixtures and asserted output outside #[cfg(test)] modules; allow-expect-in-tests does not cover integration-crate roots."
)]

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::code;

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

fn assert_source_file(source: &Utf8Path, bytes: &[u8]) {
    let meta = fs_err::symlink_metadata(source.as_std_path()).expect("stat source");
    assert!(
        meta.file_type().is_file() && !meta.file_type().is_symlink(),
        "{source} must remain a regular file"
    );
    assert_eq!(
        fs_err::read(source.as_std_path()).expect("read source"),
        bytes,
        "{source} bytes must be preserved across re-apply"
    );
}

#[test]
fn dir_symlink_re_apply_is_idempotent() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[directory]]\nsource = \"d\"\ntarget = \"~/out\"\nmode = \"symlink\"\n",
    );
    let src = module.join("d");
    fs_err::create_dir_all(&src).expect("mkdir source dir");
    fs_err::write(src.join("a.conf"), b"a").expect("write source leaf");

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "first apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "atomic dir-symlink re-apply must converge, not error; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let out = f.home.join("out");
    assert!(
        fs_err::symlink_metadata(out.as_std_path())
            .expect("stat out")
            .file_type()
            .is_symlink(),
        "~/out must be a directory symlink"
    );
    assert_eq!(
        read_link_canonical(&out),
        canonical(&src),
        "the link must point at the source directory"
    );
}

#[test]
fn all_file_modes_re_apply_preserves_sources() {
    let f = Fixture::new();
    let manifest = r#"
[[file]]
source = "f_sym"
target = "~/f_sym"

[[file]]
source = "f_copy"
target = "~/f_copy"
mode = "copy"

[[file]]
source = "t.tmpl"
target = "~/t_out"

[[directory]]
source = "d_tree"
target = "~/d_tree"
mode = "symlink-tree"

[[directory]]
source = "d_sym"
target = "~/d_sym"
mode = "symlink"

[[directory]]
source = "d_copy"
target = "~/d_copy"
mode = "copy"
"#;
    let m = f.module("m", manifest);
    fs_err::write(m.join("f_sym"), b"sym-src").expect("write f_sym");
    fs_err::write(m.join("f_copy"), b"copy-src").expect("write f_copy");
    fs_err::write(m.join("t.tmpl"), b"os={{ patina.os }}\n").expect("write template");
    fs_err::create_dir_all(m.join("d_tree")).expect("mkdir d_tree");
    fs_err::write(m.join("d_tree").join("leaf.conf"), b"tree-src").expect("write d_tree leaf");
    fs_err::create_dir_all(m.join("d_sym")).expect("mkdir d_sym");
    fs_err::write(m.join("d_sym").join("inner.conf"), b"dsym-src").expect("write d_sym leaf");
    fs_err::create_dir_all(m.join("d_copy")).expect("mkdir d_copy");
    fs_err::write(m.join("d_copy").join("c.conf"), b"dcopy-src").expect("write d_copy leaf");

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "first apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "re-apply over unchanged source must be clean across all modes; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    assert_source_file(&m.join("f_sym"), b"sym-src");
    assert_source_file(&m.join("f_copy"), b"copy-src");
    assert_source_file(&m.join("t.tmpl"), b"os={{ patina.os }}\n");
    assert_source_file(&m.join("d_tree").join("leaf.conf"), b"tree-src");
    assert_source_file(&m.join("d_sym").join("inner.conf"), b"dsym-src");
    assert_source_file(&m.join("d_copy").join("c.conf"), b"dcopy-src");
}
