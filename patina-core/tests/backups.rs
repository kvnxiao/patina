#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration coverage for backup-on-overwrite (T-012 / REQ-014).
//!
//! The end-to-end `patina apply --yes` surface CHK-025 names cannot run
//! yet: the `apply` subcommand, the executor loop that calls the backup
//! writer, and the symlink executor land in later tasks (T-014, T-016).
//! These tests drive the `patina_core::backups::backup_before_overwrite`
//! entry point directly — the layer T-012 owns — by staging the on-disk
//! state the SPEC scenarios describe (a pre-existing target, an absent
//! target, a clean repository) and asserting the backup tree converges to
//! the REQ-014 shape. Each test maps to one REQ-014 `<done-when>` bullet:
//!
//! - CHK-025: overwriting a pre-existing `~/.zshrc` produces a backup holding
//!   the original bytes at the mirrored path before the overwrite.
//! - "fresh target produces no backup entry": an absent target yields no backup
//!   file.
//! - "the repo is never written during apply": backups land under the state
//!   tree, leaving a sibling repository directory byte-for-byte untouched.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::backups::backup_before_overwrite;
use patina_core::journal::mirror_backup_path;
use tempfile::TempDir;

const TS: &str = "20260528T120000Z";

/// A staged apply scene: a state directory with a `backups/` tree, a
/// `home/` standing in for the user's targets, and a `repo/` standing in
/// for the dotfiles repository the engine must never write to.
struct Scene {
    _temp: TempDir,
    backups: Utf8PathBuf,
    home: Utf8PathBuf,
    repo: Utf8PathBuf,
}

impl Scene {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let backups = root.join("state").join("patina").join("backups");
        let home = root.join("home");
        let repo = root.join("repo");
        for d in [&backups, &home, &repo] {
            fs_err::create_dir_all(d).expect("create scene dir");
        }
        Self {
            _temp: temp,
            backups,
            home,
            repo,
        }
    }
}

/// Snapshot every regular file under `dir` as (relative-path, bytes) pairs,
/// so a later snapshot can prove the tree is byte-for-byte unchanged.
fn snapshot(dir: &Utf8Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(cur) = stack.pop() {
        for entry in fs_err::read_dir(&cur).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = Utf8PathBuf::from_path_buf(entry.path()).expect("utf8 path");
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(dir).expect("path under dir").to_string();
                out.push((rel, fs_err::read(&path).expect("read file")));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn overwriting_a_pre_existing_target_stashes_the_original_bytes() {
    // CHK-025: a pre-existing `~/.zshrc` with "original" is backed up before
    // the engine would replace it with a symlink.
    let scene = Scene::new();
    let zshrc = scene.home.join(".zshrc");
    fs_err::write(&zshrc, b"original").expect("seed ~/.zshrc");

    let made = backup_before_overwrite(&scene.backups, TS, &zshrc).expect("back up the target");

    assert!(
        made,
        "an existing target must report that a backup was made"
    );
    // The backup lives where recovery would read it back, holding the
    // pre-overwrite bytes verbatim.
    let backup = mirror_backup_path(&scene.backups, TS, &zshrc);
    assert!(backup.is_file(), "the backup must be a regular file");
    assert_eq!(
        fs_err::read(&backup).expect("read backup"),
        b"original",
        "the backup must contain the pre-overwrite bytes"
    );
    // The backup nests under the per-apply <ts> root and keeps the target's
    // own file name.
    assert!(backup.starts_with(scene.backups.join(TS)));
    assert_eq!(backup.file_name(), Some(".zshrc"));
}

#[test]
fn a_freshly_created_target_produces_no_backup_entry() {
    // REQ-014 done-when: a target that does not pre-exist (a template render
    // to a new `~/.gitconfig`) yields no backup entry.
    let scene = Scene::new();
    let gitconfig = scene.home.join(".gitconfig");

    let made =
        backup_before_overwrite(&scene.backups, TS, &gitconfig).expect("no-op on absent target");

    assert!(!made, "an absent target must report no backup was made");
    let backup = mirror_backup_path(&scene.backups, TS, &gitconfig);
    assert!(
        !backup.exists(),
        "no backup entry may exist for a target that did not pre-exist"
    );
}

#[test]
fn backups_never_write_into_the_dotfiles_repository() {
    // REQ-014 done-when: after backups run, the dotfiles repository is
    // byte-for-byte unchanged — the engine writes only under the state tree.
    let scene = Scene::new();
    // A repository whose source files the engine reads but must not mutate.
    fs_err::write(scene.repo.join("zshrc"), b"managed source").expect("seed repo source");
    fs_err::create_dir_all(scene.repo.join("nested")).expect("nested dir");
    fs_err::write(scene.repo.join("nested").join("gitconfig"), b"tmpl")
        .expect("seed nested source");
    let before = snapshot(&scene.repo);

    // Back up two pre-existing user targets under home/.
    let zshrc = scene.home.join(".zshrc");
    let bashrc = scene.home.join(".bashrc");
    fs_err::write(&zshrc, b"a").expect("seed");
    fs_err::write(&bashrc, b"b").expect("seed");
    backup_before_overwrite(&scene.backups, TS, &zshrc).expect("backup zshrc");
    backup_before_overwrite(&scene.backups, TS, &bashrc).expect("backup bashrc");

    let after = snapshot(&scene.repo);
    assert_eq!(
        before, after,
        "the dotfiles repository must be byte-for-byte unchanged after backups run"
    );
    // And the backups did land — under the state tree, not the repo.
    assert!(mirror_backup_path(&scene.backups, TS, &zshrc).is_file());
    assert!(mirror_backup_path(&scene.backups, TS, &bashrc).is_file());
}
