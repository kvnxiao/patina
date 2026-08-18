//! Integration tests for `symlink_tree_orphans`.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "Integration tests use .expect() and panic! for fixtures and asserted output outside #[cfg(test)] modules; allow-*-in-tests does not cover integration-crate roots."
)]

mod common;

use common::Fixture;
use common::code;

fn current_os_family() -> &'static str {
    std::env::consts::OS
}

fn status_json(out: &std::process::Output) -> serde_json::Value {
    assert_eq!(
        code(out),
        0,
        "status must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).expect("status stdout must be a single JSON document")
}

fn state_for(doc: &serde_json::Value, suffix: &str) -> String {
    let files = doc
        .get("files")
        .and_then(serde_json::Value::as_array)
        .expect("files array");
    for entry in files {
        let path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if path.replace('\\', "/").ends_with(suffix) {
            return entry
                .get("state")
                .and_then(serde_json::Value::as_str)
                .expect("state string")
                .to_owned();
        }
    }
    panic!("no files entry ending in `{suffix}` in {doc}");
}

fn respell_target(first: &str, second: &str) -> (common::Fixture, serde_json::Value) {
    let f = Fixture::new();
    let module = f.module("cfg", &copy_entry(first));
    fs_err::write(module.join("conf"), b"body").expect("write source");

    let applied = f.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "the initial apply must succeed; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    f.module("cfg", &copy_entry(second));
    let doc = status_json(&f.run(&["status", "--json"], &[]));

    let reapplied = f.apply(&["--yes"]);
    assert_eq!(
        code(&reapplied),
        0,
        "the respelled apply must succeed; stderr: {}",
        String::from_utf8_lossy(&reapplied.stderr)
    );
    (f, doc)
}

fn copy_entry(target: &str) -> String {
    format!("[[file]]\nsource = \"conf\"\ntarget = \"{target}\"\nmode = \"copy\"\n")
}

#[test]
fn a_case_only_target_respelling_does_not_reap_the_live_target() {
    let (f, doc) = respell_target("~/.Config", "~/.config");
    assert_eq!(
        state_for(&doc, "/.Config"),
        "clean",
        "the recorded target must still read as managed after a case-only respelling: {doc}"
    );
    assert_eq!(
        fs_err::read(f.home.join(".config").as_std_path()).expect("the respelled target exists"),
        b"body",
        "the reap must not delete the object the respelled entry manages"
    );
}

#[test]
fn a_normalization_only_target_respelling_does_not_reap_the_live_target() {
    let (f, doc) = respell_target("~/.caf\u{e9}", "~/.cafe\u{301}");
    assert_eq!(
        state_for(&doc, "/.caf\u{e9}"),
        "clean",
        "the recorded target must still read as managed after a normalization-only \
         respelling: {doc}"
    );
    assert_eq!(
        fs_err::read(f.home.join(".cafe\u{301}").as_std_path())
            .expect("the respelled target exists"),
        b"body",
        "the reap must not delete the object the respelled entry manages"
    );
}

#[test]
fn deleted_symlink_tree_source_leaf_is_reported_orphaned() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[directory]]\nsource = \"d\"\ntarget = \"~/d\"\nmode = \"symlink-tree\"\n",
    );
    let src = module.join("d");
    fs_err::create_dir_all(src.join("sub")).expect("mkdir sub");
    fs_err::write(src.join("a.conf"), b"a").expect("write a");
    fs_err::write(src.join("sub").join("b.conf"), b"b").expect("write b");

    let applied = f.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "the initial symlink-tree apply must succeed; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    fs_err::remove_file(src.join("sub").join("b.conf")).expect("delete source leaf");

    let doc = status_json(&f.run(&["status", "--json"], &[]));
    assert_eq!(
        state_for(&doc, "/d/sub/b.conf"),
        "orphaned",
        "the deleted source leaf's target must classify orphaned: {doc}"
    );
    assert_eq!(
        state_for(&doc, "/d/a.conf"),
        "clean",
        "the surviving leaf must stay clean: {doc}"
    );
}

#[test]
fn next_apply_reaps_orphan_leaf_and_keeps_sibling_and_directory() {
    let f = Fixture::new();
    let module = f.module(
        "cfg",
        "[[directory]]\nsource = \"d\"\ntarget = \"~/d\"\nmode = \"symlink-tree\"\n",
    );
    let src = module.join("d");
    fs_err::create_dir_all(src.join("sub")).expect("mkdir sub");
    fs_err::write(src.join("a.conf"), b"a").expect("write a");
    fs_err::write(src.join("sub").join("b.conf"), b"b").expect("write b");

    let applied = f.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "the initial symlink-tree apply must succeed; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    let d = f.home.join("d");
    let leaf_b = d.join("sub").join("b.conf");
    let leaf_a = d.join("a.conf");
    assert!(
        fs_err::symlink_metadata(leaf_b.as_std_path()).is_ok(),
        "the nested leaf must exist after the initial apply"
    );

    fs_err::remove_file(src.join("sub").join("b.conf")).expect("delete source leaf");
    let reaped = f.apply(&["--yes"]);
    assert_eq!(
        code(&reaped),
        0,
        "the reaping apply must succeed; stderr: {}",
        String::from_utf8_lossy(&reaped.stderr)
    );

    assert!(
        fs_err::symlink_metadata(leaf_b.as_std_path()).is_err(),
        "the orphaned leaf link `~/d/sub/b.conf` must be removed"
    );
    let sub_meta =
        fs_err::symlink_metadata(d.join("sub").as_std_path()).expect("stat intermediate dir");
    assert!(
        sub_meta.file_type().is_dir() && !sub_meta.file_type().is_symlink(),
        "`~/d/sub` must remain a real directory after the leaf is reaped"
    );
    let a_meta = fs_err::symlink_metadata(leaf_a.as_std_path()).expect("stat surviving leaf");
    assert!(
        a_meta.file_type().is_symlink(),
        "the surviving leaf `~/d/a.conf` must still be a symbolic link"
    );
}

