#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

//! Integration coverage for `patina add`'s source-kind-aware table routing:
//! a file source writes a `[[file]]` entry, a directory source
//! writes a `[[directory]]` entry, and the mode flags are kind-checked.
//!
//! Each test spawns the real `patina` binary against an isolated tempdir
//! repo + state + home (via the shared [`common::Fixture`]).

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;

/// Decode stderr to a lossless string for assertions.
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Parse a module manifest into a TOML value.
fn manifest_value(fx: &Fixture, module: &str) -> toml::Value {
    let manifest = fx.root.join(module).join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    toml::from_str(&body).expect("module manifest parses")
}

/// `patina add F --module m` on a regular file writes a `[[file]]`
/// table-array entry and no `[[directory]]` entry.
#[test]
fn add_file_writes_file_table_and_no_directory_table() {
    let fx = Fixture::new();
    let file = fx.home.join("gitconfig");
    fs_err::write(file.as_std_path(), "bar").expect("seed file source");

    let out = fx.run(
        &["add", "~/gitconfig", "--module", "m", "--symlink", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 0, "add must exit 0; stderr: {}", stderr(&out));

    let parsed = manifest_value(&fx, "m");
    assert!(
        parsed.get("file").and_then(toml::Value::as_array).is_some(),
        "a [[file]] array must be present"
    );
    assert!(
        parsed.get("directory").is_none(),
        "no [[directory]] entry may be written for a file source"
    );
    let entries = parsed
        .get("file")
        .and_then(toml::Value::as_array)
        .expect("a [[file]] array");
    assert_eq!(entries.len(), 1, "exactly one [[file]] entry");
    let entry = entries.first().expect("the single [[file]] entry");
    assert_eq!(
        entry.get("mode").and_then(toml::Value::as_str),
        Some("symlink")
    );
}

/// `patina add D --module m --symlink-tree` on a directory writes a
/// `[[directory]]` entry with `mode = "symlink-tree"` and no `[[file]]`.
#[test]
fn add_directory_symlink_tree_writes_directory_table_with_mode() {
    let fx = Fixture::new();
    let dir = fx.home.join("nvim");
    fs_err::create_dir_all(dir.as_std_path()).expect("mkdir directory source");
    fs_err::write(dir.join("init.lua").as_std_path(), "-- cfg").expect("seed a leaf");
    fs_err::create_dir_all(dir.join("lua").as_std_path()).expect("mkdir nested");
    fs_err::write(dir.join("lua").join("opts.lua").as_std_path(), "-- opts")
        .expect("seed nested leaf");

    let out = fx.run(
        &["add", "~/nvim", "--module", "m", "--symlink-tree", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 0, "add must exit 0; stderr: {}", stderr(&out));

    let parsed = manifest_value(&fx, "m");
    assert!(
        parsed.get("file").is_none(),
        "no [[file]] entry may be written for a directory source"
    );
    let entries = parsed
        .get("directory")
        .and_then(toml::Value::as_array)
        .expect("a [[directory]] array");
    assert_eq!(entries.len(), 1, "exactly one [[directory]] entry");
    let entry = entries.first().expect("the single [[directory]] entry");
    assert_eq!(
        entry.get("source").and_then(toml::Value::as_str),
        Some("nvim")
    );
    assert_eq!(
        entry.get("target").and_then(toml::Value::as_str),
        Some("~/nvim")
    );
    assert_eq!(
        entry.get("mode").and_then(toml::Value::as_str),
        Some("symlink-tree")
    );

    // The whole directory tree was staged recursively into the repo.
    let staged = fx.root.join("m").join("nvim");
    assert!(staged.is_dir(), "<repo>/m/nvim must be a directory");
    assert_eq!(
        fs_err::read_to_string(staged.join("init.lua").as_std_path()).expect("read staged leaf"),
        "-- cfg"
    );
    assert_eq!(
        fs_err::read_to_string(staged.join("lua").join("opts.lua").as_std_path())
            .expect("read staged nested leaf"),
        "-- opts"
    );
}

/// A `[[directory]]` entry written by `add` is applyable — a
/// follow-up `patina apply` materializes the symlink-tree leaves, proving
/// `add` wrote a correct directory entry (not just correct manifest text).
#[test]
fn add_directory_then_apply_materializes_leaf_symlinks() {
    let fx = Fixture::new();
    let dir = fx.home.join("nvim");
    fs_err::create_dir_all(dir.as_std_path()).expect("mkdir directory source");
    fs_err::write(dir.join("init.lua").as_std_path(), "-- cfg").expect("seed a leaf");

    let add = fx.run(
        &["add", "~/nvim", "--module", "m", "--symlink-tree", "--yes"],
        &[],
    );
    assert_eq!(code(&add), 0, "add must exit 0; stderr: {}", stderr(&add));

    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );

    // The leaf target is now a symbolic link into the staged source.
    let leaf = fx.home.join("nvim").join("init.lua");
    assert!(
        is_symlink(&leaf),
        "~/nvim/init.lua must be a symbolic link after apply"
    );
}

/// `--symlink-tree` on a regular file source is rejected with a
/// typed error naming the flag and the file source kind; no entry is written.
#[test]
fn add_symlink_tree_on_a_file_is_rejected() {
    let fx = Fixture::new();
    let file = fx.home.join("gitconfig");
    fs_err::write(file.as_std_path(), "bar").expect("seed file source");

    let out = fx.run(
        &[
            "add",
            "~/gitconfig",
            "--module",
            "m",
            "--symlink-tree",
            "--yes",
        ],
        &[],
    );
    assert_eq!(code(&out), 1, "--symlink-tree on a file must exit 1");
    let stderr = stderr(&out);
    assert!(
        stderr.contains("--symlink-tree") && stderr.contains("file"),
        "stderr must name --symlink-tree and the file kind, got: {stderr}"
    );
    // No module manifest was written: the kind check runs before staging.
    assert!(
        !fx.root.join("m").join("patina.toml").exists(),
        "no manifest should be written on a kind-mismatch refusal"
    );
}

/// `--template` on a directory source is rejected with a typed
/// error naming the flag and the directory source kind.
#[test]
fn add_template_on_a_directory_is_rejected() {
    let fx = Fixture::new();
    let dir = fx.home.join("nvim");
    fs_err::create_dir_all(dir.as_std_path()).expect("mkdir directory source");

    let out = fx.run(
        &["add", "~/nvim", "--module", "m", "--template", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 1, "--template on a directory must exit 1");
    let stderr = stderr(&out);
    assert!(
        stderr.contains("--template") && stderr.contains("directory"),
        "stderr must name --template and the directory kind, got: {stderr}"
    );
}

/// `--copy` on a directory source writes a `[[directory]]` entry
/// with `mode = "copy"` (a recursive copy), never a `[[file]]`.
#[test]
fn add_copy_on_a_directory_writes_directory_copy() {
    let fx = Fixture::new();
    let dir = fx.home.join("themes");
    fs_err::create_dir_all(dir.as_std_path()).expect("mkdir directory source");
    fs_err::write(dir.join("dark.toml").as_std_path(), "x").expect("seed a leaf");

    let out = fx.run(
        &["add", "~/themes", "--module", "m", "--copy", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 0, "add must exit 0; stderr: {}", stderr(&out));

    let parsed = manifest_value(&fx, "m");
    let entries = parsed
        .get("directory")
        .and_then(toml::Value::as_array)
        .expect("a [[directory]] array");
    let entry = entries.first().expect("the single [[directory]] entry");
    assert_eq!(
        entry.get("mode").and_then(toml::Value::as_str),
        Some("copy")
    );
}

/// Whether `path` is a symbolic link (without following it).
fn is_symlink(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path.as_std_path()).is_ok_and(|m| m.file_type().is_symlink())
}
