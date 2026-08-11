#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! `patina apply` over a remote-backed module, end to end.
//!
//! Each test builds a throwaway origin repository with the real `git` binary
//! inside the fixture tempdir, hand-writes the `patina.lock` a producer machine
//! would have committed, and drives the CLI as a subprocess. Nothing touches
//! the network: the "remote" is a local filesystem path.
//!
//! See `docs/REMOTE_SOURCES.md` "Remote-backed modules", "The remote cache",
//! and "Trust boundaries".

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::code;
use std::process::Command;

/// A fixed committer epoch, so nothing in these fixtures depends on the clock.
const EPOCH: i64 = 1_700_000_000;

/// Run `git` in `cwd` with a pinned identity and date, independent of the
/// developer's global git config.
fn git_in(cwd: &Utf8Path, args: &[&str]) -> String {
    let date = format!("{EPOCH} +0000");
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd.as_std_path())
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// An origin repository living outside the dotfiles repo, so module discovery
/// never sees it.
struct Origin {
    dir: Utf8PathBuf,
}

impl Origin {
    fn new(f: &Fixture, name: &str) -> Self {
        let dir = f.home.join(".origins").join(name);
        fs_err::create_dir_all(dir.as_std_path()).expect("mkdir origin");
        git_in(&dir, &["init", "--quiet", "-b", "main"]);
        Self { dir }
    }

    /// The origin path spelled so it can be embedded in a TOML basic string: on
    /// Windows a native path's backslashes would read as escape sequences. Git
    /// accepts the forward-slash form of a Windows path.
    fn url(&self) -> String {
        self.dir.as_str().replace('\\', "/")
    }

    /// Write `files` into the origin and commit them, returning the commit SHA.
    fn commit(&self, files: &[(&str, &str)]) -> String {
        for (path, body) in files {
            let full = self.dir.join(path);
            if let Some(parent) = full.parent() {
                fs_err::create_dir_all(parent.as_std_path()).expect("mkdir origin subdir");
            }
            fs_err::write(full.as_std_path(), body).expect("write origin file");
        }
        git_in(&self.dir, &["add", "-A"]);
        git_in(&self.dir, &["commit", "--quiet", "-m", "fixture"]);
        git_in(&self.dir, &["rev-parse", "HEAD"])
    }
}

/// Write the `patina.lock` a producer machine would have committed.
fn write_lock(f: &Fixture, module: &str, origin: &Origin, rev: &str) {
    let body = format!(
        "version = 1\n\n[remotes.{module}]\nurl = \"{}\"\nref = \"main\"\nrev = \"{rev}\"\n\
         updated_at = \"2026-08-11T14:00:00Z\"\n",
        origin.url()
    );
    fs_err::write(f.root.join("patina.lock").as_std_path(), body).expect("write patina.lock");
}

/// Block until the wall clock crosses into the next second.
///
/// Used only where a test needs two applies to land in distinct journal cycles:
/// the engine keys those by a one-second-resolution timestamp, so two applies
/// inside one second collapse onto a single `<ts>.COMMIT`.
fn wait_for_next_second() {
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    };
    let start = now();
    while now() == start {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The checkout directory the engine resolves a remote module's sources
/// against.
fn checkout(f: &Fixture, module: &str, rev: &str) -> Utf8PathBuf {
    patina_core::remote::cache::checkout_dir(&f.state_root(), module, rev)
}

#[test]
fn a_remote_copy_mode_directory_materializes_from_the_pinned_checkout() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer");
    let rev = origin.commit(&[
        ("skills/humanizer/SKILL.md", "humanize\n"),
        ("README.md", "unrelated\n"),
    ]);
    f.module(
        "humanizer",
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n\n\
             [[directory]]\nsource = \"skills/humanizer\"\n\
             target = \"~/.claude/skills/humanizer\"\nmode = \"copy\"\n",
            origin.url()
        ),
    );
    write_lock(&f, "humanizer", &origin, &rev);

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "apply must converge to the committed pin; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(
            f.home
                .join(".claude/skills/humanizer/SKILL.md")
                .as_std_path()
        )
        .expect("the deployed leaf is readable"),
        "humanize\n"
    );
    assert!(
        !f.home.join(".claude/skills/humanizer/README.md").exists(),
        "only the declared subtree may be deployed; the rest of the remote stays in the cache"
    );
}

#[test]
fn a_remote_symlink_entry_points_into_the_pinned_checkout() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "prompts");
    let rev = origin.commit(&[("prompts/agent.md", "be brief\n")]);
    f.module(
        "prompts",
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n\n\
             [[file]]\nsource = \"prompts/agent.md\"\ntarget = \"~/.agent.md\"\n",
            origin.url()
        ),
    );
    write_lock(&f, "prompts", &origin, &rev);

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "apply must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let deployed = f.home.join(".agent.md");
    assert_eq!(
        fs_err::read_to_string(deployed.as_std_path()).expect("read through the link"),
        "be brief\n"
    );
    let link = fs_err::read_link(deployed.as_std_path()).expect("the target is a symbolic link");
    let link = Utf8PathBuf::from_path_buf(link).expect("utf8 link target");
    assert!(
        link.starts_with(dunce::simplified(
            checkout(&f, "prompts", &rev).as_std_path()
        )),
        "the link must point into the pinned checkout, got {link}"
    );
}

