//! Integration tests for symlink tree.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixtures and asserted output; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
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

fn assert_real_dir(path: &Utf8Path) {
    let meta = fs_err::symlink_metadata(path.as_std_path()).expect("stat path");
    assert!(
        meta.file_type().is_dir() && !meta.file_type().is_symlink(),
        "{path} must be a real directory, not a symbolic link"
    );
}

#[test]
fn symlink_tree_links_each_leaf_and_keeps_intermediate_dirs_real() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[directory]]\nsource = \"d\"\ntarget = \"~/d\"\nmode = \"symlink-tree\"\n",
    );
    let src = module.join("d");
    fs_err::create_dir_all(src.join("sub")).expect("mkdir sub");
    fs_err::write(src.join("a.conf"), b"a").expect("write a");
    fs_err::write(src.join("sub").join("b.conf"), b"b").expect("write b");

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "a `symlink-tree` apply must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let d = f.home.join("d");
    let a = d.join("a.conf");
    let b = d.join("sub").join("b.conf");
    assert_eq!(
        read_link_canonical(&a),
        canonical(&src.join("a.conf")),
        "~/d/a.conf must resolve to the source leaf"
    );
    assert_eq!(
        read_link_canonical(&b),
        canonical(&src.join("sub").join("b.conf")),
        "~/d/sub/b.conf must resolve to the nested source leaf"
    );
    assert_real_dir(&d);
    assert_real_dir(&d.join("sub"));
}

#[test]
fn symlink_tree_backs_up_pre_existing_leaf_and_replaces_it_with_a_link() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[directory]]\nsource = \"d\"\ntarget = \"~/d\"\nmode = \"symlink-tree\"\n",
    );
    let src = module.join("d");
    fs_err::create_dir_all(&src).expect("mkdir source");
    fs_err::write(src.join("a.conf"), b"managed").expect("write source leaf");

    let d = f.home.join("d");
    let leaf = d.join("a.conf");
    fs_err::create_dir_all(&d).expect("mkdir target dir");
    fs_err::write(&leaf, b"original").expect("write pre-existing leaf");

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "apply over a pre-existing leaf must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs_err::symlink_metadata(leaf.as_std_path())
            .expect("stat leaf after apply")
            .file_type()
            .is_symlink(),
        "the pre-existing regular file leaf must be replaced by a symlink"
    );
    assert_eq!(
        read_link_canonical(&leaf),
        canonical(&src.join("a.conf")),
        "the leaf link must resolve to the source"
    );

    let rolled = f.run(&["rollback", "--yes"], &[]);
    assert_eq!(
        code(&rolled),
        0,
        "rollback must succeed; stderr: {}",
        String::from_utf8_lossy(&rolled.stderr)
    );
    let restored = fs_err::symlink_metadata(leaf.as_std_path()).expect("stat leaf after rollback");
    assert!(
        restored.file_type().is_file() && !restored.file_type().is_symlink(),
        "rollback must restore the leaf to a regular file"
    );
    assert_eq!(
        fs_err::read(leaf.as_std_path()).expect("read restored leaf"),
        b"original",
        "the prior leaf bytes must be recovered from the backup"
    );
}

#[test]
fn apply_refuses_a_foreign_symlink_planted_inside_a_managed_tree() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[directory]]\nsource = \"d\"\ntarget = \"~/d\"\nmode = \"symlink-tree\"\n",
    );
    let src = module.join("d");
    fs_err::create_dir_all(src.join("sub")).expect("mkdir sub");
    fs_err::write(src.join("sub").join("b.conf"), b"repo bytes").expect("write b");

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "first apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    // Replace the real interior directory with a link into the repository:
    // a leaf write through it would delete and re-link the repo's own file.
    let interior = f.home.join("d").join("sub");
    fs_err::remove_dir_all(interior.as_std_path()).expect("clear interior dir");
    common::symlink_dir(&src.join("sub"), &interior);

    let second = f.apply(&["--yes"]);
    assert_ne!(
        code(&second),
        0,
        "an apply over a symlinked interior directory must fail"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("is a symbolic link; remove the link"),
        "stderr must name the refusal: {stderr}"
    );
    let leaf = src.join("sub").join("b.conf");
    assert!(
        fs_err::symlink_metadata(leaf.as_std_path())
            .expect("stat repo leaf")
            .file_type()
            .is_file(),
        "the repository source leaf must survive as a regular file"
    );
    assert_eq!(
        fs_err::read(leaf.as_std_path()).expect("read repo leaf"),
        b"repo bytes",
        "no write may reach the repository through the planted link"
    );
}

#[test]
fn symlink_tree_skips_empty_source_subdirectory() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[directory]]\nsource = \"d\"\ntarget = \"~/d\"\nmode = \"symlink-tree\"\n",
    );
    let src = module.join("d");
    fs_err::create_dir_all(src.join("empty")).expect("mkdir empty");
    fs_err::write(src.join("a.conf"), b"a").expect("write a");

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "apply must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let d = f.home.join("d");
    assert!(
        fs_err::symlink_metadata(d.join("a.conf").as_std_path()).is_ok(),
        "the real leaf must be linked"
    );
    assert!(
        !d.join("empty").exists(),
        "an empty source subdir must produce no target directory"
    );
}

#[test]
fn symlink_tree_re_apply_over_unchanged_source_is_a_noop() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[directory]]\nsource = \"d\"\ntarget = \"~/d\"\nmode = \"symlink-tree\"\n",
    );
    let src = module.join("d");
    fs_err::create_dir_all(src.join("sub")).expect("mkdir sub");
    fs_err::write(src.join("a.conf"), b"a").expect("write a");
    fs_err::write(src.join("sub").join("b.conf"), b"b").expect("write b");

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
        "re-apply over unchanged source must succeed; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let d = f.home.join("d");
    assert_eq!(
        read_link_canonical(&d.join("a.conf")),
        canonical(&src.join("a.conf"))
    );
    assert_eq!(
        read_link_canonical(&d.join("sub").join("b.conf")),
        canonical(&src.join("sub").join("b.conf"))
    );
}
