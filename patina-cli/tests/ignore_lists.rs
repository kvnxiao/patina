//! Integration tests for `ignore_lists`.
#![expect(
    clippy::expect_used,
    reason = "Integration tests use .expect() for fixtures and asserted output outside #[cfg(test)] modules; allow-expect-in-tests does not cover integration-crate roots."
)]

mod common;

use camino::Utf8Path;
use common::Fixture;
use common::code;

fn set_repo_ignore(f: &Fixture, patterns: &[&str]) {
    let list = patterns
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs_err::write(
        f.root.join("patina.toml"),
        format!("[patina]\nroot = true\nignore = [{list}]\n"),
    )
    .expect("write root manifest");
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn assert_applied(out: &std::process::Output) {
    assert_eq!(
        code(out),
        0,
        "apply must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_ignored_directory_contributes_no_leaves_including_nested_ones() {
    let f = Fixture::new();
    let module = f.module(
        "py",
        "[[directory]]\nsource = \"scripts\"\ntarget = \"~/bin\"\nmode = \"symlink-tree\"\n\
         ignore = [\"__pycache__/\"]\n",
    );
    let src = module.join("scripts");
    fs_err::create_dir_all(src.join("__pycache__")).expect("mkdir cache");
    fs_err::write(src.join("run.py"), b"print(1)").expect("write script");
    fs_err::write(src.join("__pycache__").join("mod.pyc"), b"\x00").expect("write pyc");

    assert_applied(&f.apply(&["--yes"]));

    assert!(
        f.home.join("bin").join("run.py").exists(),
        "the real leaf must deploy"
    );
    assert!(
        !f.home.join("bin").join("__pycache__").exists(),
        "an ignored directory must not be recreated at the target"
    );
}

#[test]
fn a_repo_wide_pattern_reaches_an_entry_declaring_no_list() {
    let f = Fixture::new();
    set_repo_ignore(&f, &["*.pyc"]);
    let module = f.module(
        "py",
        "[[directory]]\nsource = \"scripts\"\ntarget = \"~/bin\"\nmode = \"symlink-tree\"\n",
    );
    let src = module.join("scripts");
    fs_err::create_dir_all(&src).expect("mkdir src");
    fs_err::write(src.join("run.py"), b"x").expect("write script");
    fs_err::write(src.join("stale.pyc"), b"y").expect("write pyc");

    assert_applied(&f.apply(&["--yes"]));

    assert!(f.home.join("bin").join("run.py").exists());
    assert!(
        !f.home.join("bin").join("stale.pyc").exists(),
        "the repo-wide pattern must apply to an entry that declares no `ignore`"
    );
}

#[test]
fn an_entry_negation_overrides_a_repo_wide_pattern() {
    let f = Fixture::new();
    set_repo_ignore(&f, &["*.pyc"]);
    let module = f.module(
        "py",
        "[[directory]]\nsource = \"scripts\"\ntarget = \"~/bin\"\nmode = \"symlink-tree\"\n\
         ignore = [\"!keep.pyc\"]\n",
    );
    let src = module.join("scripts");
    fs_err::create_dir_all(&src).expect("mkdir src");
    fs_err::write(src.join("keep.pyc"), b"x").expect("write keep");
    fs_err::write(src.join("drop.pyc"), b"y").expect("write drop");

    assert_applied(&f.apply(&["--yes"]));

    assert!(
        f.home.join("bin").join("keep.pyc").exists(),
        "the per-entry negation must rescue the leaf the repo-wide pattern dropped"
    );
    assert!(
        !f.home.join("bin").join("drop.pyc").exists(),
        "the repo-wide pattern still governs every other leaf"
    );
}

#[test]
fn matching_folds_case_on_every_platform() {
    let f = Fixture::new();
    let module = f.module(
        "junk",
        "[[directory]]\nsource = \"d\"\ntarget = \"~/d\"\nmode = \"symlink-tree\"\n\
         ignore = [\"thumbs.db\"]\n",
    );
    let src = module.join("d");
    fs_err::create_dir_all(&src).expect("mkdir src");
    fs_err::write(src.join("Thumbs.db"), b"x").expect("write junk");
    fs_err::write(src.join("real.conf"), b"y").expect("write real");

    assert_applied(&f.apply(&["--yes"]));

    assert!(f.home.join("d").join("real.conf").exists());
    assert!(
        !f.home.join("d").join("Thumbs.db").exists(),
        "matching folds case, so one lowercase pattern covers the capitalized file"
    );
}

fn fixture_with_a_newly_ignored_deployed_leaf() -> Fixture {
    let f = Fixture::new();
    let manifest =
        "[[directory]]\nsource = \"scripts\"\ntarget = \"~/bin\"\nmode = \"symlink-tree\"\n";
    let module = f.module("py", manifest);
    let src = module.join("scripts");
    fs_err::create_dir_all(&src).expect("mkdir src");
    fs_err::write(src.join("run.py"), b"x").expect("write script");
    fs_err::write(src.join("stale.pyc"), b"y").expect("write pyc");

    assert_applied(&f.apply(&["--yes"]));
    assert!(
        f.home.join("bin").join("stale.pyc").exists(),
        "the first apply deploys the leaf, before any pattern excludes it"
    );

    fs_err::write(
        module.join("patina.toml"),
        format!("{manifest}ignore = [\"*.pyc\"]\n"),
    )
    .expect("rewrite module manifest");
    f
}

#[test]
fn a_newly_ignored_deployed_leaf_is_reaped_and_the_diff_includes_the_reason() {
    let f = fixture_with_a_newly_ignored_deployed_leaf();

    let preview = f.apply(&[]);
    let body = stdout_of(&preview);
    assert!(
        body.contains("(ignored)"),
        "the removal must include its reason so a just-added pattern explains the deletion, \
         got:\n{body}"
    );
    assert!(
        f.home.join("bin").join("stale.pyc").exists(),
        "a preview must not mutate anything"
    );

    assert_applied(&f.apply(&["--yes"]));
    assert!(
        !f.home.join("bin").join("stale.pyc").exists(),
        "the confirmed apply reaps the now-ignored leaf"
    );
    assert!(
        f.home.join("bin").join("run.py").exists(),
        "its sibling is untouched"
    );
}

#[test]
fn the_json_reaped_array_carries_the_target_and_the_reason() {
    let f = fixture_with_a_newly_ignored_deployed_leaf();

    let out = f.apply(&["--json"]);
    let body = stdout_of(&out);
    let document: serde_json::Value =
        serde_json::from_str(&body).expect("--json emits one JSON document");
    let reaped = document
        .get("reaped")
        .and_then(serde_json::Value::as_array)
        .expect("the envelope carries a reaped array");

    assert_eq!(reaped.len(), 1, "one leaf became ignored, got: {reaped:?}");
    let row = reaped.first().expect("one reaped row");
    assert_eq!(
        row.get("reason").and_then(serde_json::Value::as_str),
        Some("ignored")
    );
    let target = row
        .get("target")
        .and_then(serde_json::Value::as_str)
        .expect("each reaped row carries a target");
    assert!(
        Utf8Path::new(target).ends_with("stale.pyc"),
        "the reaped target must be the now-ignored leaf, got {target}"
    );
}

#[test]
fn re_applying_over_a_source_holding_ignored_paths_is_a_byte_identical_no_op() {
    let f = Fixture::new();
    let module = f.module(
        "py",
        "[[directory]]\nsource = \"scripts\"\ntarget = \"~/bin\"\nmode = \"symlink-tree\"\n\
         ignore = [\"__pycache__/\", \"*.pyc\"]\n",
    );
    let src = module.join("scripts");
    fs_err::create_dir_all(&src).expect("mkdir src");
    fs_err::write(src.join("run.py"), b"x").expect("write script");

    assert_applied(&f.apply(&["--yes"]));

    fs_err::create_dir_all(src.join("__pycache__")).expect("mkdir cache");
    fs_err::write(src.join("__pycache__").join("run.pyc"), b"\x00").expect("write pyc");

    let first = f.apply(&["--yes"]);
    assert_applied(&first);
    let second = f.apply(&["--yes"]);
    assert_applied(&second);

    assert_eq!(
        stdout_of(&first),
        stdout_of(&second),
        "two applies over an unchanged source must produce byte-identical stdout"
    );
    assert!(
        !f.home.join("bin").join("__pycache__").exists(),
        "the generated cache never reaches the target"
    );
}

fn fixture_with_a_pyc_excluding_tree() -> (Fixture, camino::Utf8PathBuf) {
    let f = Fixture::new();
    let module = f.module(
        "py",
        "[[directory]]\nsource = \"scripts\"\ntarget = \"~/bin\"\nmode = \"symlink-tree\"\n\
         ignore = [\"*.pyc\"]\n",
    );
    fs_err::create_dir_all(module.join("scripts")).expect("mkdir src");
    (f, module)
}

#[test]
fn add_refuses_a_file_the_tree_it_lands_in_already_excludes() {
    let (f, _module) = fixture_with_a_pyc_excluding_tree();
    let source = f.home.join("cached.pyc");
    fs_err::write(source.as_std_path(), b"x").expect("seed the file to add");

    let out = f.run(
        &[
            "add",
            "~/cached.pyc",
            "--module",
            "py/scripts",
            "--symlink",
            "--yes",
        ],
        &[],
    );

    assert_ne!(code(&out), 0, "the contradiction must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--force"),
        "the refusal must include the override, got:\n{stderr}"
    );
    assert!(
        source.exists(),
        "a refusal must leave the user's file where it was"
    );
}

#[test]
fn add_force_declares_the_excluded_path_anyway() {
    let (f, module) = fixture_with_a_pyc_excluding_tree();
    fs_err::write(f.home.join("cached.pyc").as_std_path(), b"x").expect("seed the file to add");

    let out = f.run(
        &[
            "add",
            "~/cached.pyc",
            "--module",
            "py/scripts",
            "--symlink",
            "--yes",
            "--force",
        ],
        &[],
    );

    assert_eq!(
        code(&out),
        0,
        "--force overrides the refusal; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        module.join("scripts").join("cached.pyc").is_file(),
        "the path is staged into the repository despite the pattern"
    );
}

#[test]
fn doctor_reports_targets_a_new_pattern_stranded() {
    let f = fixture_with_a_newly_ignored_deployed_leaf();

    let out = f.run(&["doctor", "--json"], &[]);
    let body = stdout_of(&out);
    let document: serde_json::Value =
        serde_json::from_str(&body).expect("doctor --json emits one JSON document");
    let findings = document
        .get("findings")
        .and_then(serde_json::Value::as_array)
        .expect("the document carries a findings array");

    let stranded = findings
        .iter()
        .find(|f| f.get("code").and_then(serde_json::Value::as_str) == Some("DOC-IGNORED-DEPLOYED"))
        .expect("the DOC-IGNORED-DEPLOYED finding must be raised");
    assert_eq!(
        stranded.get("level").and_then(serde_json::Value::as_str),
        Some("warning"),
        "a pending reap is a heads-up, not an error"
    );

    let message = stranded
        .get("message")
        .and_then(serde_json::Value::as_str)
        .expect("the finding carries a message");
    assert!(
        !message.contains("  "),
        "a wrapped message literal must not leak its source indentation, got:\n{message}"
    );
}

fn fixture_with_a_newly_ignored_directory() -> Fixture {
    let f = Fixture::new();
    let manifest =
        "[[directory]]\nsource = \"scripts\"\ntarget = \"~/bin\"\nmode = \"symlink-tree\"\n";
    let module = f.module("py", manifest);
    let src = module.join("scripts");
    fs_err::create_dir_all(src.join("__pycache__")).expect("mkdir cache");
    fs_err::write(src.join("run.py"), b"x").expect("write script");
    fs_err::write(src.join("__pycache__").join("mod.pyc"), b"y").expect("write pyc");

    assert_applied(&f.apply(&["--yes"]));
    assert!(
        f.home
            .join("bin")
            .join("__pycache__")
            .join("mod.pyc")
            .exists(),
        "the first apply deploys the nested leaf, before any pattern excludes it"
    );

    fs_err::write(
        module.join("patina.toml"),
        format!("{manifest}ignore = [\"__pycache__/\"]\n"),
    )
    .expect("rewrite module manifest");
    f
}

#[test]
fn a_leaf_under_a_newly_ignored_directory_is_reaped() {
    let f = fixture_with_a_newly_ignored_directory();

    let out = f.apply(&["--json"]);
    let document: serde_json::Value =
        serde_json::from_str(&stdout_of(&out)).expect("--json emits one JSON document");
    let reaped = document
        .get("reaped")
        .and_then(serde_json::Value::as_array)
        .expect("the envelope carries a reaped array");
    assert_eq!(
        reaped.len(),
        1,
        "the nested leaf is the one pending removal, got: {reaped:?}"
    );
    assert_eq!(
        reaped
            .first()
            .and_then(|row| row.get("reason"))
            .and_then(serde_json::Value::as_str),
        Some("ignored")
    );

    assert_applied(&f.apply(&["--yes"]));
    assert!(
        !f.home
            .join("bin")
            .join("__pycache__")
            .join("mod.pyc")
            .exists(),
        "the leaf under the newly-ignored directory is reaped"
    );
    assert!(
        f.home.join("bin").join("run.py").exists(),
        "its sibling outside the directory is untouched"
    );
}

#[test]
fn a_negation_the_walk_cannot_reach_does_not_strand_a_deployed_leaf() {
    let f = Fixture::new();
    let manifest = "[[directory]]\nsource = \"out\"\ntarget = \"~/out\"\nmode = \"symlink-tree\"\n";
    let module = f.module("gen", manifest);
    let src = module.join("out");
    fs_err::create_dir_all(src.join("build")).expect("mkdir build");
    fs_err::write(src.join("build").join("keep.txt"), b"x").expect("write keep");

    assert_applied(&f.apply(&["--yes"]));
    assert!(f.home.join("out").join("build").join("keep.txt").exists());

    fs_err::write(
        module.join("patina.toml"),
        format!("{manifest}ignore = [\"build/\", \"!build/keep.txt\"]\n"),
    )
    .expect("rewrite module manifest");

    assert_applied(&f.apply(&["--yes"]));
    assert!(
        !f.home.join("out").join("build").join("keep.txt").exists(),
        "an ancestor pattern outranks a leaf negation, so the deployed leaf is reaped"
    );
}

#[test]
fn an_entry_whose_every_leaf_is_ignored_settles_instead_of_re_prompting() {
    let f = Fixture::new();
    let module = f.module(
        "py",
        "[[directory]]\nsource = \"scripts\"\ntarget = \"~/bin\"\nmode = \"symlink-tree\"\n\
         ignore = [\"__pycache__/\"]\n",
    );
    let src = module.join("scripts");
    fs_err::create_dir_all(src.join("__pycache__")).expect("mkdir cache");
    fs_err::write(src.join("__pycache__").join("mod.pyc"), b"y").expect("write pyc");

    assert_applied(&f.apply(&["--yes"]));
    assert!(
        !f.home.join("bin").exists(),
        "nothing is deployed, so the target directory is never created"
    );

    let out = f.apply(&["--json"]);
    let document: serde_json::Value =
        serde_json::from_str(&stdout_of(&out)).expect("--json emits one JSON document");
    let plan = document
        .get("plan")
        .and_then(serde_json::Value::as_array)
        .expect("the envelope carries a plan array");
    for row in plan {
        assert_eq!(
            row.get("state").and_then(serde_json::Value::as_str),
            Some("unchanged"),
            "an entry with nothing left to deploy has settled, got: {row:?}"
        );
    }
}

#[test]
fn add_of_a_whole_directory_symlink_does_not_warn_about_ignored_leaves() {
    let f = Fixture::new();
    set_repo_ignore(&f, &["*.pyc"]);
    f.module("py", "");
    let source = f.home.join("tools");
    fs_err::create_dir_all(source.as_std_path()).expect("mkdir the directory to add");
    fs_err::write(source.join("run.py").as_std_path(), b"x").expect("write script");
    fs_err::write(source.join("mod.pyc").as_std_path(), b"y").expect("write pyc");

    let out = f.run(
        &["add", "~/tools", "--module", "py", "--symlink", "--yes"],
        &[],
    );

    assert_eq!(
        code(&out),
        0,
        "the add must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("ignore"),
        "a whole-directory symlink deploys every leaf, so nothing is excluded, got:\n{stderr}"
    );
}