#[test]
fn when_flipped_to_false_orphans_then_reaps_target_with_backup() {
    let f = Fixture::new();
    let true_when = format!("patina.os == '{}'", current_os_family());
    let manifest_true = format!(
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\nwhen = \"{true_when}\"\n"
    );
    let module = f.module("git", &manifest_true);
    fs_err::write(module.join("gitconfig"), b"[user]\n  name = me\n").expect("write source");

    let applied = f.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "the `when`-true apply must succeed; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let target = f.home.join(".gitconfig");
    assert!(
        target.exists(),
        "the `when`-true target must be materialized"
    );

    let manifest_false = "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n\
         when = \"patina.os == 'definitely-not-this-os'\"\n";
    fs_err::write(module.join("patina.toml"), manifest_false).expect("rewrite manifest");

    let doc = status_json(&f.run(&["status", "--json"], &[]));
    assert_eq!(
        state_for(&doc, "/.gitconfig"),
        "orphaned",
        "a `when`-flipped-false target must classify orphaned: {doc}"
    );

    let reaped = f.apply(&["--yes"]);
    assert_eq!(
        code(&reaped),
        0,
        "the reaping apply must succeed; stderr: {}",
        String::from_utf8_lossy(&reaped.stderr)
    );
    assert!(
        !target.exists(),
        "the orphaned `~/.gitconfig` must be removed by the reaping apply"
    );

    let state_root = f.state_root();
    let backup = find_backup_with_bytes(&state_root, ".gitconfig", b"[user]\n  name = me\n");
    assert!(
        backup.is_some(),
        "the reaped target's prior bytes must be recorded in a backup under {state_root}"
    );
}

#[test]
fn a_directory_symlink_entry_shields_its_source_from_the_reap() {
    let f = Fixture::new();
    let tree_only =
        "[[directory]]\nsource = \"tree\"\ntarget = \"~/skills\"\nmode = \"symlink-tree\"\n";
    let module = f.module("cfg", tree_only);
    let tree = module.join("tree");
    fs_err::create_dir_all(tree.join("pack")).expect("mkdir pack");
    fs_err::write(tree.join("pack").join("SKILL.md"), b"old").expect("write tree leaf");

    let applied = f.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "the initial symlink-tree apply must succeed; stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    fs_err::remove_dir_all(tree.join("pack").as_std_path()).expect("delete tree leaf");
    let pack = module.join("pack");
    fs_err::create_dir_all(&pack).expect("mkdir pack source");
    fs_err::write(pack.join("SKILL.md"), b"new").expect("write pack source");
    f.module(
        "cfg",
        &format!(
            "{tree_only}\n[[directory]]\nsource = \"pack\"\ntarget = \"~/skills/pack\"\nmode = \"symlink\"\n"
        ),
    );

    let reapplied = f.apply(&["--yes"]);
    assert_eq!(
        code(&reapplied),
        0,
        "the directory-symlink apply must succeed; stderr: {}",
        String::from_utf8_lossy(&reapplied.stderr)
    );

    let link = f.home.join("skills").join("pack");
    let link_meta = fs_err::symlink_metadata(link.as_std_path()).expect("stat the claimed target");
    assert!(
        link_meta.file_type().is_symlink(),
        "`~/skills/pack` must be the directory symlink the new entry declares"
    );
    assert_eq!(
        fs_err::read(pack.join("SKILL.md").as_std_path()).expect("the entry's source survives"),
        b"new",
        "the reap must not follow the directory symlink into the entry's source"
    );
}

fn find_backup_with_bytes(
    root: &camino::Utf8Path,
    file_name: &str,
    want: &[u8],
) -> Option<camino::Utf8PathBuf> {
    let entries = fs_err::read_dir(root.as_std_path()).ok()?;
    for entry in entries.flatten() {
        let path = camino::Utf8PathBuf::from_path_buf(entry.path()).ok()?;
        let meta = fs_err::symlink_metadata(path.as_std_path()).ok()?;
        if meta.is_dir() {
            if let Some(found) = find_backup_with_bytes(&path, file_name, want) {
                return Some(found);
            }
        } else if path.file_name() == Some(file_name)
            && fs_err::read(path.as_std_path()).is_ok_and(|bytes| bytes == want)
        {
            return Some(path);
        }
    }
    None
}
