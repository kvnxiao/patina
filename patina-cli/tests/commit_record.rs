//! Integration tests for `commit_record`.
#![expect(
    clippy::expect_used,
    reason = "Integration tests use .expect() for fixture setup outside #[cfg(test)] modules; allow-expect-in-tests does not cover integration-crate roots."
)]
#![expect(
    clippy::panic,
    reason = "Integration tests use panic! for unexpected fixture or record shapes outside #[cfg(test)] modules; allow-*-in-tests does not cover integration-crate roots."
)]
#![expect(
    clippy::indexing_slicing,
    reason = "The COMMIT envelope and single-element commit-file vector are indexed after length assertions; a bounds-check panic remains a test failure."
)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::ApplyRecord;
use patina_core::ExpectedTarget;
use patina_core::FILE_MAJOR_VERSION;
use patina_core::HostOs;
use patina_core::content_hash;
use patina_core::read_latest_commit;
use std::process::Output;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: Utf8PathBuf,
    home: Utf8PathBuf,
    state: Utf8PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        let repo = root.join("repo");
        let home = root.join("home");
        let state = root.join("state");
        fs_err::create_dir_all(&repo).expect("mkdir repo");
        fs_err::create_dir_all(&home).expect("mkdir home");
        fs_err::create_dir_all(&state).expect("mkdir state");
        fs_err::write(repo.join("patina.toml"), "[patina]\nroot = true\n")
            .expect("write root manifest");
        Self {
            _temp: temp,
            root: repo,
            home,
            state,
        }
    }

    fn module(&self, name: &str, manifest: &str) -> Utf8PathBuf {
        let dir = self.root.join(name);
        fs_err::create_dir_all(&dir).expect("mkdir module");
        fs_err::write(dir.join("patina.toml"), manifest).expect("write module manifest");
        dir
    }

    fn invoke(&self, subcommand: &str, args: &[&str]) -> Output {
        let bin = env!("CARGO_BIN_EXE_patina");
        std::process::Command::new(bin)
            .arg(subcommand)
            .args(args)
            .env("PATINA_REPO", self.root.as_str())
            .env("HOME", self.home.as_str())
            .env("USERPROFILE", self.home.as_str())
            .env("XDG_STATE_HOME", self.state.as_str())
            .env("LOCALAPPDATA", self.state.as_str())
            .env_remove("PATINA_PROFILE")
            .output()
            .expect("spawn patina")
    }

    fn apply(&self, args: &[&str]) -> Output {
        self.invoke("apply", args)
    }

    fn status(&self, args: &[&str]) -> Output {
        self.invoke("status", args)
    }

    fn journal_dir(&self) -> Utf8PathBuf {
        patina_core::state_dir::resolve_with_env(HostOs::current(), |name| match name {
            "XDG_STATE_HOME" | "LOCALAPPDATA" => Some(self.state.as_str().to_owned()),
            "HOME" | "USERPROFILE" => Some(self.home.as_str().to_owned()),
            _ => None,
        })
        .expect("resolve fixture state dir")
        .join("journal")
    }

    fn commit_record(&self) -> ApplyRecord {
        read_latest_commit(self.journal_dir())
            .expect("read COMMIT record")
            .expect("an apply must have written a COMMIT record")
    }

    fn commit_bytes(&self) -> Vec<u8> {
        let mut commits: Vec<Utf8PathBuf> = fs_err::read_dir(self.journal_dir())
            .expect("read journal dir")
            .filter_map(Result::ok)
            .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
            .filter(|p| p.as_str().ends_with(".COMMIT"))
            .collect();
        commits.sort();
        assert_eq!(
            commits.len(),
            1,
            "exactly one COMMIT sentinel must exist, found {commits:?}"
        );
        fs_err::read(&commits[0]).expect("read COMMIT bytes")
    }
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

fn assert_applied(out: &Output) {
    assert_eq!(
        code(out),
        0,
        "apply --yes must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn entry_for<'r>(record: &'r ApplyRecord, suffix: &str) -> &'r ExpectedTarget {
    record
        .targets
        .iter()
        .find(|t| t.target().replace('\\', "/").ends_with(suffix))
        .unwrap_or_else(|| panic!("no recorded target ending in `{suffix}`"))
}

fn content_hash_of(entry: &ExpectedTarget) -> [u8; 32] {
    match entry {
        ExpectedTarget::Content { hash, .. } => *hash,
        _ => panic!("expected a Content target, got {entry:?}"),
    }
}

fn canonical(path: &Utf8Path) -> String {
    let canon = dunce::canonicalize(path.as_std_path()).expect("canonicalize path");
    camino::Utf8PathBuf::from_path_buf(canon)
        .expect("canonical path is utf8")
        .into_string()
}

#[test]
fn copy_target_records_canonical_source_and_blake3_hash() {
    let f = Fixture::new();
    let module = f.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    let source = module.join("gitconfig");
    let contents = b"[user]\n\tname = Ada\n";
    fs_err::write(&source, contents).expect("write source");

    assert_applied(&f.apply(&["--yes"]));

    let record = f.commit_record();
    let entry = entry_for(&record, "/.gitconfig");
    assert!(
        entry.target().replace('\\', "/").ends_with("/.gitconfig"),
        "target resolves to ~/.gitconfig, got {}",
        entry.target()
    );
    assert_eq!(
        entry.source(),
        canonical(&source),
        "recorded source must be the canonical absolute source path"
    );
    assert_eq!(
        content_hash_of(entry),
        content_hash(contents),
        "recorded hash must be the blake3 content hash of the source bytes"
    );
}

#[test]
fn symlink_target_records_link_target_as_source() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"rc\"\ntarget = \"~/.rc\"\nmode = \"symlink\"\n",
    );
    let source = module.join("rc");
    fs_err::write(&source, b"export A=1\n").expect("write source");

    assert_applied(&f.apply(&["--yes"]));

    let record = f.commit_record();
    let entry = entry_for(&record, "/.rc");
    let expected = canonical(&source);
    match entry {
        ExpectedTarget::Symlink { link_target, .. } => {
            assert_eq!(
                entry.source(),
                expected,
                "symlink source accessor must return the canonical link target"
            );
            assert_eq!(
                link_target.replace('\\', "/").trim_start_matches("//?/"),
                expected.replace('\\', "/").trim_start_matches("//?/"),
                "link_target must be the canonical source path"
            );
        }
        _ => panic!("symlink mode must record a Symlink target, got {entry:?}"),
    }
}