#[test]
fn a_patina_toml_inside_the_checkout_contributes_nothing() {
    // Remote content is third-party input: mappings, hooks, and variables come
    // only from manifests in the user's own repository. A hostile manifest in the
    // checkout must be inert bytes.
    let f = Fixture::new();
    let origin = Origin::new(&f, "hostile");
    let rev = origin.commit(&[
        ("payload/note.md", "harmless\n"),
        (
            "patina.toml",
            "[patina]\nroot = true\n\n[[file]]\nsource = \"payload/note.md\"\n\
             target = \"~/.ssh/authorized_keys\"\nmode = \"copy\"\n\n\
             [[hook]]\nevent = \"post_apply\"\ncommand = \"touch pwned\"\n",
        ),
    ]);
    f.module(
        "hostile",
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n\n\
             [[file]]\nsource = \"payload/note.md\"\ntarget = \"~/.note.md\"\nmode = \"copy\"\n",
            origin.url()
        ),
    );
    write_lock(&f, "hostile", &origin, &rev);

    let out = f.apply(&["--json", "--yes"]);
    assert_eq!(
        code(&out),
        0,
        "the hostile manifest must not break the apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The checkout really does contain the hostile manifest; otherwise this
    // test would pass for the wrong reason.
    assert!(
        checkout(&f, "hostile", &rev).join("patina.toml").is_file(),
        "the fixture must actually place a patina.toml inside the checkout"
    );
    assert!(
        !f.home.join(".ssh/authorized_keys").exists(),
        "an entry declared inside the checkout must contribute no operation"
    );
    assert!(
        !f.root.join("pwned").exists() && !f.home.join("pwned").exists(),
        "a hook declared inside the checkout must never run"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("one JSON document");
    let plan = doc
        .get("plan")
        .and_then(serde_json::Value::as_array)
        .expect("a plan array");
    assert_eq!(
        plan.len(),
        1,
        "only the entry declared in the user's own manifest may plan: {plan:?}"
    );
}

#[test]
fn a_remote_module_with_no_lock_entry_fails_planning_and_names_the_command() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer");
    origin.commit(&[("a.md", "a\n")]);
    f.module(
        "humanizer",
        &format!(
            "[remote]\nurl = \"{}\"\n\n\
             [[file]]\nsource = \"a.md\"\ntarget = \"~/.a.md\"\nmode = \"copy\"\n",
            origin.url()
        ),
    );

    let out = f.apply(&["--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        code(&out),
        1,
        "an unpinned remote must fail; stderr: {stderr}"
    );
    assert!(
        stderr.contains("patina remote update humanizer"),
        "the message must name the command that creates the first pin; stderr: {stderr}"
    );
    assert!(!f.home.join(".a.md").exists(), "nothing may be applied");
}

#[test]
fn a_cold_cache_with_an_unreachable_remote_fails_naming_the_rev() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "gone");
    let rev = origin.commit(&[("a.md", "a\n")]);
    f.module(
        "gone",
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n\n\
             [[file]]\nsource = \"a.md\"\ntarget = \"~/.a.md\"\nmode = \"copy\"\n",
            origin.url()
        ),
    );
    write_lock(&f, "gone", &origin, &rev);
    // Simulating offline: the remote is unreachable and nothing is cached yet.
    fs_err::remove_dir_all(origin.dir.as_std_path()).expect("delete the origin");

    let out = f.apply(&["--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        code(&out),
        1,
        "a cold cache with an unreachable remote must fail; stderr: {stderr}"
    );
    assert!(
        stderr.contains(&rev) && stderr.contains("gone"),
        "the error must name the remote and the missing rev; stderr: {stderr}"
    );
    assert!(!f.home.join(".a.md").exists(), "nothing may be applied");
}

#[test]
fn a_warm_cache_applies_fully_with_the_remote_unreachable() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "warm");
    let rev = origin.commit(&[("a.md", "a\n")]);
    f.module(
        "warm",
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n\n\
             [[file]]\nsource = \"a.md\"\ntarget = \"~/.a.md\"\nmode = \"copy\"\n",
            origin.url()
        ),
    );
    write_lock(&f, "warm", &origin, &rev);
    assert_eq!(code(&f.apply(&["--yes"])), 0, "priming apply");

    // Now go offline. The checkout is already materialized, so the whole apply
    // must still work.
    fs_err::remove_dir_all(origin.dir.as_std_path()).expect("delete the origin");
    fs_err::remove_file(f.home.join(".a.md").as_std_path()).expect("delete the deployed file");

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "a warm cache must not need the remote; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".a.md").as_std_path()).expect("redeployed"),
        "a\n"
    );
}

