//! Integration tests for remove cli.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;

fn applied_symlink_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(
        fx.root.join("zsh").join("zshrc").as_std_path(),
        "shell-config",
    )
    .expect("seed repo source");

    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );
    let zshrc = fx.home.join(".zshrc");
    assert!(
        is_symlink(&zshrc),
        "~/.zshrc must be a symlink after apply; stderr: {}",
        stderr(&applied)
    );
    fx
}

#[test]
fn remove_replaces_target_drops_entry_and_status_omits_it() {
    let fx = applied_symlink_fixture();
    let zshrc = fx.home.join(".zshrc");
    let source = fx.root.join("zsh").join("zshrc");

    let out = fx.run(&["remove", "~/.zshrc", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "remove must exit 0; stderr: {}",
        stderr(&out)
    );

    assert!(!is_symlink(&zshrc), "~/.zshrc must no longer be a symlink");
    assert!(zshrc.is_file(), "~/.zshrc must be a regular file");
    assert_eq!(
        fs_err::read_to_string(zshrc.as_std_path()).expect("read ~/.zshrc"),
        "shell-config"
    );

    let manifest = fx.root.join("zsh").join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    assert!(
        !body.contains("[[file]]"),
        "the [[file]] entry must be removed, got: {body}"
    );

    assert!(source.is_file(), "<repo>/zsh/zshrc must still exist");
    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read repo source"),
        "shell-config",
        "the repository source must be unchanged"
    );

    let status = fx.run(&["status", "--json"], &[]);
    assert_eq!(
        code(&status),
        0,
        "status must exit 0; stderr: {}",
        stderr(&status)
    );
    let stdout = String::from_utf8(status.stdout).expect("utf8 status stdout");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("status is one JSON doc");
    let files = doc
        .get("files")
        .and_then(serde_json::Value::as_array)
        .expect("a files array");
    assert!(
        !files.iter().any(|f| {
            f.get("path")
                .and_then(serde_json::Value::as_str)
                .and_then(|p| Utf8Path::new(p).file_name())
                .is_some_and(|name| name == ".zshrc")
        }),
        "status must not list the removed target, got files: {files:?}"
    );
}

