#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup and assertions; allow-expect-in-tests covers #[cfg(test)] modules but not the top level of a tests/*.rs integration crate."
)]

//! Integration coverage for `patina add`.
//!
//! Each test spawns the real `patina` binary against an isolated tempdir
//! repo + state + home (via the shared [`common::Fixture`]). Because the
//! binary is invoked as a subprocess, its stdin is not a TTY, so the
//! non-interactive paths (missing-flag refusals, no prompts) are the ones
//! under test here; the TTY prompt branches are unit-tested in `cmd::add`.

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::code;
use std::io::BufRead;
use std::io::BufReader;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::Once;
use std::time::Duration;
use std::time::Instant;

/// `patina add ~/.zshrc --module zsh --symlink --yes` moves the
/// dotfile into `<repo>/zsh/zshrc`, writes a `[[file]]` entry, and leaves
/// the original target as a regular file with the original bytes (apply has
/// not run).
#[test]
fn add_moves_file_writes_entry_and_leaves_target() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(
        &["add", "~/.zshrc", "--module", "zsh", "--symlink", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 0, "add must exit 0; stderr: {}", stderr(&out));

    // The moved file is a regular file with the original content.
    let moved = fx.root.join("zsh").join("zshrc");
    assert!(moved.is_file(), "<repo>/zsh/zshrc must be a regular file");
    assert!(
        !is_symlink(&moved),
        "<repo>/zsh/zshrc must not be a symlink"
    );
    assert_eq!(
        fs_err::read_to_string(moved.as_std_path()).expect("read moved file"),
        "foo"
    );

    // The module manifest carries the entry with source/target/mode.
    let manifest = fx.root.join("zsh").join("patina.toml");
    let body = fs_err::read_to_string(manifest.as_std_path()).expect("read module manifest");
    let parsed: toml::Value = toml::from_str(&body).expect("module manifest parses");
    let entries = parsed
        .get("file")
        .and_then(toml::Value::as_array)
        .expect("a [[file]] array");
    assert_eq!(entries.len(), 1, "exactly one [[file]] entry");
    let entry = entries.first().expect("the single [[file]] entry");
    assert_eq!(
        entry.get("source").and_then(toml::Value::as_str),
        Some("zshrc")
    );
    assert_eq!(
        entry.get("target").and_then(toml::Value::as_str),
        Some("~/.zshrc")
    );
    assert_eq!(
        entry.get("mode").and_then(toml::Value::as_str),
        Some("symlink")
    );

    // The original target is still a regular file with the original bytes:
    // add does NOT materialize, so apply has not yet run.
    assert!(zshrc.is_file(), "~/.zshrc must remain a regular file");
    assert!(!is_symlink(&zshrc), "~/.zshrc must not be a symlink yet");
    assert_eq!(
        fs_err::read_to_string(zshrc.as_std_path()).expect("read ~/.zshrc"),
        "foo"
    );
}

/// Given the post-state of the prior add (the file staged into the repo
/// and the `[[file]]` entry written, target still a regular file), running
/// `patina apply --yes` materializes the target as a symbolic link whose
/// readlink destination is the canonical path of `<repo>/zsh/zshrc`. This is
/// the convergence half: it proves `add` wrote a correct, *applyable*
/// entry, which the manifest text + not-yet-a-symlink check cannot.
#[test]
fn add_then_apply_materializes_target_as_symlink() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    // Post-state of the prior add: stage the file and write the entry.
    let add = fx.run(
        &["add", "~/.zshrc", "--module", "zsh", "--symlink", "--yes"],
        &[],
    );
    assert_eq!(code(&add), 0, "add must exit 0; stderr: {}", stderr(&add));
    assert!(
        !is_symlink(&zshrc),
        "~/.zshrc must not be a symlink before apply"
    );

    // Convergence: apply materializes the staged entry.
    let applied = fx.apply(&["--yes"]);
    assert_eq!(
        code(&applied),
        0,
        "apply must exit 0; stderr: {}",
        stderr(&applied)
    );

    // The target is now a symlink pointing at the canonical staged source.
    assert!(
        is_symlink(&zshrc),
        "~/.zshrc must be a symbolic link after apply"
    );
    let link_target = fs_err::read_link(zshrc.as_std_path()).expect("read_link ~/.zshrc");
    let staged = fx.root.join("zsh").join("zshrc");
    let canonical = fs_err::canonicalize(staged.as_std_path()).expect("canonicalize staged source");
    assert_eq!(
        fs_err::canonicalize(&link_target).expect("canonicalize link target"),
        canonical,
        "the symlink must resolve to the canonical <repo>/zsh/zshrc"
    );
}

