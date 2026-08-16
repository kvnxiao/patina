#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use .expect()/panic! on fixtures and asserted output; allow-*-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Orphan classification and reaping for `symlink-tree` leaves and
//! `when`-gated entries.
//!
//! The status managed-set is `when`-aware and expands `symlink-tree` entries
//! per leaf, so a dropped target is classified ORPHANED and reaped on the next
//! apply.
//!
//! Each test drives `PATINA_REPO=<tempdir> patina apply --yes`, perturbs the
//! repository (deletes a `symlink-tree` source leaf, or flips a `[[file]]`
//! entry's `when` to false), then asserts:
//!
//! - `patina status` classifies the now-unmanaged target ORPHANED;
//! - the next `patina apply --yes` removes the orphan leaf link while its
//!   surviving sibling leaf and the intermediate directory stay in place;
//! - a reaped `[[file]]` target's prior bytes were backed up, provable by
//!   finding the original bytes in the reaping run's backup tree;
//! - a target respelled only in case or Unicode normal form is the same target,
//!   so the reap leaves it alone rather than deleting what the respelled entry
//!   just materialized;
//! - a recorded leaf whose directory a whole-directory `symlink` entry has
//!   since claimed is not reaped through that link. Reaping through it would
//!   delete the entry's source rather than a stale target.

mod common;

use common::Fixture;
use common::code;

/// The OS family string the engine's `patina.os` built-in resolves to on
/// this host (`"macos"`, `"linux"`, or `"windows"`). `std::env::consts::OS`
/// equals the value the engine's `normalized_os` returns on the three
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

/// Apply a `copy` entry at `first`, respell its target to `second`, and apply
/// again. Returns the fixture and the `patina status --json` taken between the
/// two applies.
///
/// The two spellings differ only in case or Unicode normal form. A
/// case-insensitive (or normalizing) filesystem resolves them to one object.
/// Recording the first spelling and re-deriving the second must therefore yield
/// one managed key, or the reap deletes what the second apply just wrote.
///
/// The status document is the host-independent half of the proof. Classifying
/// the recorded target only compares managed keys, so it reports ORPHANED
/// under an unfolded key on every filesystem. The surviving bytes prove the
/// outcome only where the two spellings name one object.
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

/// A one-entry module manifest copying `conf` to `target`.
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
    // `é` precomposed against `e` plus a combining acute. APFS resolves the two
    // to one file; elsewhere they are two files, and only the status
    // classification can fail.
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
    // An applied `symlink-tree` whose source contained `sub/b.conf`, with
    // that source leaf then deleted, makes `patina status` classify
    // `~/d/sub/b.conf` as orphaned. The managed set walks the live source,
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

    // Delete the source leaf, then re-apply. The orphan leaf link is reaped.
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
    let a_meta = fs_err::symlink_metadata(leaf_a.as_std_path()).expect("stat surviving leaf");
    assert!(
        a_meta.file_type().is_symlink(),
        "the surviving leaf `~/d/a.conf` must still be a symbolic link"
    );
}

#[test]
fn when_flipped_to_false_orphans_then_reaps_target_with_backup() {
    // A `[[file]]` entry with a true `when` whose target was materialized,
    // then its `when` edited to a predicate false on this host, is
    // classified orphaned by `patina status`. The next `patina apply --yes`
    // removes the target after recording its prior bytes in a backup, proven
    // by finding those bytes in the reaping run's backup tree.
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

    // The prior bytes were recorded in a backup before removal. The reaping
    // run's backup tree holds a `.gitconfig` whose bytes are the original
    // target's. Searching the backup tree, rather than a specific `<ts>`
    // directory, proves the never-overwrite-without-backup guarantee held for
    // the reap without coupling the test to the timestamp layout.
    // The backup tree lives under the *resolved* state root. That root
    // differs per platform (`XDG_STATE_HOME` / `LOCALAPPDATA` on
    // Linux/Windows, `$HOME/Library/Application Support/patina` on macOS), so
    // the test searches `f.state_root()`, the per-platform resolver. The raw
    // `f.state` env value only backs the state dir on Linux/Windows.
    let state_root = f.state_root();
    let backup = find_backup_with_bytes(&state_root, ".gitconfig", b"[user]\n  name = me\n");
    assert!(
        backup.is_some(),
        "the reaped target's prior bytes must be recorded in a backup under {state_root}"
    );
}

#[test]
fn a_directory_symlink_entry_shields_its_source_from_the_reap() {
    // A `symlink-tree` entry materialized `~/skills/pack/SKILL.md`. The source
    // leaf is then deleted and a whole-directory `symlink` entry claims
    // `~/skills/pack`, so the recorded leaf path now resolves through the new
    // link into that entry's source. Reaping it would delete the source file.
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

    // Hand `~/skills/pack` to a whole-directory `symlink` entry backed by a
    // different source, and drop the tree leaf that used to occupy it.
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