#[test]
fn remove_purge_deletes_target_and_drops_entry() {
    let fx = applied_symlink_fixture();
    let zshrc = fx.home.join(".zshrc");
    let source = fx.root.join("zsh").join("zshrc");

    let out = fx.run(&["remove", "~/.zshrc", "--purge", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "remove --purge must exit 0; stderr: {}",
        stderr(&out)
    );

    assert!(
        fs_err::symlink_metadata(zshrc.as_std_path()).is_err(),
        "~/.zshrc must not exist after --purge"
    );

    let manifest = fx.root.join("zsh").join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    assert!(
        !body.contains("[[file]]"),
        "the [[file]] entry must be removed, got: {body}"
    );
    assert!(source.is_file(), "<repo>/zsh/zshrc must still exist");
    assert_eq!(
        fs_err::read_to_string(source.as_std_path()).expect("read repo source"),
        "shell-config",
        "the repository source must be unchanged"
    );
}

#[test]
fn remove_unmanaged_path_exits_1() {
    let fx = applied_symlink_fixture();
    let bashrc = fx.home.join(".bashrc");
    fs_err::write(bashrc.as_std_path(), "untouched").expect("seed ~/.bashrc");

    let out = fx.run(&["remove", "~/.bashrc", "--yes"], &[]);
    assert_eq!(
        code(&out),
        1,
        "removing an unmanaged path must exit 1; stderr: {}",
        stderr(&out)
    );
    let stderr = stderr(&out);
    assert!(
        stderr.contains("~/.bashrc") && stderr.contains("not managed"),
        "stderr must include the path and say it is not managed, got: {stderr}"
    );

    assert_eq!(
        fs_err::read_to_string(bashrc.as_std_path()).expect("read ~/.bashrc"),
        "untouched",
        "the unmanaged file must be unchanged"
    );
    let manifest = fx.root.join("zsh").join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    assert!(
        body.contains("[[file]]"),
        "the managed entry must survive an unmanaged-path refusal, got: {body}"
    );
}

#[test]
fn remove_json_emits_document() {
    let fx = applied_symlink_fixture();

    let out = fx.run(&["remove", "~/.zshrc", "--json", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "remove --json must exit 0; stderr: {}",
        stderr(&out)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is one JSON doc");
    assert_eq!(
        doc.get("removed").and_then(serde_json::Value::as_str),
        Some("~/.zshrc")
    );
    assert_eq!(
        doc.get("purged").and_then(serde_json::Value::as_bool),
        Some(false)
    );
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn is_symlink(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path.as_std_path()).is_ok_and(|m| m.file_type().is_symlink())
}

#[test]
fn remove_resolves_a_relative_path_against_the_working_directory() {
    let fx = applied_symlink_fixture();
    let zshrc = fx.home.join(".zshrc");

    let out = fx.run_in(&fx.home, &["remove", ".zshrc", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "remove must exit 0; stderr: {}",
        stderr(&out)
    );

    assert!(!is_symlink(&zshrc), "~/.zshrc must no longer be a symlink");
    assert_eq!(
        fs_err::read_to_string(zshrc.as_std_path()).expect("read ~/.zshrc"),
        "shell-config"
    );

    let manifest = fx.root.join("zsh").join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    assert!(
        !body.contains("[[file]]"),
        "the [[file]] entry must be removed, got: {body}"
    );
}

#[test]
fn remove_edits_the_manifest_that_declares_a_nested_source() {
    let fx = Fixture::new();
    fx.module(
        "zsh",
        "[[file]]\nsource = \"conf/zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    let nested = fx.root.join("zsh").join("conf");
    fs_err::create_dir_all(nested.as_std_path()).expect("mkdir nested source dir");
    fs_err::write(nested.join("zshrc").as_std_path(), "shell-config").expect("seed repo source");

    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );

    let out = fx.run(&["remove", "~/.zshrc", "--yes"], &[]);
    assert_eq!(
        code(&out),
        0,
        "remove must exit 0 for a nested source; stderr: {}",
        stderr(&out)
    );

    let body = fs_err::read_to_string(fx.root.join("zsh").join("patina.toml").as_std_path())
        .expect("read module manifest");
    assert!(
        !body.contains("[[file]]"),
        "the declaring manifest must lose the entry, got: {body}"
    );
    assert!(
        !fx.root
            .join("zsh")
            .join("conf")
            .join("patina.toml")
            .exists(),
        "the source's own parent directory must not be treated as a module"
    );
    let zshrc = fx.home.join(".zshrc");
    assert!(zshrc.is_file() && !is_symlink(&zshrc));
    assert_eq!(
        fs_err::read_to_string(zshrc.as_std_path()).expect("read replacement"),
        "shell-config"
    );
}

#[test]
fn remove_refuses_a_tree_leaf_and_leaves_it_a_symlink() {
    let fx = Fixture::new();
    fx.module(
        "cfg",
        "[[directory]]\nsource = \"tree\"\ntarget = \"~/conf\"\nmode = \"symlink-tree\"\n",
    );
    let tree = fx.root.join("cfg").join("tree");
    fs_err::create_dir_all(tree.as_std_path()).expect("mkdir tree source");
    fs_err::write(tree.join("a.conf").as_std_path(), "leaf bytes").expect("seed leaf");

    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );
    let leaf = fx.home.join("conf").join("a.conf");
    assert!(is_symlink(&leaf), "the leaf must be a symlink after apply");

    let out = fx.run(&["remove", "~/conf/a.conf", "--yes"], &[]);

    assert_eq!(
        code(&out),
        1,
        "a tree leaf has no [[file]] entry to drop; stderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("[[directory]]"),
        "the refusal must point at the declaring entry; stderr: {}",
        stderr(&out)
    );
    assert!(
        is_symlink(&leaf),
        "a refused remove must not convert the leaf into a regular file"
    );
    let body = fs_err::read_to_string(fx.root.join("cfg").join("patina.toml").as_std_path())
        .expect("read module manifest");
    assert!(
        body.contains("[[directory]]"),
        "the manifest must be untouched, got: {body}"
    );
}

#[test]
fn a_refused_remove_leaves_the_target_byte_identical_and_still_a_symlink() {
    let fx = applied_symlink_fixture();
    let zshrc = fx.home.join(".zshrc");
    let manifest = fx.root.join("zsh").join("patina.toml");
    fs_err::write(
        manifest.as_std_path(),
        "[[directory]]\nsource = \"dir\"\ntarget = \"~/dir\"\nmode = \"symlink\"\n",
    )
    .expect("rewrite manifest");
    fs_err::create_dir_all(fx.root.join("zsh").join("dir").as_std_path()).expect("mkdir dir");

    let out = fx.run(&["remove", "~/.zshrc", "--yes"], &[]);

    assert_eq!(
        code(&out),
        1,
        "no manifest declares the target any more; stderr: {}",
        stderr(&out)
    );
    assert!(
        is_symlink(&zshrc),
        "a refused remove must leave the target a symlink"
    );
    assert_eq!(
        fs_err::read_to_string(zshrc.as_std_path()).expect("read through the link"),
        "shell-config",
        "a refused remove must leave the target byte-identical"
    );
}

#[test]
fn remove_edits_the_manifest_whose_module_contains_the_journaled_source() {
    let fx = Fixture::new();
    for (module, pick) in [("alpha", "a"), ("beta", "b")] {
        fx.module(
            module,
            &format!(
                "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n\
                 when = \"patina.env.PICK == '{pick}'\"\n"
            ),
        );
        fs_err::write(
            fx.root.join(module).join("zshrc").as_std_path(),
            format!("from {module}"),
        )
        .expect("seed repo source");
    }

    let applied = fx.run(&["apply", "--yes"], &[("PICK", "a")]);
    assert_eq!(
        code(&applied),
        0,
        "the alpha-selecting apply must exit 0; stderr: {}",
        stderr(&applied)
    );

    let out = fx.run(&["remove", "~/.zshrc", "--yes"], &[("PICK", "b")]);
    assert_eq!(
        code(&out),
        0,
        "remove must exit 0; stderr: {}",
        stderr(&out)
    );

    let alpha = fs_err::read_to_string(fx.root.join("alpha").join("patina.toml").as_std_path())
        .expect("read alpha manifest");
    let beta = fs_err::read_to_string(fx.root.join("beta").join("patina.toml").as_std_path())
        .expect("read beta manifest");
    assert!(
        !alpha.contains("[[file]]"),
        "the journaled source is in alpha; alpha's entry must be removed, got: {alpha}"
    );
    assert!(
        beta.contains("[[file]]"),
        "beta declares the same target under a predicate this host does not select; its \
         declaration must survive, got: {beta}"
    );
}