/// Two mode flags produce a clap usage error (exit 2)
/// and stderr names the conflicting flags.
#[test]
fn add_two_mode_flags_is_a_usage_error() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(&["add", "~/.zshrc", "--symlink", "--copy"], &[]);
    assert_eq!(
        code(&out),
        2,
        "two mode flags must be a clap usage error (exit 2)"
    );
    let stderr = stderr(&out);
    assert!(
        stderr.contains("--symlink") && stderr.contains("--copy"),
        "stderr must name the conflicting flags, got: {stderr}"
    );
}

/// In a non-TTY shell without `--module`, `add` exits 1 and names
/// the missing `--module` flag.
#[test]
fn add_non_tty_without_module_exits_1() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(&["add", "~/.zshrc", "--symlink", "--yes"], &[]);
    assert_eq!(code(&out), 1, "non-TTY add without --module must exit 1");
    assert!(
        stderr(&out).contains("--module"),
        "stderr must name the missing --module flag, got: {}",
        stderr(&out)
    );

    // No move occurred: the original is untouched and no module was created.
    assert!(zshrc.is_file(), "~/.zshrc must be untouched on refusal");
    assert!(
        !fx.root.join("zsh").exists(),
        "no module directory should be created on refusal"
    );
}

/// A path that is already managed exits 1 and names the owning
/// module.
#[test]
fn add_already_managed_path_exits_1() {
    let fx = Fixture::new();
    // Seed an existing module that already manages ~/.zshrc.
    fx.module(
        "zsh",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n",
    );
    fs_err::write(fx.root.join("zsh").join("zshrc").as_std_path(), "old").expect("seed source");
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(
        &["add", "~/.zshrc", "--module", "other", "--copy", "--yes"],
        &[],
    );
    assert_eq!(code(&out), 1, "adding an already-managed path must exit 1");
    let stderr = stderr(&out);
    assert!(
        stderr.contains("already managed") && stderr.contains("zsh"),
        "stderr must say the path is already managed and name the module, got: {stderr}"
    );
    // The original target is untouched.
    assert!(zshrc.is_file(), "~/.zshrc must be untouched on refusal");
}

/// `add --json --yes` emits a single deterministic JSON document
/// on stdout naming the added target, module, and mode; stderr carries no
/// prose.
#[test]
fn add_json_emits_deterministic_document() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    let out = fx.run(
        &[
            "add",
            "~/.zshrc",
            "--module",
            "zsh",
            "--symlink",
            "--json",
            "--yes",
        ],
        &[],
    );
    assert_eq!(
        code(&out),
        0,
        "add --json must exit 0; stderr: {}",
        stderr(&out)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is one JSON doc");
    assert_eq!(
        doc.get("added").and_then(serde_json::Value::as_str),
        Some("~/.zshrc")
    );
    assert_eq!(
        doc.get("module").and_then(serde_json::Value::as_str),
        Some("zsh")
    );
    assert_eq!(
        doc.get("mode").and_then(serde_json::Value::as_str),
        Some("symlink")
    );
}

