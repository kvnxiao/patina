#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! `patina apply` over entries sourced from a declared remote, end to end.
//!
//! Each test declares the remote in the root manifest, builds a throwaway
//! origin repository with the real `git` binary inside the fixture tempdir,
//! hand-writes the `patina.lock` a producer machine would have committed, and
//! drives the CLI as a subprocess. The "remote" is a local filesystem path, so
//! the network is never involved.
//!
//! See `docs/REMOTE_SOURCES.md` "The remote registry", "The remote cache", and
//! "Trust boundaries".

mod common;

use camino::Utf8PathBuf;
use common::Fixture;
use common::Origin;
use common::code;

/// A fixed committer epoch, so nothing in these fixtures depends on the clock.
const EPOCH: i64 = 1_700_000_000;

/// Write the `patina.lock` a producer machine would have committed.
fn write_lock(f: &Fixture, name: &str, origin: &Origin, rev: &str) {
    let body = format!(
        "version = 1\n\n[remotes.{name}]\nurl = \"{}\"\nref = \"main\"\nrev = \"{rev}\"\n\
         updated_at = \"2026-08-11T14:00:00Z\"\n",
        origin.url()
    );
    fs_err::write(f.root.join("patina.lock").as_std_path(), body).expect("write patina.lock");
}

/// Declare `origin` in the root manifest under `name`, tracking `main`.
fn declare(f: &Fixture, name: &str, origin: &Origin) {
    f.declare_remote(name, &origin.url(), Some("main"));
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

/// The checkout directory the engine resolves a remote-sourced entry against.
fn checkout(f: &Fixture, name: &str, rev: &str) -> Utf8PathBuf {
    patina_core::remote::cache::checkout_dir(&f.state_root(), &remote_name(name), rev)
}

/// A validated remote name, for the cache paths the assertions read.
fn remote_name(spelling: &str) -> patina_core::RemoteName {
    patina_core::RemoteName::parse(spelling).expect("a legal remote name")
}

#[test]
fn a_remote_copy_mode_directory_materializes_from_the_pinned_checkout() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", EPOCH);
    let rev = origin.commit_files(
        &[
            ("skills/humanizer/SKILL.md", "humanize\n"),
            ("README.md", "unrelated\n"),
        ],
        EPOCH,
    );
    declare(&f, "humanizer", &origin);
    f.module(
        "agents",
        "[[directory]]\nsource = \"skills/humanizer\"\nremote = \"humanizer\"\n\
         target = \"~/.claude/skills/humanizer\"\nmode = \"copy\"\n",
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
fn a_checkout_holding_a_real_symlink_fails_the_apply_plan() {
    // Patina materializes checkouts with `core.symlinks=false`, so a real link
    // in the cache means it was made or altered by something else. The plan
    // must refuse to deploy through it: the executors dereference links, which
    // would read (copy) or plant (symlink-tree) paths outside the checkout.
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", EPOCH);
    let rev = origin.commit_files(&[("skills/humanizer/SKILL.md", "humanize\n")], EPOCH);
    declare(&f, "humanizer", &origin);
    f.module(
        "agents",
        "[[directory]]\nsource = \"skills/humanizer\"\nremote = \"humanizer\"\n\
         target = \"~/.claude/skills/humanizer\"\nmode = \"copy\"\n",
    );
    write_lock(&f, "humanizer", &origin, &rev);

    // Fabricate the checkout the way an attacker (not Patina) would leave it:
    // a plain leaf plus a link pointing at a file outside the checkout.
    let source_dir = checkout(&f, "humanizer", &rev).join("skills/humanizer");
    fs_err::create_dir_all(source_dir.as_std_path()).expect("mkdir fabricated checkout");
    fs_err::write(source_dir.join("SKILL.md").as_std_path(), "humanize\n")
        .expect("write plain leaf");
    let outside = f.home.join("secret");
    fs_err::write(outside.as_std_path(), "key material\n").expect("write outside file");
    common::symlink_file(&outside, &source_dir.join("creds"));

    let out = f.apply(&["--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(code(&out), 0, "the plan must refuse; stderr: {stderr}");
    assert!(
        stderr.contains("symbolic link") && stderr.contains("humanizer"),
        "the error must say what was found and where: {stderr}"
    );
    assert!(
        !f.home.join(".claude/skills/humanizer/creds").exists(),
        "nothing may be deployed from a checkout that failed the plan"
    );
}

#[test]
fn status_reports_applied_leaves_clean_when_the_pin_moved_but_its_checkout_is_absent() {
    // The multi-machine flow: a `git pull` bumps the pin before the next
    // apply. Status must not fetch, so the new rev's leaves are unknowable, and
    // the previously applied, still-correct leaves must read CLEAN with a
    // warning, not ORPHANED.
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", EPOCH);
    let rev = origin.commit_files(&[("skills/humanizer/SKILL.md", "humanize\n")], EPOCH);
    declare(&f, "humanizer", &origin);
    f.module(
        "agents",
        "[[directory]]\nsource = \"skills/humanizer\"\nremote = \"humanizer\"\n\
         target = \"~/.claude/skills/humanizer\"\nmode = \"copy\"\n",
    );
    write_lock(&f, "humanizer", &origin, &rev);
    assert_eq!(code(&f.apply(&["--yes"])), 0, "priming apply");

    // The pulled lockfile pins a rev this machine has not materialized.
    write_lock(&f, "humanizer", &origin, &"c".repeat(40));

    let out = f.run(&["status"], &[]);
    assert_eq!(
        code(&out),
        0,
        "status must not fail over an absent checkout"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("orphaned: 0"),
        "an unmaterialized checkout must not orphan applied leaves: {stdout}"
    );
    assert!(
        stdout.contains("clean") && stdout.contains("SKILL.md"),
        "the applied leaf must still be reported: {stdout}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("humanizer"),
        "status must warn which remote it could not assess: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_entry_naming_the_remote_in_another_case_still_resolves() {
    // Names are one identity ignoring case (the registry rejects case-only
    // duplicates on that basis), so a reference spelled differently must find
    // the declaration rather than fail as undeclared.
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", EPOCH);
    let rev = origin.commit_files(&[("SKILL.md", "humanize\n")], EPOCH);
    declare(&f, "humanizer", &origin);
    f.module(
        "agents",
        "[[file]]\nsource = \"SKILL.md\"\nremote = \"Humanizer\"\n\
         target = \"~/.claude/skills/humanizer/SKILL.md\"\nmode = \"copy\"\n",
    );
    write_lock(&f, "humanizer", &origin, &rev);

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "a case-respelled reference must resolve; stderr: {}",
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
}

#[test]
fn one_module_mixes_its_own_files_with_a_remotes() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", EPOCH);
    let rev = origin.commit_files(&[("SKILL.md", "humanize\n")], EPOCH);
    declare(&f, "humanizer", &origin);
    let module = f.module(
        "agents",
        "[[file]]\nsource = \"shared/AGENTS.md\"\ntarget = \"~/.claude/CLAUDE.md\"\n\
         mode = \"copy\"\n\n\
         [[file]]\nsource = \"SKILL.md\"\nremote = \"humanizer\"\n\
         target = \"~/.claude/skills/humanizer/SKILL.md\"\nmode = \"copy\"\n",
    );
    fs_err::create_dir_all(module.join("shared").as_std_path()).expect("mkdir shared");
    fs_err::write(module.join("shared/AGENTS.md").as_std_path(), "be brief\n")
        .expect("write the local source");
    write_lock(&f, "humanizer", &origin, &rev);

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "a mixed module must plan and apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".claude/CLAUDE.md").as_std_path())
            .expect("the local target is readable"),
        "be brief\n"
    );
    assert_eq!(
        fs_err::read_to_string(
            f.home
                .join(".claude/skills/humanizer/SKILL.md")
                .as_std_path()
        )
        .expect("the remote-sourced target is readable"),
        "humanize\n"
    );
}

#[test]
fn an_entry_naming_an_undeclared_remote_fails_planning() {
    let f = Fixture::new();
    f.module(
        "agents",
        "[[file]]\nsource = \"SKILL.md\"\nremote = \"humanizer\"\ntarget = \"~/.skill.md\"\n",
    );

    let out = f.apply(&["--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        code(&out),
        1,
        "an entry naming nothing declared must fail; stderr: {stderr}"
    );
    assert!(
        stderr.contains("humanizer") && stderr.contains("[[remote]]"),
        "the message must name the remote and where to declare it; stderr: {stderr}"
    );
    assert!(!f.home.join(".skill.md").exists(), "nothing may be applied");
}

#[test]
fn a_remote_only_a_when_false_entry_names_is_never_fetched() {
    // Materializing a checkout is a consequence of an entry actually selecting
    // the remote here. This one is switched off on every host, so the run must
    // not read the (absent) pin, must not reach the (deleted) origin, and must
    // not leave a cache directory behind.
    let f = Fixture::new();
    let origin = Origin::new(&f, "unused", EPOCH);
    origin.commit_files(&[("a.md", "a\n")], EPOCH);
    declare(&f, "unused", &origin);
    let module = f.module(
        "agents",
        "[[file]]\nsource = \"local.md\"\ntarget = \"~/.local.md\"\nmode = \"copy\"\n\n\
         [[file]]\nsource = \"a.md\"\nremote = \"unused\"\ntarget = \"~/.a.md\"\n\
         when = \"false\"\n",
    );
    fs_err::write(module.join("local.md").as_std_path(), "local\n").expect("write local source");
    // The lockfile is left without a pin, so resolving this remote would fail
    // outright.
    fs_err::remove_dir_all(origin.dir.as_std_path()).expect("delete the origin");

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "a `when`-false entry must not drag its remote into the plan; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".local.md").as_std_path()).expect("local applied"),
        "local\n"
    );
    assert!(
        !patina_core::remote::cache::module_dir(&f.state_root(), &remote_name("unused")).exists(),
        "no cache directory may be created for a remote nothing active names"
    );
}

#[test]
fn a_repository_with_no_remote_module_ignores_a_malformed_lockfile() {
    // The lockfile is read lazily, on the first entry that selects a remote, so
    // a repo with no such entry must apply even when a stray patina.lock is
    // unreadable.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("zshrc").as_std_path(), "export X=1\n").expect("write source");
    fs_err::write(
        f.root.join("patina.lock").as_std_path(),
        "this is not valid toml : : :\n",
    )
    .expect("write a corrupt lockfile");

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "a non-remote repo must not fail on a malformed patina.lock; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".zshrc").as_std_path()).expect("deployed target"),
        "export X=1\n"
    );
}

#[test]
fn apply_update_under_json_does_not_bump_the_lockfile() {
    // A `--json` run is a single-document preview: `--update` must be ignored so
    // it neither rewrites patina.lock nor prints human lines onto stdout.
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", EPOCH);
    let rev = origin.commit_files(&[("skills/x/SKILL.md", "one\n")], EPOCH);
    declare(&f, "humanizer", &origin);
    f.module(
        "agents",
        "[[directory]]\nsource = \"skills/x\"\nremote = \"humanizer\"\n\
         target = \"~/.claude/skills/x\"\nmode = \"copy\"\n",
    );
    write_lock(&f, "humanizer", &origin, &rev);
    // A newer upstream tip a producer pass would otherwise bump to.
    origin.commit_files(&[("skills/x/SKILL.md", "two\n")], EPOCH);
    let before =
        fs_err::read_to_string(f.root.join("patina.lock").as_std_path()).expect("read lock");

    let out = f.apply(&["--update", "--json"]);

    assert_eq!(
        code(&out),
        0,
        "a json preview exits 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after =
        fs_err::read_to_string(f.root.join("patina.lock").as_std_path()).expect("read lock");
    assert_eq!(
        before, after,
        "`--update` under `--json` must not rewrite patina.lock"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ignored with `--json`"),
        "the skip must be announced on stderr: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .expect("stdout must be exactly one JSON document");
}

#[test]
fn a_remote_source_that_escapes_its_checkout_is_refused() {
    // A hostile manifest climbs out of the checkout with `..` to read host
    // files. The resolver must refuse it before anything is deployed.
    let f = Fixture::new();
    let origin = Origin::new(&f, "evil", EPOCH);
    let rev = origin.commit_files(&[("inside.txt", "ok\n")], EPOCH);
    declare(&f, "evil", &origin);
    f.module(
        "evil",
        "[[file]]\nsource = \"../escape.txt\"\nremote = \"evil\"\ntarget = \"~/.escaped\"\n",
    );
    write_lock(&f, "evil", &origin, &rev);

    let out = f.apply(&["--yes"]);
    assert_ne!(
        code(&out),
        0,
        "a source resolving outside the checkout must fail the apply"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("resolves outside its checkout"),
        "the failure must name the escape, got: {stderr}"
    );
    assert!(
        !f.home.join(".escaped").exists(),
        "nothing may be deployed when the source escapes the checkout"
    );
}

#[test]
fn a_remote_symlink_entry_points_into_the_pinned_checkout() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "prompts", EPOCH);
    let rev = origin.commit_files(&[("prompts/agent.md", "be brief\n")], EPOCH);
    declare(&f, "prompts", &origin);
    f.module(
        "prompts",
        "[[file]]\nsource = \"prompts/agent.md\"\nremote = \"prompts\"\n\
         target = \"~/.agent.md\"\n",
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
    // The engine records canonical paths, while `checkout` spells the cache
    // directory the way the environment gives the state dir. Canonicalizing
    // both keeps the comparison honest: otherwise it pits `/var/...` against
    // `/private/var/...` on macOS, and a long path against an 8.3 short one on
    // Windows.
    let expected = patina_core::canonicalize_path(&checkout(&f, "prompts", &rev))
        .expect("canonicalize the checkout directory");
    let link = patina_core::canonicalize_path(&link).expect("canonicalize the link target");
    assert!(
        link.starts_with(&expected),
        "the link must point into the pinned checkout {expected}, got {link}"
    );
}

#[test]
fn a_patina_toml_inside_the_checkout_contributes_nothing() {
    // Remote content is third-party input: mappings, hooks, and variables come
    // only from manifests in the user's own repository. A hostile manifest in the
    // checkout must be inert bytes.
    let f = Fixture::new();
    let origin = Origin::new(&f, "hostile", EPOCH);
    let rev = origin.commit_files(
        &[
            ("payload/note.md", "harmless\n"),
            (
                "patina.toml",
                "[patina]\nroot = true\n\n[[file]]\nsource = \"payload/note.md\"\n\
             target = \"~/.ssh/authorized_keys\"\nmode = \"copy\"\n\n\
             [[hook]]\nevent = \"post_apply\"\ncommand = \"touch pwned\"\n",
            ),
        ],
        EPOCH,
    );
    declare(&f, "hostile", &origin);
    f.module(
        "hostile",
        "[[file]]\nsource = \"payload/note.md\"\nremote = \"hostile\"\n\
         target = \"~/.note.md\"\nmode = \"copy\"\n",
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
fn an_entry_naming_an_unpinned_remote_fails_planning_and_names_the_command() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", EPOCH);
    origin.commit_files(&[("a.md", "a\n")], EPOCH);
    declare(&f, "humanizer", &origin);
    f.module(
        "agents",
        "[[file]]\nsource = \"a.md\"\nremote = \"humanizer\"\ntarget = \"~/.a.md\"\n\
         mode = \"copy\"\n",
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
    let origin = Origin::new(&f, "gone", EPOCH);
    let rev = origin.commit_files(&[("a.md", "a\n")], EPOCH);
    declare(&f, "gone", &origin);
    f.module(
        "agents",
        "[[file]]\nsource = \"a.md\"\nremote = \"gone\"\ntarget = \"~/.a.md\"\n\
         mode = \"copy\"\n",
    );
    write_lock(&f, "gone", &origin, &rev);
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
    let origin = Origin::new(&f, "warm", EPOCH);
    let rev = origin.commit_files(&[("a.md", "a\n")], EPOCH);
    declare(&f, "warm", &origin);
    f.module(
        "agents",
        "[[file]]\nsource = \"a.md\"\nremote = \"warm\"\ntarget = \"~/.a.md\"\n\
         mode = \"copy\"\n",
    );
    write_lock(&f, "warm", &origin, &rev);
    assert_eq!(code(&f.apply(&["--yes"])), 0, "priming apply");

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
    // on another, and hash differently in the journal. A checkout must contain
    // the commit's bytes verbatim.
    let f = Fixture::new();
    let origin = Origin::new(&f, "crlf", EPOCH);
    let rev = origin.commit_files(&[("a.md", "one\ntwo\n")], EPOCH);
    declare(&f, "crlf", &origin);
    f.module(
        "agents",
        "[[file]]\nsource = \"a.md\"\nremote = \"crlf\"\ntarget = \"~/.a.md\"\n\
         mode = \"copy\"\n",
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
    let origin = Origin::new(&f, "stable", EPOCH);
    let rev = origin.commit_files(&[("a.md", "a\n")], EPOCH);
    declare(&f, "stable", &origin);
    f.module(
        "agents",
        "[[file]]\nsource = \"a.md\"\nremote = \"stable\"\ntarget = \"~/.a.md\"\n\
         mode = \"copy\"\n",
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
    let origin = Origin::new(&f, "moving", EPOCH);
    let first_rev = origin.commit_files(&[("a.md", "first\n")], EPOCH);
    declare(&f, "moving", &origin);
    f.module(
        "agents",
        "[[file]]\nsource = \"a.md\"\nremote = \"moving\"\ntarget = \"~/.a.md\"\n",
    );
    write_lock(&f, "moving", &origin, &first_rev);
    assert_eq!(code(&f.apply(&["--yes"])), 0, "apply the first pin");

    // Two applies inside one second would share a `<ts>.COMMIT`. The prior
    // checkout would then be unreferenced and swept, and rollback would find a
    // dangling link.
    wait_for_next_second();

    let second_rev = origin.commit_files(&[("a.md", "second\n")], EPOCH);
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
    let origin = Origin::new(&f, "sweep", EPOCH);
    let rev = origin.commit_files(&[("a.md", "a\n")], EPOCH);
    declare(&f, "sweep", &origin);
    f.module(
        "agents",
        "[[file]]\nsource = \"a.md\"\nremote = \"sweep\"\ntarget = \"~/.a.md\"\n\
         mode = \"copy\"\n",
    );
    write_lock(&f, "sweep", &origin, &rev);
    assert_eq!(code(&f.apply(&["--yes"])), 0, "priming apply");

    // A checkout of a rev nothing was ever applied from is unreferenced by
    // every journal record, so the post-apply sweep must remove it.
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