#[test]
fn a_checkout_holds_the_commit_bytes_even_under_autocrlf() {
    // `core.autocrlf = true` is a common Windows setting. If it reached the
    // checkout, the same pinned commit would deploy CRLF on one machine and LF
    // on another and hash differently in the journal, so a checkout must hold
    // the commit's bytes verbatim.
    let f = Fixture::new();
    let origin = Origin::new(&f, "crlf");
    let rev = origin.commit(&[("a.md", "one\ntwo\n")]);
    f.module(
        "crlf",
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n\n\
             [[file]]\nsource = \"a.md\"\ntarget = \"~/.a.md\"\nmode = \"copy\"\n",
            origin.url()
        ),
    );
    write_lock(&f, "crlf", &origin, &rev);

    let hostile_config = f.home.join("hostile.gitconfig");
    fs_err::write(
        hostile_config.as_std_path(),
        "[core]\n\tautocrlf = true\n\teol = crlf\n",
    )
    .expect("write a hostile global git config");

    let out = f.apply_with_env(
        &["--yes"],
        &[("GIT_CONFIG_GLOBAL", hostile_config.as_str())],
    );
    assert_eq!(
        code(&out),
        0,
        "apply must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let deployed =
        fs_err::read(f.home.join(".a.md").as_std_path()).expect("the deployed file is readable");
    assert!(
        !deployed.contains(&b'\r'),
        "the deployed bytes must match the commit, but carry CR: {deployed:?}"
    );
}

#[test]
fn re_applying_an_unchanged_pin_is_a_byte_identical_no_op() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "stable");
    let rev = origin.commit(&[("a.md", "a\n")]);
    f.module(
        "stable",
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n\n\
             [[file]]\nsource = \"a.md\"\ntarget = \"~/.a.md\"\nmode = \"copy\"\n",
            origin.url()
        ),
    );
    write_lock(&f, "stable", &origin, &rev);
    assert_eq!(code(&f.apply(&["--yes"])), 0, "priming apply");

    let first = f.apply(&["--yes"]);
    let second = f.apply(&["--yes"]);
    assert_eq!(code(&first), 0);
    assert_eq!(code(&second), 0);
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
        "two applies over an unchanged pin must produce identical stdout"
    );
}

#[test]
fn bumping_the_pin_re_points_the_link_and_rollback_restores_the_prior_checkout() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "moving");
    let first_rev = origin.commit(&[("a.md", "first\n")]);
    f.module(
        "moving",
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n\n\
             [[file]]\nsource = \"a.md\"\ntarget = \"~/.a.md\"\n",
            origin.url()
        ),
    );
    write_lock(&f, "moving", &origin, &first_rev);
    assert_eq!(code(&f.apply(&["--yes"])), 0, "apply the first pin");

    // Journal files are keyed by a one-second-resolution timestamp, so two
    // applies inside the same second share a `<ts>.COMMIT` and the earlier
    // record is overwritten, which would leave the prior checkout unreferenced
    // and swept, and rollback with a dangling link. Cross a second boundary so
    // the two applies get distinct journal cycles.
    wait_for_next_second();

    let second_rev = origin.commit(&[("a.md", "second\n")]);
    write_lock(&f, "moving", &origin, &second_rev);
    let bumped = f.apply(&["--yes"]);
    assert_eq!(
        code(&bumped),
        0,
        "applying the bumped pin must succeed; stderr: {}",
        String::from_utf8_lossy(&bumped.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".a.md").as_std_path()).expect("read through the link"),
        "second\n",
        "the link must now resolve into the new checkout"
    );

    let rolled_back = f.run(&["rollback", "--yes"], &[]);
    assert_eq!(
        code(&rolled_back),
        0,
        "rollback must succeed; stderr: {}",
        String::from_utf8_lossy(&rolled_back.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".a.md").as_std_path()).expect("read through the link"),
        "first\n",
        "rollback must re-point the link at the prior checkout, which must still exist"
    );
}

#[test]
fn apply_prunes_a_checkout_no_journal_record_references() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "sweep");
    let rev = origin.commit(&[("a.md", "a\n")]);
    f.module(
        "sweep",
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n\n\
             [[file]]\nsource = \"a.md\"\ntarget = \"~/.a.md\"\nmode = \"copy\"\n",
            origin.url()
        ),
    );
    write_lock(&f, "sweep", &origin, &rev);
    assert_eq!(code(&f.apply(&["--yes"])), 0, "priming apply");

    // A checkout of a rev nothing was ever applied from: no journal record can
    // name it, so the post-apply sweep must remove it.
    let orphan = checkout(&f, "sweep", "cccccccccccccccccccccccccccccccccccccccc");
    fs_err::create_dir_all(orphan.as_std_path()).expect("mkdir orphan checkout");
    fs_err::write(orphan.join("a.md").as_std_path(), b"stale").expect("write orphan leaf");

    // Force a non-no-op apply so the run reaches the post-commit sweep.
    fs_err::remove_file(f.home.join(".a.md").as_std_path()).expect("delete the deployed file");
    assert_eq!(code(&f.apply(&["--yes"])), 0, "second apply");

    assert!(!orphan.exists(), "the unreferenced checkout must be swept");
    assert!(
        checkout(&f, "sweep", &rev).is_dir(),
        "the checkout the committed record names must survive"
    );
}