/// A process holding the engine's exclusive lock blocks a
/// concurrent `patina add` until it is released; `add` then completes
/// successfully.
///
/// Process A is the `patina-core` `lock_helper` example, which acquires the
/// exclusive lock at `<state>/lock` (the same path `patina add` resolves
/// from the fixture env) and holds it for a fixed window. Process B is
/// `patina add`, launched once A is observed to hold the lock. B must not
/// finish before A releases — its completion proves it waited on the lock.
#[test]
fn add_serializes_behind_a_held_exclusive_lock() {
    let fx = Fixture::new();
    let zshrc = fx.home.join(".zshrc");
    fs_err::write(zshrc.as_std_path(), "foo").expect("seed ~/.zshrc");

    // The helper locks `<state_dir>/lock` directly; the CLI resolves its
    // state dir from the fixture env, so point the helper at that same
    // resolved directory.
    let state_dir = fx.state_root();
    fs_err::create_dir_all(state_dir.join("journal").as_std_path()).expect("mkdir journal");

    let helper = lock_helper_path();
    ensure_lock_helper_built();

    let hold = Duration::from_secs(2);
    let mut holder = spawn_holder(&helper, &state_dir, hold);
    wait_for_acquired(&mut holder);
    let released_after = Instant::now();

    // Launch the contender. Give it a generous lock timeout so it waits out
    // the holder rather than timing out.
    let started = Instant::now();
    let out = fx.run(
        &["add", "~/.zshrc", "--module", "zsh", "--symlink", "--yes"],
        &[("PATINA_LOCK_TIMEOUT_MS", "30000")],
    );
    let waited = started.elapsed();

    holder.wait().expect("holder exits");

    assert_eq!(
        code(&out),
        0,
        "add must complete once the lock frees; stderr: {}",
        stderr(&out)
    );
    // B must have blocked for most of the hold window: it cannot have
    // completed before A released the lock. Allow slack for process
    // startup, but require it waited a clear majority of the hold.
    assert!(
        waited >= Duration::from_secs(1),
        "add should have blocked on the held lock (waited {waited:?})"
    );
    assert!(
        released_after.elapsed() >= Duration::from_secs(1),
        "the holder should have been holding the lock while add waited"
    );

    // The move still happened correctly after the wait.
    let moved = fx.root.join("zsh").join("zshrc");
    assert!(
        moved.is_file(),
        "the moved source must exist after the wait"
    );
}

/// Decode stderr to a lossless string for assertions.
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Whether `path` is a symbolic link (without following it).
fn is_symlink(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path.as_std_path()).is_ok_and(|m| m.file_type().is_symlink())
}

static BUILD: Once = Once::new();

/// Build the `patina-core` `lock_helper` example once into this test
/// binary's target root so [`lock_helper_path`] can locate it under both
/// plain `cargo test` and `cargo llvm-cov`.
fn ensure_lock_helper_built() {
    BUILD.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--quiet",
                "--package",
                "patina-core",
                "--example",
                "lock_helper",
            ])
            .arg("--target-dir")
            .arg(target_root().as_str())
            .status()
            .expect("spawn cargo build for lock_helper example");
        assert!(status.success(), "building lock_helper example failed");
    });
}

/// The cargo target root this test binary was built into, derived from the
/// running test executable: `<target-root>/<profile>/deps/<test-exe>`.
fn target_root() -> Utf8PathBuf {
    let test_exe = std::env::current_exe().expect("current test exe path");
    let root = test_exe
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .expect("derive target root from test exe path");
    Utf8PathBuf::from_path_buf(root.to_owned()).expect("utf8 target root")
}

/// Locate the compiled `lock_helper` example next to this test binary.
fn lock_helper_path() -> Utf8PathBuf {
    let test_exe = std::env::current_exe().expect("current test exe path");
    let deps_dir = test_exe.parent().expect("deps dir");
    let profile_dir = deps_dir.parent().expect("profile dir");
    let mut helper = profile_dir.join("examples").join("lock_helper");
    if cfg!(windows) {
        helper.set_extension("exe");
    }
    Utf8PathBuf::from_path_buf(helper).expect("utf8 helper path")
}

/// Spawn the lock holder: acquire the exclusive lock at `<state>/lock` and
/// hold it for `hold`, with a long acquisition timeout (it acquires first,
/// so it never blocks).
fn spawn_holder(helper: &Utf8Path, state: &Utf8Path, hold: Duration) -> Child {
    Command::new(helper.as_std_path())
        .arg(state.as_str())
        .arg("exclusive")
        .arg(hold.as_millis().to_string())
        .arg("30000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lock_helper holder")
}

/// Block until the holder prints its `ACQUIRED` marker, proving it holds the
/// lock, then drain the rest of its stdout in the background so it never
/// blocks on a later write.
fn wait_for_acquired(child: &mut Child) {
    let stdout = child.stdout.take().expect("holder stdout piped");
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read holder stdout");
        assert!(read != 0, "holder exited before printing ACQUIRED marker");
        if line.starts_with("ACQUIRED ") {
            break;
        }
    }
    std::thread::spawn(move || {
        let _drained = std::io::copy(&mut reader, &mut std::io::sink());
    });
}
