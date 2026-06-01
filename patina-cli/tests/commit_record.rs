//! Integration coverage for the widened committed apply record (REQ-029,
//! T-026): the `<state>/patina/journal/<ts>.COMMIT` sentinel records, per
//! target, the canonical source path and — for content targets — a 32-byte
//! `blake3` hash of the materialized bytes, behind a version envelope whose
//! major is now `2`.
//!
//! Each test builds a self-contained tempdir dotfiles repository, points
//! `PATINA_REPO` at it, isolates the per-machine state directory under the
//! tempdir, drives `patina apply --yes` as a subprocess, then decodes the
//! COMMIT record from the isolated journal dir and asserts its provenance.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]
#![expect(
    clippy::panic,
    reason = "integration tests panic! on unexpected fixture/record shapes; allow-*-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]
#![expect(
    clippy::indexing_slicing,
    reason = "the COMMIT envelope and the single-element commit-file vector are indexed only after their length is asserted immediately above; a bounds-check panic is acceptable test signal."
)]

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::ApplyRecord;
use patina_core::ExpectedTarget;
use patina_core::HostOs;
use patina_core::content_hash;
use patina_core::read_latest_commit;
use std::process::Output;
use tempfile::TempDir;

/// A prepared fixture: an isolated repo + state dir + home.
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

    /// Write a module directory with the given `patina.toml` body, returning
    /// its path so the test can drop source files beside the manifest.
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

    /// The per-machine journal directory the subprocess writes COMMIT
    /// sentinels into. The resolved state root is platform-dependent:
    /// Linux/Windows honour `XDG_STATE_HOME` / `LOCALAPPDATA` (→ `self.state`),
    /// while macOS ignores both and uses `$HOME/Library/Application Support`
    /// (→ `self.home`). Resolve it from this fixture's own isolated env values
    /// (the same ones `invoke` passes to the subprocess) so the path matches
    /// wherever the binary actually wrote the journal.
    fn journal_dir(&self) -> Utf8PathBuf {
        patina_core::state_dir::resolve_with_env(HostOs::current(), |name| match name {
            "XDG_STATE_HOME" | "LOCALAPPDATA" => Some(self.state.as_str().to_owned()),
            "HOME" | "USERPROFILE" => Some(self.home.as_str().to_owned()),
            _ => None,
        })
        .expect("resolve fixture state dir")
        .join("journal")
    }

    /// Decode the single COMMIT record produced by the last apply.
    fn commit_record(&self) -> ApplyRecord {
        read_latest_commit(self.journal_dir())
            .expect("read COMMIT record")
            .expect("an apply must have written a COMMIT record")
    }

    /// The raw bytes of the single `<ts>.COMMIT` file in the journal dir.
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

/// The recorded entry whose target path ends with `suffix`.
fn entry_for<'r>(record: &'r ApplyRecord, suffix: &str) -> &'r ExpectedTarget {
    record
        .targets
        .iter()
        .find(|t| t.target().replace('\\', "/").ends_with(suffix))
        .unwrap_or_else(|| panic!("no recorded target ending in `{suffix}`"))
}

/// The recorded blake3 hash of a content target, panicking if the entry is
/// not a `Content` variant. `ExpectedTarget` is `#[non_exhaustive]`, so the
/// match needs a wildcard arm in this downstream crate.
fn content_hash_of(entry: &ExpectedTarget) -> [u8; 32] {
    match entry {
        ExpectedTarget::Content { hash, .. } => *hash,
        _ => panic!("expected a Content target, got {entry:?}"),
    }
}

/// Canonicalize a path the way the engine does before recording it, so the
/// test's expectation matches the recorded source byte-for-byte regardless
/// of the platform's verbatim-prefix representation.
fn canonical(path: &Utf8Path) -> String {
    // `dunce::canonicalize` mirrors the engine's `canonicalize_path`: a
    // filesystem canonicalize with the Windows `\\?\` verbatim prefix
    // stripped where the plain form is equivalent.
    let canon = dunce::canonicalize(path.as_std_path()).expect("canonicalize path");
    camino::Utf8PathBuf::from_path_buf(canon)
        .expect("canonical path is utf8")
        .into_string()
}

// CHK-062: a copy-mode `[[file]]` records, for its target, the canonical
// source path and a 32-byte blake3 hash of the source bytes.
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

// REQ-029 done-when: a symlink target records its canonical link target as
// its source.
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

// CHK-063: two consecutive applies of unchanged source record a
// byte-identical blake3 hash for the content target.
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

// CHK-064: the COMMIT file's first two bytes are the little-endian u16
// major version, now `2`.
#[test]
fn commit_envelope_major_is_two() {
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
    assert_eq!(major, 2, "the COMMIT envelope major version must be 2");
}

// REQ-029 done-when: the read side compares the recorded blake3 — an
// external edit drifts, no edit stays clean.
#[test]
fn status_uses_recorded_blake3_for_drift() {
    let f = Fixture::new();
    let module = f.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("gitconfig"), b"original\n").expect("write source");

    assert_applied(&f.apply(&["--yes"]));

    // No edit: clean.
    let clean = f.status(&["--json"]);
    let clean_doc = status_doc(&clean);
    assert_eq!(
        state_for(&clean_doc, "/.gitconfig"),
        "clean",
        "an unedited content target must be clean"
    );

    // External edit to the materialized bytes: drifted.
    fs_err::write(f.home.join(".gitconfig"), b"edited externally\n").expect("edit target");
    let drifted = f.status(&["--json"]);
    let drifted_doc = status_doc(&drifted);
    assert_eq!(
        state_for(&drifted_doc, "/.gitconfig"),
        "drifted",
        "an externally edited content target must be drifted"
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
