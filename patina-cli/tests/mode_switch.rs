//! Integration tests for `mode` edits on existing entries: every legal
//! switch converges in one apply, previews honestly, and re-applies as a
//! byte-identical no-op.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixtures and asserted output; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::code;
use common::symlink_file;

fn entry_names(dir: &Utf8Path) -> Vec<String> {
    let Ok(entries) = fs_err::read_dir(dir.as_std_path()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|e| {
            e.expect("read dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn is_symlink(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path.as_std_path())
        .expect("stat path")
        .file_type()
        .is_symlink()
}

fn is_regular_file(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path.as_std_path())
        .expect("stat path")
        .file_type()
        .is_file()
}

fn set_manifest(module_dir: &Utf8Path, manifest: &str) {
    fs_err::write(module_dir.join("patina.toml"), manifest).expect("rewrite module manifest");
}

fn apply_converges(f: &Fixture) {
    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "apply must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Two `apply --json --yes` runs over the converged state: byte-identical
/// stdout, and no new journal or backup entries.
fn assert_noop_reapply(f: &Fixture) {
    let journal_dir = f.state_root().join("journal");
    let backups_dir = f.state_root().join("backups");
    let journal_before = entry_names(&journal_dir);
    let backups_before = entry_names(&backups_dir);

    let first = f.apply(&["--json", "--yes"]);
    assert_eq!(
        code(&first),
        0,
        "the converged --json re-apply must exit 0; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = f.apply(&["--json", "--yes"]);
    assert_eq!(code(&second), 0, "the repeat --json re-apply must exit 0");
    assert_eq!(
        first.stdout,
        second.stdout,
        "two converged --json applies must be byte-identical;\nfirst:  {}\nsecond: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
    );
    assert_eq!(
        entry_names(&journal_dir),
        journal_before,
        "a converged re-apply must add no journal entry"
    );
    assert_eq!(
        entry_names(&backups_dir),
        backups_before,
        "a converged re-apply must add no backup cycle"
    );
}

/// The non-interactive preview's stdout (diff, no writes).
fn preview(f: &Fixture) -> String {
    let out = f.apply(&[]);
    assert_eq!(
        code(&out),
        0,
        "the preview must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const FILE_SYMLINK: &str = "[[file]]\nsource = \"rc\"\ntarget = \"~/rc\"\nmode = \"symlink\"\n";
const FILE_COPY: &str = "[[file]]\nsource = \"rc\"\ntarget = \"~/rc\"\nmode = \"copy\"\n";
const FILE_TEMPLATE: &str = "[[file]]\nsource = \"rc.tmpl\"\ntarget = \"~/rc\"\n";

const DIR_SYMLINK: &str =
    "[[directory]]\nsource = \"conf\"\ntarget = \"~/conf\"\nmode = \"symlink\"\n";
const DIR_SYMLINK_TREE: &str =
    "[[directory]]\nsource = \"conf\"\ntarget = \"~/conf\"\nmode = \"symlink-tree\"\n";
const DIR_COPY: &str = "[[directory]]\nsource = \"conf\"\ntarget = \"~/conf\"\nmode = \"copy\"\n";

/// A module whose `conf/` directory holds `a.conf` and `sub/b.conf`.
fn dir_fixture(manifest: &str) -> (Fixture, Utf8PathBuf) {
    let f = Fixture::new();
    let m = f.module("m", manifest);
    let conf = m.join("conf");
    fs_err::create_dir_all(conf.join("sub")).expect("mkdir conf/sub");
    fs_err::write(conf.join("a.conf"), b"alpha").expect("write a.conf");
    fs_err::write(conf.join("sub").join("b.conf"), b"beta").expect("write b.conf");
    (f, m)
}

fn assert_repo_conf_intact(m: &Utf8Path) {
    assert_eq!(
        fs_err::read(m.join("conf").join("a.conf").as_std_path()).expect("read repo a.conf"),
        b"alpha",
        "the repo source leaf a.conf must survive byte-for-byte"
    );
    assert_eq!(
        fs_err::read(m.join("conf").join("sub").join("b.conf").as_std_path())
            .expect("read repo b.conf"),
        b"beta",
        "the repo source leaf sub/b.conf must survive byte-for-byte"
    );
}

// --- [[file]] matrix ---------------------------------------------------

#[test]
fn file_symlink_to_copy_converges_with_a_replace_block() {
    let f = Fixture::new();
    let m = f.module("m", FILE_SYMLINK);
    fs_err::write(m.join("rc"), b"rc bytes").expect("write rc");
    apply_converges(&f);
    let target = f.home.join("rc");
    assert!(is_symlink(&target), "the first apply materializes a link");

    set_manifest(&m, FILE_COPY);
    let diff = preview(&f);
    assert!(
        diff.contains("replace") && diff.contains("(symlink -> file)"),
        "the mode edit previews as a replace block, got:\n{diff}"
    );
    apply_converges(&f);

    assert!(
        is_regular_file(&target),
        "the target becomes a regular file"
    );
    assert_eq!(
        fs_err::read(target.as_std_path()).expect("read target"),
        b"rc bytes"
    );
    assert_eq!(
        fs_err::read(m.join("rc").as_std_path()).expect("read source"),
        b"rc bytes",
        "the repo source is untouched"
    );
    assert_noop_reapply(&f);
}

#[test]
fn file_copy_to_symlink_converges_with_a_replace_block() {
    let f = Fixture::new();
    let m = f.module("m", FILE_COPY);
    fs_err::write(m.join("rc"), b"rc bytes").expect("write rc");
    apply_converges(&f);
    let target = f.home.join("rc");
    assert!(is_regular_file(&target), "the first apply writes a file");

    set_manifest(&m, FILE_SYMLINK);
    let diff = preview(&f);
    assert!(
        diff.contains("replace") && diff.contains("(file -> symlink)"),
        "the mode edit previews as a replace block, got:\n{diff}"
    );
    apply_converges(&f);

    assert!(is_symlink(&target), "the target becomes a symlink");
    assert_eq!(
        fs_err::read(target.as_std_path()).expect("read through the link"),
        b"rc bytes"
    );
    assert_noop_reapply(&f);
}

#[test]
fn file_copy_to_template_converges() {
    let f = Fixture::new();
    let m = f.module("m", FILE_COPY);
    fs_err::write(m.join("rc"), b"static body").expect("write rc");
    apply_converges(&f);

    fs_err::rename(m.join("rc"), m.join("rc.tmpl")).expect("rename source to .tmpl");
    set_manifest(&m, FILE_TEMPLATE);
    apply_converges(&f);

    let target = f.home.join("rc");
    assert!(is_regular_file(&target));
    assert_eq!(
        fs_err::read(target.as_std_path()).expect("read target"),
        b"static body"
    );
    assert_noop_reapply(&f);
}

#[test]
fn file_template_to_copy_converges() {
    let f = Fixture::new();
    let m = f.module("m", FILE_TEMPLATE);
    fs_err::write(m.join("rc.tmpl"), b"static body").expect("write rc.tmpl");
    apply_converges(&f);

    fs_err::rename(m.join("rc.tmpl"), m.join("rc")).expect("rename source from .tmpl");
    set_manifest(&m, FILE_COPY);
    apply_converges(&f);

    let target = f.home.join("rc");
    assert!(is_regular_file(&target));
    assert_eq!(
        fs_err::read(target.as_std_path()).expect("read target"),
        b"static body"
    );
    assert_noop_reapply(&f);
}

#[test]
fn file_template_to_symlink_converges() {
    let f = Fixture::new();
    let m = f.module("m", FILE_TEMPLATE);
    fs_err::write(m.join("rc.tmpl"), b"static body").expect("write rc.tmpl");
    apply_converges(&f);
    let target = f.home.join("rc");
    assert!(is_regular_file(&target), "the render writes a regular file");

    fs_err::rename(m.join("rc.tmpl"), m.join("rc")).expect("rename source from .tmpl");
    set_manifest(&m, FILE_SYMLINK);
    apply_converges(&f);

    assert!(is_symlink(&target), "the target becomes a symlink");
    assert_eq!(
        fs_err::read(target.as_std_path()).expect("read through the link"),
        b"static body"
    );
    assert_noop_reapply(&f);
}

#[test]
fn file_symlink_to_template_converges_and_writes_nothing_into_the_repo() {
    // The dangling-link repro: renaming the source to `.tmpl` dangles the
    // previously applied link, and the render must land at the target path,
    // never at the dead link's destination inside the repo module.
    let f = Fixture::new();
    let m = f.module("m", FILE_SYMLINK);
    fs_err::write(m.join("rc"), b"static body").expect("write rc");
    apply_converges(&f);
    let target = f.home.join("rc");
    assert!(is_symlink(&target), "the first apply materializes a link");

    fs_err::rename(m.join("rc"), m.join("rc.tmpl")).expect("rename source to .tmpl");
    set_manifest(&m, FILE_TEMPLATE);
    apply_converges(&f);

    assert!(
        is_regular_file(&target),
        "the target becomes a regular file"
    );
    assert_eq!(
        fs_err::read(target.as_std_path()).expect("read target"),
        b"static body"
    );
    assert!(
        !m.join("rc").as_std_path().exists(),
        "nothing may be created at the dead link's destination inside the repo module"
    );
    assert_noop_reapply(&f);
}

#[test]
fn file_copy_over_a_dangling_symlink_writes_the_target() {
    let f = Fixture::new();
    let m = f.module("m", FILE_COPY);
    fs_err::write(m.join("rc"), b"rc bytes").expect("write rc");
    let ghost = f.home.join("nowhere");
    fs_err::write(&ghost, b"x").expect("write ghost");
    let target = f.home.join("rc");
    symlink_file(&ghost, &target);
    fs_err::remove_file(&ghost).expect("dangle the target link");

    apply_converges(&f);

    assert!(
        is_regular_file(&target),
        "the target becomes a regular file"
    );
    assert_eq!(
        fs_err::read(target.as_std_path()).expect("read target"),
        b"rc bytes"
    );
    assert!(
        !ghost.as_std_path().exists(),
        "no file may appear at the dead link's destination"
    );
    assert_noop_reapply(&f);
}

// --- [[directory]] matrix ----------------------------------------------

#[test]
fn dir_symlink_to_symlink_tree_converges_and_preserves_the_repo() {
    // The destruction repro: writing leaves through the stale root link
    // would delete the repo's own files and link them back at themselves.
    let (f, m) = dir_fixture(DIR_SYMLINK);
    apply_converges(&f);
    let target = f.home.join("conf");
    assert!(
        is_symlink(&target),
        "the first apply materializes a dir link"
    );

    set_manifest(&m, DIR_SYMLINK_TREE);
    let diff = preview(&f);
    assert!(
        diff.contains("replace") && diff.contains("(symlink -> tree)"),
        "the mode edit previews as a root replace block, got:\n{diff}"
    );
    assert!(
        !diff.contains(&format!("remove {target}")),
        "the superseded root must not also preview as a remove, got:\n{diff}"
    );
    apply_converges(&f);

    assert!(
        !is_symlink(&target) && target.as_std_path().is_dir(),
        "the root becomes a real directory"
    );
    assert!(is_symlink(&target.join("a.conf")), "leaves are links");
    assert!(is_symlink(&target.join("sub").join("b.conf")));
    assert_repo_conf_intact(&m);

    // The commit record lists every materialized leaf, so status, rollback,
    // and the next reap resolve each leaf independently.
    let record = patina_core::journal::read_latest_commit(f.state_root().join("journal"))
        .expect("read the commit record")
        .expect("a committed apply");
    for leaf in ["a.conf", "b.conf"] {
        assert!(
            record
                .targets
                .iter()
                .any(|t| Utf8Path::new(t.target()).file_name() == Some(leaf)),
            "the commit record must list leaf {leaf}; got {:?}",
            record.targets
        );
    }
    assert_noop_reapply(&f);
}

#[test]
fn dir_symlink_tree_to_symlink_converges() {
    let (f, m) = dir_fixture(DIR_SYMLINK_TREE);
    apply_converges(&f);
    let target = f.home.join("conf");
    assert!(target.as_std_path().is_dir() && !is_symlink(&target));

    set_manifest(&m, DIR_SYMLINK);
    apply_converges(&f);

    assert!(
        is_symlink(&target),
        "the root becomes a whole-directory link"
    );
    assert_eq!(
        fs_err::read(target.join("a.conf").as_std_path()).expect("read through the link"),
        b"alpha"
    );
    assert_repo_conf_intact(&m);
    assert_noop_reapply(&f);
}

#[test]
fn dir_symlink_to_copy_converges_and_preserves_the_repo() {
    let (f, m) = dir_fixture(DIR_SYMLINK);
    apply_converges(&f);
    let target = f.home.join("conf");
    assert!(is_symlink(&target));

    set_manifest(&m, DIR_COPY);
    apply_converges(&f);

    assert!(
        !is_symlink(&target) && target.as_std_path().is_dir(),
        "the root becomes a real directory"
    );
    assert!(is_regular_file(&target.join("a.conf")), "leaves are files");
    assert_eq!(
        fs_err::read(target.join("a.conf").as_std_path()).expect("read leaf"),
        b"alpha"
    );
    assert_eq!(
        fs_err::read(target.join("sub").join("b.conf").as_std_path()).expect("read leaf"),
        b"beta"
    );
    assert_repo_conf_intact(&m);
    assert_noop_reapply(&f);
}

#[test]
fn dir_copy_to_symlink_converges() {
    let (f, m) = dir_fixture(DIR_COPY);
    apply_converges(&f);
    let target = f.home.join("conf");
    assert!(target.as_std_path().is_dir() && !is_symlink(&target));

    set_manifest(&m, DIR_SYMLINK);
    apply_converges(&f);

    assert!(
        is_symlink(&target),
        "the root becomes a whole-directory link"
    );
    assert_eq!(
        fs_err::read(target.join("a.conf").as_std_path()).expect("read through the link"),
        b"alpha"
    );
    assert_repo_conf_intact(&m);
    assert_noop_reapply(&f);
}

#[test]
fn dir_symlink_tree_to_copy_converges() {
    let (f, m) = dir_fixture(DIR_SYMLINK_TREE);
    apply_converges(&f);
    let target = f.home.join("conf");
    assert!(is_symlink(&target.join("a.conf")), "leaves start as links");

    set_manifest(&m, DIR_COPY);
    apply_converges(&f);

    assert!(
        is_regular_file(&target.join("a.conf")),
        "leaves become regular files"
    );
    assert!(is_regular_file(&target.join("sub").join("b.conf")));
    assert_eq!(
        fs_err::read(target.join("a.conf").as_std_path()).expect("read leaf"),
        b"alpha"
    );
    assert_repo_conf_intact(&m);
    assert_noop_reapply(&f);
}

#[test]
fn dir_copy_to_symlink_tree_converges() {
    let (f, m) = dir_fixture(DIR_COPY);
    apply_converges(&f);
    let target = f.home.join("conf");
    assert!(
        is_regular_file(&target.join("a.conf")),
        "leaves start as files"
    );

    set_manifest(&m, DIR_SYMLINK_TREE);
    apply_converges(&f);

    assert!(is_symlink(&target.join("a.conf")), "leaves become links");
    assert!(is_symlink(&target.join("sub").join("b.conf")));
    assert_eq!(
        fs_err::read(target.join("a.conf").as_std_path()).expect("read through the leaf link"),
        b"alpha"
    );
    assert_repo_conf_intact(&m);
    assert_noop_reapply(&f);
}

// --- transfers: a deleted entry's target claimed by another entry -------

#[test]
fn file_target_transfer_renders_the_plain_verb_and_no_remove() {
    let f = Fixture::new();
    let m = f.module(
        "m",
        "[[file]]\nsource = \"a_src\"\ntarget = \"~/t\"\nmode = \"symlink\"\n",
    );
    fs_err::write(m.join("a_src"), b"a bytes").expect("write a_src");
    fs_err::write(m.join("b_src"), b"b bytes").expect("write b_src");
    apply_converges(&f);
    let target = f.home.join("t");
    assert!(is_symlink(&target));

    // Entry A deleted; entry B (a different source) claims the target.
    set_manifest(
        &m,
        "[[file]]\nsource = \"b_src\"\ntarget = \"~/t\"\nmode = \"copy\"\n",
    );
    let diff = preview(&f);
    assert!(
        diff.contains(&format!("copy {target}")) && !diff.contains("replace"),
        "a transfer keeps the plain mode verb, got:\n{diff}"
    );
    assert!(
        diff.contains("(symlink ->"),
        "the kind-aware body shows the link being replaced, got:\n{diff}"
    );
    assert!(
        !diff.contains(&format!("remove {target}")),
        "the claimed target must not preview as a remove, got:\n{diff}"
    );
    apply_converges(&f);

    assert!(is_regular_file(&target));
    assert_eq!(
        fs_err::read(target.as_std_path()).expect("read target"),
        b"b bytes"
    );
    assert_noop_reapply(&f);
}

#[test]
fn file_target_transfer_to_symlink_renders_the_plain_verb_and_no_remove() {
    let f = Fixture::new();
    let m = f.module(
        "m",
        "[[file]]\nsource = \"a_src\"\ntarget = \"~/t\"\nmode = \"copy\"\n",
    );
    fs_err::write(m.join("a_src"), b"a bytes").expect("write a_src");
    fs_err::write(m.join("b_src"), b"b bytes").expect("write b_src");
    apply_converges(&f);
    let target = f.home.join("t");
    assert!(is_regular_file(&target));

    set_manifest(
        &m,
        "[[file]]\nsource = \"b_src\"\ntarget = \"~/t\"\nmode = \"symlink\"\n",
    );
    let diff = preview(&f);
    assert!(
        diff.contains(&format!("symlink {target}")) && !diff.contains("replace"),
        "a transfer keeps the plain mode verb, got:\n{diff}"
    );
    assert!(
        diff.contains("(text, 7 bytes)"),
        "the kind-aware body describes the live file instead of (absent), got:\n{diff}"
    );
    assert!(
        !diff.contains(&format!("remove {target}")),
        "the claimed target must not preview as a remove, got:\n{diff}"
    );
    apply_converges(&f);

    assert!(is_symlink(&target));
    assert_eq!(
        fs_err::read(target.as_std_path()).expect("read through the link"),
        b"b bytes"
    );
    assert_noop_reapply(&f);
}

#[test]
fn dir_root_transfer_keeps_the_mode_verb_and_no_remove() {
    let f = Fixture::new();
    let m = f.module(
        "m",
        "[[directory]]\nsource = \"conf_a\"\ntarget = \"~/conf\"\nmode = \"symlink\"\n",
    );
    for (dir, leaf, bytes) in [
        ("conf_a", "a.conf", b"alpha"),
        ("conf_b", "b.conf", b"bravo"),
    ] {
        let d = m.join(dir);
        fs_err::create_dir_all(&d).expect("mkdir source dir");
        fs_err::write(d.join(leaf), bytes).expect("write source leaf");
    }
    apply_converges(&f);
    let target = f.home.join("conf");
    assert!(is_symlink(&target));

    // Entry A deleted; entry B claims the directory with a different source.
    set_manifest(
        &m,
        "[[directory]]\nsource = \"conf_b\"\ntarget = \"~/conf\"\nmode = \"symlink-tree\"\n",
    );
    let diff = preview(&f);
    assert!(
        diff.contains(&format!("symlink {target}")) && !diff.contains("replace"),
        "a transferred root keeps the plain mode verb, got:\n{diff}"
    );
    assert!(
        diff.contains("(tree, 1 file)"),
        "the body shows the incoming leaf count, got:\n{diff}"
    );
    assert!(
        !diff.contains(&format!("remove {target}")),
        "the claimed root must not preview as a remove, got:\n{diff}"
    );
    apply_converges(&f);

    assert!(!is_symlink(&target) && target.as_std_path().is_dir());
    assert!(is_symlink(&target.join("b.conf")));
    assert_eq!(
        fs_err::read(m.join("conf_a").join("a.conf").as_std_path()).expect("read repo a.conf"),
        b"alpha",
        "the old entry's repo source survives"
    );
    assert_noop_reapply(&f);
}

// --- rollback of a consented root replacement ---------------------------

#[test]
fn rollback_of_a_root_replacement_restores_the_link_and_the_repo() {
    let (f, m) = dir_fixture(DIR_SYMLINK);
    apply_converges(&f);
    let target = f.home.join("conf");
    assert!(is_symlink(&target));

    set_manifest(&m, DIR_SYMLINK_TREE);
    apply_converges(&f);
    assert!(!is_symlink(&target) && target.as_std_path().is_dir());

    let rollback = f.run(&["rollback", "--yes"], &[]);
    assert_eq!(
        code(&rollback),
        0,
        "rollback must succeed; stderr: {}",
        String::from_utf8_lossy(&rollback.stderr)
    );

    assert!(
        is_symlink(&target),
        "the root reverts to the pre-apply whole-directory link"
    );
    assert_eq!(
        fs_err::read(target.join("a.conf").as_std_path()).expect("read through the restored link"),
        b"alpha"
    );
    assert_repo_conf_intact(&m);
}
