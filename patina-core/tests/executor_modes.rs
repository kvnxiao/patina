#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]
#![expect(
    clippy::indexing_slicing,
    reason = "integration tests use direct [0] / [1] indexing for assertion-only record inspection where the vector length is asserted immediately above; a bounds-check panic is acceptable test signal."
)]
#![expect(
    clippy::cloned_ref_to_slice_refs,
    reason = "single-target fixtures read most clearly as a `&[target.clone()]` slice literal alongside the multi-target `&[t1, t2]` cases they sit beside."
)]

//! Integration coverage for the five file-mode executors (REQ-005,
//! T-014). Each test drives the public [`materialize`] entry point against
//! a real tempdir fixture and asserts the materialized filesystem object
//! matches the mode's contract: symlink readlink targets, byte content for
//! copies, rendered output for templates.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::Builtins;
use patina_core::FileMode;
use patina_core::Materialization;
use patina_core::Resolver;
use patina_core::TemplateEngine;
use patina_core::materialize;
use tempfile::TempDir;

fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
    let td = TempDir::new().expect("create tempdir");
    let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
    let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
    (td, canonical)
}

/// Read a link's target and canonicalize it. The CHK contract is
/// "readlink target equals the canonical source"; canonicalizing both
/// sides makes the assertion independent of the platform's readlink
/// representation (Windows returns the verbatim `\\?\` form).
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

fn resolver() -> Resolver {
    Resolver::new(Builtins::for_tests())
}

/// CHK-010 / CHK-041: a file source with `mode = "symlink"` (and the
/// default mode) materializes the target as a symlink whose readlink
/// target equals the canonical source path.
#[test]
fn symlink_mode_links_to_canonical_source() {
    let (_td, dir) = utf8_tempdir();
    let source = dir.join("zshrc");
    fs_err::write(&source, b"export PATH").expect("write source");
    let target = dir.join("home").join(".zshrc");

    let records = materialize(
        FileMode::Symlink,
        &source,
        &[target.clone()],
        &TemplateEngine::new(),
        &resolver(),
    )
    .expect("symlink materializes");

    assert_eq!(records.len(), 1);
    assert_eq!(read_link_canonical(&target), canonical(&source));
    assert!(matches!(
        &records[0].materialization,
        Materialization::Symlink { link_target } if *link_target == source
    ));
}

/// REQ-005 done-when: a directory source under `symlink` mode produces one
/// symlink per file at the mirrored target path (no atomic dir symlink).
#[test]
fn symlink_mode_directory_source_walks_per_file() {
    let (_td, dir) = utf8_tempdir();
    let src = dir.join("config");
    fs_err::create_dir_all(src.join("nvim")).expect("mkdir nvim");
    fs_err::write(src.join("alias.sh"), b"alias g=git").expect("write alias");
    fs_err::write(src.join("nvim").join("init.lua"), b"-- init").expect("write init");
    let target = dir.join("dest");

    let records = materialize(
        FileMode::Symlink,
        &src,
        &[target.clone()],
        &TemplateEngine::new(),
        &resolver(),
    )
    .expect("walked symlinks");

    assert_eq!(records.len(), 2);
    assert_eq!(
        read_link_canonical(&target.join("alias.sh")),
        canonical(&src.join("alias.sh"))
    );
    assert_eq!(
        read_link_canonical(&target.join("nvim").join("init.lua")),
        canonical(&src.join("nvim").join("init.lua"))
    );
    // The target directory itself is a real directory, not a single link.
    assert!(
        !fs_err::symlink_metadata(&target)
            .expect("target metadata")
            .file_type()
            .is_symlink()
    );
}

/// REQ-005 behavior: `symlink-dir` materializes a single directory symlink
/// at the target and does not walk into the source.
#[test]
fn symlink_dir_mode_creates_single_atomic_link() {
    let (_td, dir) = utf8_tempdir();
    let src = dir.join("nvim");
    fs_err::create_dir_all(&src).expect("mkdir source");
    fs_err::write(src.join("init.lua"), b"-- cfg").expect("write child");
    let target = dir.join(".config").join("nvim");

    let records = materialize(
        FileMode::SymlinkDir,
        &src,
        &[target.clone()],
        &TemplateEngine::new(),
        &resolver(),
    )
    .expect("dir symlink");

    assert_eq!(records.len(), 1);
    assert_eq!(read_link_canonical(&target), canonical(&src));
    assert!(
        fs_err::symlink_metadata(&target)
            .expect("target metadata")
            .file_type()
            .is_symlink()
    );
}

/// CHK-043 (single-target slice): `copy` mode materializes a regular file
/// whose byte content equals the source.
#[test]
fn copy_mode_writes_byte_identical_file() {
    let (_td, dir) = utf8_tempdir();
    let source = dir.join("agent.toml");
    fs_err::write(&source, b"name = patina").expect("write source");
    let target = dir.join("claude").join("agent.toml");

    let records = materialize(
        FileMode::Copy,
        &source,
        &[target.clone()],
        &TemplateEngine::new(),
        &resolver(),
    )
    .expect("copy");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].materialization, Materialization::Copy);
    assert_eq!(fs_err::read(&target).expect("read copy"), b"name = patina");
    assert!(
        !fs_err::symlink_metadata(&target)
            .expect("target metadata")
            .file_type()
            .is_symlink()
    );
}

/// REQ-005 done-when: `copy-tree` materializes a directory tree of regular
/// files mirroring the source.
#[test]
fn copy_tree_mode_mirrors_directory() {
    let (_td, dir) = utf8_tempdir();
    let src = dir.join("src");
    fs_err::create_dir_all(src.join("deep")).expect("mkdir deep");
    fs_err::write(src.join("a.txt"), b"a").expect("write a");
    fs_err::write(src.join("deep").join("b.txt"), b"b").expect("write b");
    let target = dir.join("dest");

    let records = materialize(
        FileMode::CopyTree,
        &src,
        &[target.clone()],
        &TemplateEngine::new(),
        &resolver(),
    )
    .expect("copy tree");

    assert_eq!(records.len(), 2);
    assert_eq!(fs_err::read(target.join("a.txt")).expect("read a"), b"a");
    assert_eq!(
        fs_err::read(target.join("deep").join("b.txt")).expect("read b"),
        b"b"
    );
}

/// CHK-011: a `.tmpl` source renders through `MiniJinja` and materializes at
/// the target with the `.tmpl` suffix stripped as a regular file, and the
/// `.tmpl` path does not exist at the target.
#[test]
fn template_render_mode_strips_suffix_and_renders() {
    let (_td, dir) = utf8_tempdir();
    let source = dir.join("gitconfig.tmpl");
    fs_err::write(&source, b"[user]\n    email = {{ email }}").expect("write template");
    let target = dir.join("home").join("gitconfig.tmpl");

    let resolver = resolver()
        .with_repo_shared([("email", "kevin@example.com")])
        .expect("layer accepted");

    let records = materialize(
        FileMode::TemplateRender,
        &source,
        &[target.clone()],
        &TemplateEngine::new(),
        &resolver,
    )
    .expect("render");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].materialization, Materialization::Render);
    let output = dir.join("home").join("gitconfig");
    assert_eq!(
        fs_err::read_to_string(&output).expect("read rendered"),
        "[user]\n    email = kevin@example.com"
    );
    assert!(!target.exists());
}
