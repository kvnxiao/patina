#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]
#![expect(
    clippy::indexing_slicing,
    reason = "integration tests use direct [0] / [1] indexing for assertion-only record inspection where the vector length is asserted immediately above; a bounds-check panic is acceptable test signal."
)]

//! Multi-target fan-out coverage for the file-mode executors. A `[[file]]`
//! entry declaring
//! `targets = [t1, t2, ...]` materializes the source at every target
//! according to the mode, and a `.tmpl` source is rendered once and
//! written to each target.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::Builtins;
use patina_core::FileMode;
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

/// Read a link's target and canonicalize it; see `executor_modes.rs` for
/// why both sides are canonicalized before comparison.
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

/// A multi-target `symlink` entry fans out to N symbolic links,
/// each pointing at the same canonical source path.
#[test]
fn symlink_fans_out_to_every_target() {
    let (_td, dir) = utf8_tempdir();
    let source = dir.join("agent.toml");
    fs_err::write(&source, b"shared").expect("write source");
    let t1 = dir.join("claude").join("agent.toml");
    let t2 = dir.join("codex").join("agent.toml");

    let records = materialize(
        FileMode::Symlink,
        &source,
        &[t1.clone(), t2.clone()],
        &TemplateEngine::new(),
        &resolver(),
    )
    .expect("fan-out symlinks");

    assert_eq!(records.len(), 2);
    assert_eq!(read_link_canonical(&t1), canonical(&source));
    assert_eq!(read_link_canonical(&t2), canonical(&source));
    // Records carry per-target granularity in target order.
    assert_eq!(records[0].target, t1);
    assert_eq!(records[1].target, t2);
}

/// A multi-target `copy` entry writes a byte copy at every
/// target.
#[test]
fn copy_fans_out_to_every_target() {
    let (_td, dir) = utf8_tempdir();
    let source = dir.join("agent.toml");
    fs_err::write(&source, b"name = shared").expect("write source");
    let t1 = dir.join("claude").join("agent.toml");
    let t2 = dir.join("codex").join("agent.toml");

    let records = materialize(
        FileMode::Copy,
        &source,
        &[t1.clone(), t2.clone()],
        &TemplateEngine::new(),
        &resolver(),
    )
    .expect("fan-out copies");

    assert_eq!(records.len(), 2);
    assert_eq!(fs_err::read(&t1).expect("read t1"), b"name = shared");
    assert_eq!(fs_err::read(&t2).expect("read t2"), b"name = shared");
    for target in [&t1, &t2] {
        assert!(
            !fs_err::symlink_metadata(target)
                .expect("target metadata")
                .file_type()
                .is_symlink()
        );
    }
}

/// A multi-target `.tmpl` *source* entry renders the template once
/// against the resolved context and writes the same rendered bytes to each
/// declared target. The targets are declared
/// suffix-less (`source = "agent.toml.tmpl"`,
/// `targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]`); the executor
/// writes to each declared target verbatim.
#[test]
fn template_renders_once_and_writes_each_target() {
    let (_td, dir) = utf8_tempdir();
    let source = dir.join("agent.toml.tmpl");
    fs_err::write(&source, b"name = {{ who }}").expect("write template");
    let t1 = dir.join("claude").join("agent.toml");
    let t2 = dir.join("codex").join("agent.toml");

    let resolver = resolver()
        .with_repo_shared([("who", "patina")])
        .expect("layer accepted");

    let records = materialize(
        FileMode::TemplateRender,
        &source,
        &[t1.clone(), t2.clone()],
        &TemplateEngine::new(),
        &resolver,
    )
    .expect("render fan-out");

    assert_eq!(records.len(), 2);
    assert_eq!(records[0].target, t1);
    assert_eq!(records[1].target, t2);
    let rendered1 = fs_err::read_to_string(&t1).expect("read t1");
    let rendered2 = fs_err::read_to_string(&t2).expect("read t2");
    assert_eq!(rendered1, "name = patina");
    // Render-once guarantee: the two targets receive byte-identical output.
    assert_eq!(rendered1, rendered2);
}
