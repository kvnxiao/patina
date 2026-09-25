//! Integration tests for rollback of reaped targets and of targets edited
//! after the apply.

#![cfg(test)]

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::stderr;
use patina_core::journal::COMMIT_SUFFIX;

const BOTH: &str = "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n\n\
                    [[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n";

const ONLY_A: &str = "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n";

fn canonical_home(fx: &Fixture) -> Utf8PathBuf {
    let canon = dunce::canonicalize(fx.home.as_std_path()).expect("canonicalize the home");
    Utf8PathBuf::from_path_buf(canon).expect("canonical home is utf8")
}

fn kept_copies(stderr: &str) -> Vec<(Utf8PathBuf, Utf8PathBuf)> {
    stderr
        .lines()
        .filter_map(|line| {
            let rest = line.split_once("kept a copy of ")?.1;
            let (target, copy) = rest.split_once(" from before the rollback at ")?;
            Some((Utf8PathBuf::from(target), Utf8PathBuf::from(copy.trim())))
        })
        .collect()
}

fn reaped_b(b_source: &str) -> Fixture {
    let fx = Fixture::new();
    let module = fx.module("shell", BOTH);
    fs_err::write(module.join("a"), "A\n").expect("write source a");
    fs_err::write(module.join("b"), b_source).expect("write source b");
    fx.run_ok(&["apply", "--yes"]);
    fx.module("shell", ONLY_A);
    fx.run_ok(&["apply", "--yes"]);
    assert!(
        fs_err::symlink_metadata(fx.home.join(".b")).is_err(),
        "the second apply must reap ~/.b"
    );
    fx
}

#[test]
fn rollback_restores_a_reaped_file_with_its_pre_reap_bytes() {
    let fx = reaped_b("B\n");

    let out = fx.run_ok(&["rollback", "--yes"]);

    let b = fx.home.join(".b");
    let meta = fs_err::symlink_metadata(&b).expect("stat the restored target");
    assert!(meta.file_type().is_file(), "~/.b must be a regular file");
    assert_eq!(fs_err::read_to_string(&b).expect("read ~/.b"), "B\n");
    assert!(
        kept_copies(&stderr(&out)).is_empty(),
        "nothing lived at the reaped path, so nothing is kept; stderr: {}",
        stderr(&out)
    );
}

#[test]
fn rollback_restores_a_reaped_symlink_as_the_same_link() {
    let fx = Fixture::new();
    let module = fx.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    let source = module.join("zshrc");
    fs_err::write(&source, "export Z=1\n").expect("write source");
    fx.run_ok(&["apply", "--yes"]);
    let zshrc = fx.home.join(".zshrc");
    let applied_link = fs_err::read_link(&zshrc).expect("read the applied link");
    fx.module("zsh", "");
    fx.run_ok(&["apply", "--yes"]);
    assert!(
        fs_err::symlink_metadata(&zshrc).is_err(),
        "the second apply must reap ~/.zshrc"
    );

    fx.run_ok(&["rollback", "--yes"]);

    let meta = fs_err::symlink_metadata(&zshrc).expect("stat the restored target");
    assert!(meta.file_type().is_symlink(), "~/.zshrc must be a symlink");
    assert_eq!(
        fs_err::read_link(&zshrc).expect("read the restored link"),
        applied_link
    );
}

#[test]
fn rollback_keeps_a_file_recreated_at_a_reaped_path_and_names_the_copy() {
    let fx = reaped_b("B\n");
    let b = fx.home.join(".b");
    fs_err::write(&b, "USER-B\n").expect("recreate ~/.b outside patina");

    let out = fx.run_ok(&["rollback", "--yes"]);

    assert_eq!(fs_err::read_to_string(&b).expect("read ~/.b"), "B\n");
    let copies = kept_copies(&stderr(&out));
    let [(target, copy)] = copies.as_slice() else {
        panic!("expected one kept copy; stderr: {}", stderr(&out));
    };
    assert_eq!(target, &canonical_home(&fx).join(".b"));
    assert_eq!(
        fs_err::read_to_string(copy).expect("read the kept copy"),
        "USER-B\n"
    );
    assert!(
        copy.components()
            .any(|component| component.as_str() == patina_core::journal::RECOVERED_DIR),
        "the copy is under the state directory's recovered/: {copy}"
    );
}

#[test]
fn rollback_keeps_edited_targets_and_does_not_copy_an_unedited_one() {
    let fx = Fixture::new();
    let module = fx.module(
        "shell",
        &format!("{BOTH}\n[[file]]\nsource = \"c\"\ntarget = \"~/.c\"\nmode = \"copy\"\n"),
    );
    for name in ["a", "b", "c"] {
        fs_err::write(module.join(name), format!("NEW-{name}\n")).expect("write a source");
    }
    let (a, b, c) = (fx.home.join(".a"), fx.home.join(".b"), fx.home.join(".c"));
    fs_err::write(&b, "OLD-b\n").expect("seed a pre-existing ~/.b");
    fx.run_ok(&["apply", "--yes"]);
    fs_err::write(&a, "EDITED-a\n").expect("edit the created ~/.a");
    fs_err::write(&b, "EDITED-b\n").expect("edit the updated ~/.b");

    let out = fx.run_ok(&["rollback", "--yes"]);

    assert!(
        fs_err::symlink_metadata(&a).is_err(),
        "~/.a was created, so rollback deletes it"
    );
    assert_eq!(fs_err::read_to_string(&b).expect("read ~/.b"), "OLD-b\n");
    assert!(
        fs_err::symlink_metadata(&c).is_err(),
        "~/.c was created, so rollback deletes it"
    );
    let home = canonical_home(&fx);
    let mut kept: Vec<(Utf8PathBuf, String)> = kept_copies(&stderr(&out))
        .into_iter()
        .map(|(target, copy)| {
            let bytes = fs_err::read_to_string(&copy).expect("read a kept copy");
            (target, bytes)
        })
        .collect();
    kept.sort();
    assert_eq!(
        kept,
        [
            (home.join(".a"), "EDITED-a\n".to_owned()),
            (home.join(".b"), "EDITED-b\n".to_owned())
        ],
        "only the edited targets are kept; stderr: {}",
        stderr(&out)
    );
}

#[test]
fn debug_journal_lists_the_targets_the_reap_removed() {
    let fx = reaped_b("B\n");
    let journal = fx.state_root().join("journal");
    let latest = newest_commit(&journal);

    let out = fx.run_ok(&["debug", "journal", latest.as_str()]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let reaped = stdout
        .split_once("reaped:\n")
        .map(|(_, rest)| rest)
        .expect("the record lists reaped targets");
    let b = fx.home.join(".b");
    assert!(
        reaped
            .lines()
            .any(|line| Utf8Path::new(line.trim()).file_name() == b.file_name()),
        "the reaped list names ~/.b: {stdout}"
    );
}

fn newest_commit(journal: &Utf8Path) -> Utf8PathBuf {
    let mut commits: Vec<Utf8PathBuf> = fs_err::read_dir(journal)
        .expect("read the journal")
        .map(|entry| {
            Utf8PathBuf::from_path_buf(entry.expect("read a journal entry").path())
                .expect("utf8 journal path")
        })
        .filter(|path| path.as_str().ends_with(COMMIT_SUFFIX))
        .collect();
    commits.sort();
    commits.pop().expect("a committed apply")
}
