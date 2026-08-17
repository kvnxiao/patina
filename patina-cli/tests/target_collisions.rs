//! Target-collision validation exercised through the CLI, so the failure occurs
//! before any diff or write. The rules live in `patina_core::apply::collisions`
//! and `docs/REMOTE_SOURCES.md` "Target collision validation".

mod common;

use common::Fixture;
use common::code;

/// The OS family string the engine's `patina.os` built-in resolves to on this
/// host, so a `when` predicate built from it is deterministically true here.
fn current_os_family() -> &'static str {
    std::env::consts::OS
}

#[test]
fn two_active_entries_on_one_target_fail_planning() {
    let f = Fixture::new();
    let git = f.module(
        "git",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(git.join("gitconfig"), "[user]\n").expect("write git source");
    let work = f.module(
        "work",
        "[[file]]\nsource = \"gitconfig\"\ntarget = \"~/.gitconfig\"\nmode = \"copy\"\n",
    );
    fs_err::write(work.join("gitconfig"), "[user]\nname = w\n").expect("write work source");

    let out = f.apply(&["--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        code(&out),
        1,
        "a colliding manifest must fail planning; stderr: {stderr}"
    );
    assert!(
        stderr.contains("same target") && stderr.contains("git") && stderr.contains("work"),
        "the error must include both colliding modules; stderr: {stderr}"
    );
    assert!(
        !f.home.join(".gitconfig").exists(),
        "planning must fail before any write"
    );
}

#[test]
fn when_disjoint_entries_on_one_target_plan_cleanly() {
    // The same target claimed twice, but the two `when` guards cannot both
    // hold. Only one entry is active, so the pair is legal and must apply.
    let f = Fixture::new();
    let os = current_os_family();
    let module = f.module(
        "shell",
        &format!(
            "[[file]]\nsource = \"here\"\ntarget = \"~/.profile\"\nmode = \"copy\"\n\
             when = \"patina.os == '{os}'\"\n\n\
             [[file]]\nsource = \"elsewhere\"\ntarget = \"~/.profile\"\nmode = \"copy\"\n\
             when = \"patina.os != '{os}'\"\n"
        ),
    );
    fs_err::write(module.join("here"), "on-this-os\n").expect("write here");
    fs_err::write(module.join("elsewhere"), "on-another-os\n").expect("write elsewhere");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "mutually exclusive `when` guards must not collide; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".profile")).expect("target written"),
        "on-this-os\n",
        "the active entry's source must be the one materialized"
    );
}

#[test]
fn a_target_inside_a_whole_directory_symlink_target_fails_planning() {
    let f = Fixture::new();
    let tree = f.module(
        "skills",
        "[[directory]]\nsource = \"skills\"\ntarget = \"~/.claude/skills\"\n",
    );
    fs_err::create_dir_all(tree.join("skills")).expect("mkdir tree source");
    fs_err::write(tree.join("skills").join("keep.md"), "keep\n").expect("write leaf");
    let inner = f.module(
        "extra",
        "[[file]]\nsource = \"note.md\"\ntarget = \"~/.claude/skills/note.md\"\nmode = \"copy\"\n",
    );
    fs_err::write(inner.join("note.md"), "note\n").expect("write inner source");

    let out = f.apply(&["--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        code(&out),
        1,
        "a target inside a whole-directory symlink must fail planning; stderr: {stderr}"
    );
    assert!(
        stderr.contains("contains the target"),
        "the error must explain the containment; stderr: {stderr}"
    );
    assert!(
        !f.home.join(".claude").join("skills").exists(),
        "planning must fail before any write"
    );
}

#[test]
fn a_tree_leaf_colliding_with_another_entry_includes_the_leaf() {
    // The tree deploys `humanizer/SKILL.md` under `~/.claude/skills`, and the
    // other entry claims exactly that path. The two declared targets are a
    // directory and a file inside it, so only the expanded leaves make the
    // collision visible.
    let f = Fixture::new();
    let tree = f.module(
        "skills",
        "[[directory]]\nsource = \"skills\"\ntarget = \"~/.claude/skills\"\nmode = \"copy\"\n",
    );
    fs_err::create_dir_all(tree.join("skills").join("humanizer")).expect("mkdir tree source");
    fs_err::write(
        tree.join("skills").join("humanizer").join("SKILL.md"),
        "local\n",
    )
    .expect("write leaf");
    let inner = f.module(
        "extra",
        "[[file]]\nsource = \"SKILL.md\"\n\
         target = \"~/.claude/skills/humanizer/SKILL.md\"\nmode = \"copy\"\n",
    );
    fs_err::write(inner.join("SKILL.md"), "upstream\n").expect("write inner source");

    let out = f.apply(&["--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        code(&out),
        1,
        "two entries writing one leaf must fail planning; stderr: {stderr}"
    );
    assert!(
        stderr.contains("same target") && stderr.contains("SKILL.md"),
        "the error must include the colliding leaf; stderr: {stderr}"
    );
    assert!(
        !f.home.join(".claude").join("skills").exists(),
        "planning must fail before any write"
    );
}

#[test]
fn an_entry_under_a_tree_target_that_hits_no_leaf_plans_cleanly() {
    // A remote entry takes this shape when it adds one upstream file to a
    // directory the repository fills.
    let f = Fixture::new();
    let tree = f.module(
        "skills",
        "[[directory]]\nsource = \"skills\"\ntarget = \"~/.claude/skills\"\nmode = \"copy\"\n",
    );
    fs_err::create_dir_all(tree.join("skills")).expect("mkdir tree source");
    fs_err::write(tree.join("skills").join("keep.md"), "keep\n").expect("write leaf");
    let inner = f.module(
        "extra",
        "[[file]]\nsource = \"SKILL.md\"\n\
         target = \"~/.claude/skills/humanizer/SKILL.md\"\nmode = \"copy\"\n",
    );
    fs_err::write(inner.join("SKILL.md"), "upstream\n").expect("write inner source");

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "a path under a tree target that no leaf claims must apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".claude/skills/keep.md")).expect("the tree leaf"),
        "keep\n"
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".claude/skills/humanizer/SKILL.md"))
            .expect("the neighbouring entry"),
        "upstream\n"
    );
}

#[test]
fn a_multi_target_fan_out_element_collides() {
    let f = Fixture::new();
    let module = f.module(
        "shared",
        "[[file]]\nsource = \"rc\"\ntargets = [\"~/.rc-a\", \"~/.rc-b\"]\nmode = \"copy\"\n\n\
         [[file]]\nsource = \"other\"\ntarget = \"~/.rc-b\"\nmode = \"copy\"\n",
    );
    fs_err::write(module.join("rc"), "rc\n").expect("write rc");
    fs_err::write(module.join("other"), "other\n").expect("write other");

    let out = f.apply(&["--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        code(&out),
        1,
        "a fan-out element collision must fail planning; stderr: {stderr}"
    );
    assert!(
        stderr.contains(".rc-b"),
        "the error must include the colliding fan-out element; stderr: {stderr}"
    );
    assert!(
        !f.home.join(".rc-a").exists(),
        "planning must fail before any write, including the non-colliding element"
    );
}