#[test]
fn two_applies_record_byte_identical_hash() {
    let f = Fixture::new();
    let module = f.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("gitconfig"), b"stable bytes\n").expect("write source");

    assert_applied(&f.apply(&["--yes"]));
    let first = content_hash_of(entry_for(&f.commit_record(), "/.gitconfig"));

    assert_applied(&f.apply(&["--yes"]));
    let second = content_hash_of(entry_for(&f.commit_record(), "/.gitconfig"));

    assert_eq!(
        first, second,
        "the recorded blake3 hash must be byte-identical across unchanged applies"
    );
}

#[test]
fn commit_envelope_major_matches_supported() {
    let f = Fixture::new();
    let module = f.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("gitconfig"), b"payload\n").expect("write source");

    assert_applied(&f.apply(&["--yes"]));

    let bytes = f.commit_bytes();
    let envelope = bytes.get(..2).expect("COMMIT file has a 2-byte envelope");
    let major = u16::from_le_bytes([envelope[0], envelope[1]]);
    assert_eq!(
        major, FILE_MAJOR_VERSION,
        "the COMMIT envelope major must be the journal's supported major"
    );
}

#[test]
fn status_uses_recorded_blake3_for_drift() {
    let f = Fixture::new();
    let module = f.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("gitconfig"), b"original\n").expect("write source");

    assert_applied(&f.apply(&["--yes"]));

    let clean = f.status(&["--json"]);
    let clean_doc = status_doc(&clean);
    assert_eq!(
        state_for(&clean_doc, "/.gitconfig"),
        "clean",
        "an unedited content target must be clean"
    );

    fs_err::write(f.home.join(".gitconfig"), b"edited externally\n").expect("edit target");
    let drifted = f.status(&["--json"]);
    let drifted_doc = status_doc(&drifted);
    assert_eq!(
        state_for(&drifted_doc, "/.gitconfig"),
        "drifted",
        "an externally edited content target must be drifted"
    );
}

#[test]
fn directory_and_file_entries_get_distinct_indices_and_envelope_major_is_unchanged() {
    let f = Fixture::new();
    let module = f.module(
        "m",
        concat!(
            "[[file]]\nsource = \"a\"\ntarget = \"~/.a\"\nmode = \"copy\"\n",
            "[[file]]\nsource = \"b\"\ntarget = \"~/.b\"\nmode = \"copy\"\n",
            "[[directory]]\nsource = \"d\"\ntarget = \"~/.d\"\nmode = \"copy\"\n",
        ),
    );
    fs_err::write(module.join("a"), b"alpha\n").expect("write file a");
    fs_err::write(module.join("b"), b"bravo\n").expect("write file b");
    let dir = module.join("d");
    fs_err::create_dir_all(&dir).expect("mkdir dir d");
    fs_err::write(dir.join("one"), b"one\n").expect("write leaf one");
    fs_err::write(dir.join("two"), b"two\n").expect("write leaf two");

    assert_applied(&f.apply(&["--yes"]));

    let record = f.commit_record();

    let a = entry_for(&record, "/.a").entry();
    let b = entry_for(&record, "/.b").entry();
    let d_one = entry_for(&record, "/.d/one").entry();
    let d_two = entry_for(&record, "/.d/two").entry();

    assert_ne!(
        a, b,
        "the two `[[file]]` entries must have distinct indices"
    );
    assert_eq!(
        d_one, d_two,
        "the two leaves of one `[[directory]]` entry must share its single entry index"
    );
    assert_ne!(
        a, d_one,
        "a `[[file]]` and a `[[directory]]` entry must not collide on an index"
    );
    assert_ne!(
        b, d_one,
        "a `[[file]]` and a `[[directory]]` entry must not collide on an index"
    );
    assert!(
        a < d_one && b < d_one,
        "every `[[file]]` entry index must precede the `[[directory]]` entry index (got files {a},{b}; dir {d_one})"
    );

    let bytes = f.commit_bytes();
    let envelope = bytes.get(..2).expect("COMMIT file has a 2-byte envelope");
    let major = u16::from_le_bytes([envelope[0], envelope[1]]);
    assert_eq!(
        major, FILE_MAJOR_VERSION,
        "the COMMIT envelope major must stay the supported major; the record layout carries no version bump"
    );
}

fn status_doc(out: &Output) -> serde_json::Value {
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
