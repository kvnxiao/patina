#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use .expect()/panic! on fixtures and asserted output; allow-*-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! The status managed-set is `when`-aware and
//! expands `symlink-tree` entries per leaf, so a dropped target is classified
//! ORPHANED and reaped on the next apply.
//!
//! Each test drives `PATINA_REPO=<tempdir> patina apply --yes`, perturbs the
//! repository (deletes a `symlink-tree` source leaf, or flips a `[[file]]`
//! entry's `when` to false), then asserts:
//!
//! - `patina status` classifies the now-unmanaged target ORPHANED;
//! - the next `patina apply --yes` removes the orphan leaf link while its
//!   surviving sibling leaf and the intermediate directory stay in place;
//! - a reaped `[[file]]` target's prior bytes were backed up — provable by
//!   finding the original bytes in the reaping run's backup tree.

mod common;

use common::Fixture;
use common::code;

/// The OS family string the engine's `patina.os` built-in resolves to on
/// this host (`"macos"`, `"linux"`, or `"windows"`). `std::env::consts::OS`
/// is exactly the value the engine's `normalized_os` returns on the three
/// supported platforms, so a `when` built from it is deterministically true
/// here (matching `conditional_entries.rs`).
fn current_os_family() -> &'static str {
    std::env::consts::OS
}

/// Parse a `patina status --json` document, asserting a clean exit.
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

/// The classification of the `files[]` entry whose path ends with `suffix`.
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

#[test]
fn deleted_symlink_tree_source_leaf_is_reported_orphaned() {
    // An applied `symlink-tree` whose source contained `sub/b.conf`,
    // with that source leaf then deleted, makes `patina status` classify
    // `~/d/sub/b.conf` as orphaned — the managed set walks the *live* source
    // and the deleted leaf is no longer in it.
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

    // Delete the nested source leaf; its materialized target leaf is now an
    // orphan of a removed source file.
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
    // With the same deleted-source-leaf state, `patina apply --yes`
    // removes `~/d/sub/b.conf`, leaves `~/d/sub` as a real directory, and
    // leaves the surviving `~/d/a.conf` a symbolic link.
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

    // Delete the source leaf, then re-apply: the orphan leaf link is reaped.
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
    // The intermediate directory is never removed, even though it
    // is now empty.
    let sub_meta =
        fs_err::symlink_metadata(d.join("sub").as_std_path()).expect("stat intermediate dir");
    assert!(
        sub_meta.file_type().is_dir() && !sub_meta.file_type().is_symlink(),
        "`~/d/sub` must remain a real directory after the leaf is reaped"
    );
    // The surviving leaf stays a symbolic link.
    let a_meta = fs_err::symlink_metadata(leaf_a.as_std_path()).expect("stat surviving leaf");
    assert!(
        a_meta.file_type().is_symlink(),
        "the surviving leaf `~/d/a.conf` must still be a symbolic link"
    );
}

#[test]
fn when_flipped_to_false_orphans_then_reaps_target_with_backup() {
    // A `[[file]]` entry with a true `when` whose target was
    // materialized, then its `when` edited to a predicate false on this host,
    // is classified orphaned by `patina status`, and the next
    // `patina apply --yes` removes the target after recording its prior bytes
    // in a backup (proven by finding those bytes in the reaping run's backup
    // tree).
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

    // Flip the entry's `when` to a predicate false on this host by rewriting
    // the module manifest (a user-repo edit).
    let manifest_false = "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n\
         when = \"patina.os == 'definitely-not-this-os'\"\n";
    fs_err::write(module.join("patina.toml"), manifest_false).expect("rewrite manifest");

    // Status classifies the now-unmanaged target orphaned.
    let doc = status_json(&f.run(&["status", "--json"], &[]));
    assert_eq!(
        state_for(&doc, "/.gitconfig"),
        "orphaned",
        "a `when`-flipped-false target must classify orphaned: {doc}"
    );

    // The next apply reaps it.
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

    // The prior bytes were recorded in a backup before removal: the reaping
    // run's backup tree holds a `.gitconfig` whose bytes are the original
    // target's. Searching the backup tree (rather than poking at a specific
    // `<ts>` directory) proves the never-overwrite-without-backup guarantee
    // held for the reap without coupling the test to the timestamp layout.
    // The backup tree lives under the *resolved* state root, which differs
    // per platform — `XDG_STATE_HOME`/`LOCALAPPDATA` on Linux/Windows but
    // `$HOME/Library/Application Support/patina` on macOS — so search
    // `f.state_root()` (the per-platform resolver) rather than the raw
    // `f.state` env value, which only backs the state dir on Linux/Windows.
    let state_root = f.state_root();
    let backup = find_backup_with_bytes(&state_root, ".gitconfig", b"[user]\n  name = me\n");
    assert!(
        backup.is_some(),
        "the reaped target's prior bytes must be recorded in a backup under {state_root}"
    );
}

/// Recursively search `root` for a regular file named `file_name` whose
/// bytes equal `want`, returning its path. Used to prove the reap stashed a
/// target's prior bytes into the backup tree without depending on the
/// per-cycle `<ts>` directory name.
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
