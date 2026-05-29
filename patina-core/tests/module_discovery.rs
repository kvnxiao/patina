#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; the lint's allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration tests for module enumeration (REQ-004).

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::discovery::ModuleDiscoveryError;
use patina_core::discovery::discover_modules;
use tempfile::TempDir;

fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
    let td = TempDir::new().expect("create tempdir");
    let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
    let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
    (td, canonical)
}

fn write_file(path: &Utf8Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent.as_std_path()).expect("create parents");
    }
    fs_err::write(path.as_std_path(), contents).expect("write file");
}

#[test]
fn discovers_modules_alphabetically_with_absolute_paths() {
    // CHK-008: T/patina.toml (root), T/zsh/patina.toml, T/nvim/patina.toml
    // discovers exactly {zsh, nvim}, alphabetically ordered.
    let (_td, root) = utf8_tempdir();
    write_file(&root.join("patina.toml"), "[patina]\nroot = true\n");
    write_file(&root.join("zsh").join("patina.toml"), "");
    write_file(&root.join("nvim").join("patina.toml"), "");

    let modules = discover_modules(&root).expect("discovery succeeds");
    let names: Vec<&str> = modules.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["nvim", "zsh"]);

    for module in &modules {
        assert!(
            module.path.is_absolute(),
            "module path must be absolute: {}",
            module.path
        );
        assert!(module.path.starts_with(&root));
    }
}

#[test]
fn rejects_manifest_at_depth_two() {
    // CHK-009: T/zsh/plugins/patina.toml triggers MaximumModuleDepth.
    let (_td, root) = utf8_tempdir();
    write_file(&root.join("patina.toml"), "[patina]\nroot = true\n");
    write_file(&root.join("zsh").join("plugins").join("patina.toml"), "");

    let err = discover_modules(&root).expect_err("depth-2 manifest rejected");
    let rendered = err.to_string();

    assert!(matches!(
        err,
        ModuleDiscoveryError::MaximumModuleDepth { .. }
    ));
    assert!(
        rendered.contains("zsh/plugins/patina.toml")
            || rendered.contains("zsh\\plugins\\patina.toml"),
        "error must name the offending path; got: {rendered}"
    );
    assert!(
        rendered.contains("maximum module depth"),
        "error must contain `maximum module depth`; got: {rendered}"
    );
}

#[test]
fn rejects_non_root_manifest_declaring_root_true() {
    // Task scenario: T/zsh/patina.toml contains `[patina]\nroot = true`.
    let (_td, root) = utf8_tempdir();
    write_file(&root.join("patina.toml"), "[patina]\nroot = true\n");
    write_file(
        &root.join("zsh").join("patina.toml"),
        "[patina]\nroot = true\n",
    );

    let err = discover_modules(&root).expect_err("unexpected root key rejected");
    let rendered = err.to_string();
    assert!(matches!(
        err,
        ModuleDiscoveryError::UnexpectedRootKey { .. }
    ));
    assert!(
        rendered.contains("root"),
        "must mention root key: {rendered}"
    );
    assert!(
        rendered.contains("patina.toml"),
        "must name offending file: {rendered}"
    );
}

#[test]
fn rejects_root_manifest_missing_root_key() {
    // Task scenario: T/patina.toml lacks `root = true`.
    let (_td, root) = utf8_tempdir();
    write_file(&root.join("patina.toml"), "");

    let err = discover_modules(&root).expect_err("missing root key rejected");
    let rendered = err.to_string();
    assert!(matches!(err, ModuleDiscoveryError::MissingRootKey { .. }));
    assert!(
        rendered.contains("root = true") || rendered.contains("`root`"),
        "must mention missing root key: {rendered}"
    );
    assert!(
        rendered.contains("patina.toml"),
        "must name the file: {rendered}"
    );
}

#[test]
fn non_module_subdirectories_are_silently_skipped() {
    // Subdirectories without a patina.toml (e.g. `.git`, scratch dirs)
    // are not modules.
    let (_td, root) = utf8_tempdir();
    write_file(&root.join("patina.toml"), "[patina]\nroot = true\n");
    write_file(&root.join("zsh").join("patina.toml"), "");
    fs_err::create_dir_all(root.join(".git").as_std_path()).expect("create .git");

    let modules = discover_modules(&root).expect("discovery succeeds");
    let names: Vec<&str> = modules.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["zsh"]);
}
