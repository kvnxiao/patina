//! Plan-time validation that no two active entries fight over one target.
//!
//! Two shapes are rejected, both before a diff is rendered and therefore
//! before anything is written: two active entries resolving to the same
//! canonical target, and an active whole-directory `symlink` entry whose target
//! contains another active entry's target. See `docs/REMOTE_SOURCES.md` under
//! "Target collision validation" for the normative rules.
//!
//! Only a whole-directory `symlink` owns a subtree, because that is the one
//! mode that plants a single object over the entire target path. A
//! `symlink-tree` or `copy` `[[directory]]` materializes one object per source
//! leaf and the journal records each leaf as its own target, so its footprint
//! is those leaves — which is what the planner hands this module, already
//! expanded. Two entries filling different parts of one directory are therefore
//! legal, and two entries writing one leaf of it are not.
//!
//! Validation runs over the **active** set, after `when` filtering, so two
//! entries aimed at one path under mutually exclusive guards are legal. Every
//! element of a `targets = [...]` fan-out participates, since each element is
//! its own claim on the filesystem.

use crate::config::FileMode;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::collections::BTreeMap;

/// One active entry's claim on the filesystem, as the collision check sees it.
///
/// Borrowed from the planner's resolved entries so validation allocates only
/// when it is about to fail.
#[derive(Debug, Clone, Copy)]
pub struct TargetClaim<'a> {
    /// Name of the module directory that declared the entry.
    pub module: &'a str,
    /// The entry's declared source, relative to its module directory (or to the
    /// checkout of the remote it names): the string the author wrote, so the
    /// error text points at something greppable in the manifest.
    pub source: &'a Utf8Path,
    /// The entry's resolved executor mode.
    pub mode: FileMode,
    /// The declared directory target [`targets`](Self::targets) are the leaves
    /// of, for a tree-mode entry the planner expanded; `None` when the targets
    /// are the declared targets themselves. Reported so an error over a leaf
    /// names the directory the author actually wrote.
    pub tree_target: Option<&'a Utf8Path>,
    /// The entry's canonical targets: one element per declared target, or one
    /// per materialized leaf for an expanded tree-mode entry.
    pub targets: &'a [Utf8PathBuf],
}

impl TargetClaim<'_> {
    /// Whether this claim plants a whole directory at each of its targets, so
    /// every path beneath one of those targets falls inside its footprint.
    fn owns_a_directory(&self) -> bool {
        matches!(self.mode, FileMode::SymlinkDir)
    }
}

/// Two active entries fight over one target.
///
/// The payload is boxed behind an opaque wrapper so [`EngineError`], which
/// every fallible engine entry point returns by value, stays one pointer wider
/// rather than growing to the size of the widest collision variant.
///
/// [`EngineError`]: crate::error::EngineError
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct CollisionError(Box<CollisionRepr>);

impl From<CollisionRepr> for CollisionError {
    fn from(repr: CollisionRepr) -> Self {
        Self(Box::new(repr))
    }
}

/// The collision shapes, private so new ones stay additive.
#[derive(Debug, thiserror::Error)]
enum CollisionRepr {
    /// Two active entries resolve to the same canonical target. Whichever ran
    /// second would silently overwrite the first, so the plan is refused.
    #[error(
        "two entries resolve to the same target {target}: `{first_source}` in module \
         `{first_module}`{} and `{second_source}` in module `{second_module}`{}. Give them \
         distinct targets, or guard them with mutually exclusive `when` predicates",
        leaf_of(.first_tree_target.as_deref()), leaf_of(.second_tree_target.as_deref())
    )]
    SameTarget {
        /// The canonical target both entries claim.
        target: Utf8PathBuf,
        /// Module of the entry declared first.
        first_module: String,
        /// Declared source of the entry declared first.
        first_source: Utf8PathBuf,
        /// Directory target the first entry's claim was expanded from, when it
        /// is a tree-mode entry.
        first_tree_target: Option<Utf8PathBuf>,
        /// Module of the entry declared second.
        second_module: String,
        /// Declared source of the entry declared second.
        second_source: Utf8PathBuf,
        /// Directory target the second entry's claim was expanded from, when it
        /// is a tree-mode entry.
        second_tree_target: Option<Utf8PathBuf>,
    },

    /// An active whole-directory `symlink` entry's target contains another
    /// active entry's target. That entry replaces the whole target path with a
    /// single link, so the inner entry's output would be planted over or
    /// swallowed.
    #[error(
        "the directory entry `{outer_source}` in module `{outer_module}` links its whole \
         target {outer_target}, which contains the target {inner_target} of \
         `{inner_source}` in module `{inner_module}`{}. A `[[directory]]` `mode = \"symlink\"` \
         replaces its target path outright, so move one of the two — or switch it to \
         `symlink-tree`, which owns only the leaves it materializes",
        leaf_of(.inner_tree_target.as_deref())
    )]
    ContainedTarget {
        /// Module of the whole-directory `symlink` entry.
        outer_module: String,
        /// Declared source of the whole-directory `symlink` entry.
        outer_source: Utf8PathBuf,
        /// That entry's canonical target.
        outer_target: Utf8PathBuf,
        /// Module of the entry whose target falls inside.
        inner_module: String,
        /// Declared source of the entry whose target falls inside.
        inner_source: Utf8PathBuf,
        /// Directory target the inner claim was expanded from, when it is a
        /// tree-mode entry.
        inner_tree_target: Option<Utf8PathBuf>,
        /// The contained canonical target.
        inner_target: Utf8PathBuf,
    },
}

/// The parenthetical naming the directory target an expanded leaf came from,
/// or nothing at all for an entry whose targets are as declared.
fn leaf_of(tree_target: Option<&Utf8Path>) -> String {
    tree_target.map_or_else(String::new, |target| {
        format!(" (under its target {target})")
    })
}

/// Reject both collision shapes over the active entry set.
///
/// Claims must arrive in declaration order, which makes the reported pair a
/// deterministic function of the manifest rather than of hash iteration.
///
/// # Errors
///
/// Returns a [`CollisionError`] naming the first pair of claims that share a
/// canonical target, or the first directory-mode target that contains another
/// claim's target.
pub fn validate_targets(claims: &[TargetClaim<'_>]) -> Result<(), CollisionError> {
    // Tree-mode leaves share parent directories, so the anchored respelling of
    // each parent is computed once and reused rather than re-resolved through
    // the filesystem per leaf.
    let mut anchors = BTreeMap::new();
    let mut staked: Vec<StakedTarget<'_>> = Vec::new();
    for claim in claims {
        for target in claim.targets {
            staked.push(StakedTarget {
                claim: *claim,
                target,
                key: comparison_key(target, &mut anchors),
            });
        }
    }
    reject_duplicate_targets(&staked)?;
    reject_contained_targets(&staked)
}

/// One target of one claim, with the key the comparisons use.
struct StakedTarget<'a> {
    claim: TargetClaim<'a>,
    target: &'a Utf8Path,
    key: Utf8PathBuf,
}

/// Re-spell `target` so two paths under the same chain of not-yet-created
/// directories compare against each other correctly.
///
/// [`resolve_location`](crate::paths::resolve_location) canonicalizes as much
/// of the parent chain as exists on disk, so how much of a resolved target is
/// canonical depends on how deep the missing directories start. Given
/// `~/.claude/skills` and `~/.claude/skills/note.md` where neither `.claude`
/// nor `skills` exists yet, the first comes back with `$HOME` canonicalized and
/// the second stays lexical from `$HOME` down. On a host where `$HOME` is
/// reached through a symbolic link (macOS `/var` to `/private/var`) or has a
/// short name (Windows 8.3), those two spellings differ and the containment
/// between them would go unnoticed.
///
/// Anchoring both on the deepest ancestor that does exist puts every target in
/// one spelling. The leaf itself is never canonicalized: a target already
/// materialized as a symbolic link into the repository would otherwise resolve
/// to its source, the same trap
/// [`resolve_location`](crate::paths::resolve_location) exists to avoid. The
/// key is for comparison only; errors report the target as the planner resolved
/// it.
fn comparison_key(
    target: &Utf8Path,
    anchors: &mut BTreeMap<Utf8PathBuf, Utf8PathBuf>,
) -> Utf8PathBuf {
    let Some(leaf) = target.file_name() else {
        return case_fold(target);
    };
    // The leaf rides along as a to-be-appended component from the start, so only
    // the parent chain (real directories) is ever resolved through the
    // filesystem — and each distinct parent only once per validation pass.
    let parent = match target.parent() {
        Some(parent) if !parent.as_str().is_empty() => parent,
        _ => return case_fold(target),
    };
    if let Some(anchored) = anchors.get(parent) {
        return case_fold(&anchored.join(leaf));
    }
    let anchored = anchor(parent).unwrap_or_else(|| parent.to_path_buf());
    let key = case_fold(&anchored.join(leaf));
    anchors.insert(parent.to_path_buf(), anchored);
    key
}

/// Re-spell `parent` on the deepest ancestor of it that exists, or `None` when
/// nothing on the path exists and every component is therefore already in its
/// lexical spelling.
fn anchor(parent: &Utf8Path) -> Option<Utf8PathBuf> {
    let mut missing: Vec<&str> = Vec::new();
    let mut cursor = parent;
    loop {
        if cursor.exists() {
            let mut anchored = crate::paths::canonicalize(cursor)
                .unwrap_or_else(|_uncanonicalizable| cursor.to_path_buf());
            for component in missing.iter().rev() {
                anchored.push(component);
            }
            return Some(anchored);
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(up), Some(name)) if !up.as_str().is_empty() => {
                missing.push(name);
                cursor = up;
            }
            _ => return None,
        }
    }
}

/// Fold the comparison key through [`crate::caseless::fold`], which owns the
/// case-and-normalization contract.
fn case_fold(key: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(crate::caseless::fold(key.as_str()))
}

/// Reject two claims resolving to the same target.
fn reject_duplicate_targets(staked: &[StakedTarget<'_>]) -> Result<(), CollisionError> {
    let mut seen: BTreeMap<&Utf8Path, &StakedTarget<'_>> = BTreeMap::new();
    for stake in staked {
        if let Some(first) = seen.insert(stake.key.as_path(), stake) {
            return Err(CollisionRepr::SameTarget {
                target: stake.target.to_path_buf(),
                first_module: first.claim.module.to_owned(),
                first_source: first.claim.source.to_path_buf(),
                first_tree_target: first.claim.tree_target.map(Utf8Path::to_path_buf),
                second_module: stake.claim.module.to_owned(),
                second_source: stake.claim.source.to_path_buf(),
                second_tree_target: stake.claim.tree_target.map(Utf8Path::to_path_buf),
            }
            .into());
        }
    }
    Ok(())
}

/// Reject a directory-mode target that strictly contains another claim's
/// target.
///
/// Runs after [`reject_duplicate_targets`], so no two keys are equal by the
/// time this walk starts; a `starts_with` hit is therefore always a
/// strict-descendant relation. The equality guard is kept anyway so the
/// function reads correctly on its own.
fn reject_contained_targets(staked: &[StakedTarget<'_>]) -> Result<(), CollisionError> {
    for outer in staked.iter().filter(|stake| stake.claim.owns_a_directory()) {
        for inner in staked {
            if inner.key == outer.key || !inner.key.starts_with(&outer.key) {
                continue;
            }
            return Err(CollisionRepr::ContainedTarget {
                outer_module: outer.claim.module.to_owned(),
                outer_source: outer.claim.source.to_path_buf(),
                outer_target: outer.target.to_path_buf(),
                inner_module: inner.claim.module.to_owned(),
                inner_source: inner.claim.source.to_path_buf(),
                inner_tree_target: inner.claim.tree_target.map(Utf8Path::to_path_buf),
                inner_target: inner.target.to_path_buf(),
            }
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a claim over string literals. Targets are already-canonical
    /// absolute paths in production; the tests use POSIX-shaped absolutes,
    /// which `camino` treats as ordinary paths on every host.
    fn claim<'a>(
        module: &'a str,
        source: &'a str,
        mode: FileMode,
        targets: &'a [Utf8PathBuf],
    ) -> TargetClaim<'a> {
        TargetClaim {
            module,
            source: Utf8Path::new(source),
            mode,
            tree_target: None,
            targets,
        }
    }

    /// The claim the planner produces for a tree-mode entry: leaf targets, plus
    /// the directory target they were expanded from.
    fn leaf_claim<'a>(
        module: &'a str,
        source: &'a str,
        mode: FileMode,
        tree_target: &'a str,
        targets: &'a [Utf8PathBuf],
    ) -> TargetClaim<'a> {
        TargetClaim {
            tree_target: Some(Utf8Path::new(tree_target)),
            ..claim(module, source, mode, targets)
        }
    }

    fn targets(paths: &[&str]) -> Vec<Utf8PathBuf> {
        paths.iter().map(Utf8PathBuf::from).collect()
    }

    #[test]
    fn two_entries_on_one_target_collide() {
        let a = targets(&["/home/u/.gitconfig"]);
        let b = targets(&["/home/u/.gitconfig"]);
        let claims = [
            claim("git", "gitconfig", FileMode::Symlink, &a),
            claim("work", "gitconfig", FileMode::Copy, &b),
        ];
        let err = validate_targets(&claims).expect_err("one target, two entries");
        assert!(
            matches!(
                err.0.as_ref(),
                CollisionRepr::SameTarget { first_module, second_module, .. }
                    if first_module == "git" && second_module == "work"
            ),
            "the first-declared entry must be reported first, got {err:?}"
        );
    }

    #[test]
    fn distinct_targets_do_not_collide() {
        let a = targets(&["/home/u/.gitconfig"]);
        let b = targets(&["/home/u/.zshrc"]);
        let claims = [
            claim("git", "gitconfig", FileMode::Symlink, &a),
            claim("zsh", "zshrc", FileMode::Symlink, &b),
        ];
        validate_targets(&claims).expect("distinct targets are legal");
    }

    #[test]
    fn a_target_nested_under_a_whole_directory_symlink_collides() {
        let outer = targets(&["/home/u/.config/nvim"]);
        let inner = targets(&["/home/u/.config/nvim/init.lua"]);
        let claims = [
            claim("nvim", "nvim", FileMode::SymlinkDir, &outer),
            claim("nvim-extra", "init.lua", FileMode::Symlink, &inner),
        ];
        let err = validate_targets(&claims).expect_err("nested under a whole-directory symlink");
        assert!(
            matches!(
                err.0.as_ref(),
                CollisionRepr::ContainedTarget { inner_target, .. }
                    if inner_target == "/home/u/.config/nvim/init.lua"
            ),
            "the contained target must be named, got {err:?}"
        );
    }

    #[test]
    fn containment_is_detected_when_the_inner_entry_is_declared_first() {
        // Declaration order must not decide whether containment is found: the
        // directory entry is the *second* claim here.
        let inner = targets(&["/home/u/.config/nvim/init.lua"]);
        let outer = targets(&["/home/u/.config/nvim"]);
        let claims = [
            claim("nvim-extra", "init.lua", FileMode::Symlink, &inner),
            claim("nvim", "nvim", FileMode::SymlinkDir, &outer),
        ];
        validate_targets(&claims).expect_err("order must not hide containment");
    }

    #[test]
    fn a_tree_entry_does_not_own_the_paths_between_its_leaves() {
        // A `symlink-tree` / `copy` `[[directory]]` materializes one object per
        // leaf, so another entry may fill a different part of the same
        // directory. The planner hands the leaves over already expanded, and
        // none of them is `/home/u/.config/nvim/lua/extra.lua`.
        let leaves = targets(&["/home/u/.config/nvim/init.lua"]);
        let other = targets(&["/home/u/.config/nvim/lua/extra.lua"]);
        let claims = [
            leaf_claim(
                "nvim",
                "nvim",
                FileMode::CopyTree,
                "/home/u/.config/nvim",
                &leaves,
            ),
            claim("nvim-extra", "extra.lua", FileMode::Symlink, &other),
        ];
        validate_targets(&claims).expect("a tree entry owns only the leaves it materializes");
    }

    #[test]
    fn a_tree_leaf_colliding_with_another_entry_is_refused() {
        let leaves = targets(&["/home/u/.config/nvim/init.lua"]);
        let other = targets(&["/home/u/.config/nvim/init.lua"]);
        let claims = [
            leaf_claim(
                "nvim",
                "nvim",
                FileMode::SymlinkTree,
                "/home/u/.config/nvim",
                &leaves,
            ),
            claim("nvim-extra", "init.lua", FileMode::Symlink, &other),
        ];
        let err = validate_targets(&claims).expect_err("one leaf, two entries");
        assert!(
            err.to_string().contains("/home/u/.config/nvim"),
            "the error must name the directory target the leaf came from, got: {err}"
        );
    }

    #[test]
    fn a_sibling_sharing_a_path_prefix_is_not_contained() {
        // `/home/u/.config/nvim-backup` shares the textual prefix
        // `/home/u/.config/nvim` but is not under it; component-wise
        // `starts_with` must not report it.
        let outer = targets(&["/home/u/.config/nvim"]);
        let sibling = targets(&["/home/u/.config/nvim-backup"]);
        let claims = [
            claim("nvim", "nvim", FileMode::SymlinkDir, &outer),
            claim("backup", "nvim-backup", FileMode::SymlinkDir, &sibling),
        ];
        validate_targets(&claims).expect("a prefix-sharing sibling is not contained");
    }

    #[test]
    fn a_file_target_does_not_contain_anything() {
        // Only directory-mode entries own a subtree. A `[[file]]` entry whose
        // target happens to be a path prefix of another target claims just the
        // one path, so this is not a containment error.
        let file_target = targets(&["/home/u/a"]);
        let under = targets(&["/home/u/a/b"]);
        let claims = [
            claim("m", "a", FileMode::Symlink, &file_target),
            claim("m", "b", FileMode::Symlink, &under),
        ];
        validate_targets(&claims).expect("a file-mode entry owns only its own path");
    }

    #[test]
    fn every_element_of_a_multi_target_fan_out_participates() {
        // The collision is on the *second* element of the first entry's
        // fan-out, so a check that only looked at the first target would miss
        // it.
        let fan_out = targets(&["/home/u/.a", "/home/u/.b"]);
        let single = targets(&["/home/u/.b"]);
        let claims = [
            claim("m", "shared", FileMode::Copy, &fan_out),
            claim("n", "other", FileMode::Copy, &single),
        ];
        let err = validate_targets(&claims).expect_err("fan-out element collides");
        assert!(
            matches!(
                err.0.as_ref(),
                CollisionRepr::SameTarget { target, .. } if target == "/home/u/.b"
            ),
            "the colliding fan-out element must be named, got {err:?}"
        );
    }

    #[test]
    fn a_fan_out_containing_its_own_sibling_target_collides() {
        // One entry declaring both a whole-directory link and something inside
        // it is the same hazard as two entries doing so.
        let fan_out = targets(&["/home/u/d", "/home/u/d/inner"]);
        let claims = [claim("m", "d", FileMode::SymlinkDir, &fan_out)];
        validate_targets(&claims).expect_err("a self-contained fan-out collides");
    }

    #[test]
    fn containment_is_found_when_the_two_targets_are_spelled_differently() {
        // A `..` hop stands in for the spelling divergence, since `Path::components`
        // preserves it while canonicalization resolves it away.
        let temp = TempDir::new().expect("tempdir");
        let home = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .join("home");
        fs_err::create_dir_all(home.as_std_path()).expect("mkdir home");

        let outer_target = home.join(".claude").join("skills");
        let inner_target = home
            .join("..")
            .join("home")
            .join(".claude")
            .join("skills")
            .join("note.md");
        assert!(
            !inner_target.starts_with(&outer_target),
            "the fixture must actually produce two different spellings"
        );

        let outer = vec![outer_target];
        let inner = vec![inner_target];
        let claims = [
            claim("skills", "skills", FileMode::SymlinkDir, &outer),
            claim("extra", "note.md", FileMode::Copy, &inner),
        ];
        validate_targets(&claims)
            .expect_err("containment must be found regardless of how each target is spelled");
    }

    use crate::test_util::symlink_file;

    #[test]
    fn a_target_that_is_a_symlink_is_keyed_by_its_location_not_its_source() {
        // A re-apply sees a target already materialized as a symlink into the
        // repository. The collision key must stay at the target location;
        // dereferencing the leaf to its source would compare the wrong paths and
        // miss (or invent) collisions.
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let source = dir.join("real.conf");
        fs_err::write(source.as_std_path(), b"x").expect("write source");
        let link = dir.join("link.conf");
        symlink_file(&source, &link);

        let key = comparison_key(&link, &mut BTreeMap::new());
        let canonical_source = crate::paths::canonicalize(&source).expect("canonicalize source");
        assert_ne!(
            key.as_str().to_lowercase(),
            canonical_source.as_str().to_lowercase(),
            "the key must not resolve the leaf symlink to its source"
        );
        assert!(
            key.as_str().to_lowercase().ends_with("link.conf"),
            "the key must keep the declared leaf name, got {key}"
        );
    }

    #[test]
    fn case_only_targets_collide_on_every_host() {
        let a = targets(&["/home/u/.config/app/config.toml"]);
        let b = targets(&["/home/u/.config/app/Config.toml"]);
        let claims = [
            claim("m", "config.toml", FileMode::Symlink, &a),
            claim("n", "Config.toml", FileMode::Copy, &b),
        ];
        validate_targets(&claims).expect_err("case-only-differing targets must collide");
    }

    #[test]
    fn targets_differing_only_in_unicode_normalization_collide() {
        // `café.conf` with a precomposed é, then with `e` plus a combining acute.
        let precomposed = targets(&["/home/u/.config/caf\u{e9}.conf"]);
        let decomposed = targets(&["/home/u/.config/cafe\u{301}.conf"]);
        assert_ne!(
            precomposed, decomposed,
            "the two spellings must differ as raw paths, or the test proves nothing"
        );
        let claims = [
            claim("m", "cafe.conf", FileMode::Symlink, &precomposed),
            claim("n", "cafe.conf", FileMode::Copy, &decomposed),
        ];
        validate_targets(&claims).expect_err("NFC and NFD spellings of one name must collide");
    }

    #[test]
    fn normalization_folding_survives_the_case_mapping() {
        let upper = targets(&["/home/u/.config/CAF\u{c9}.conf"]);
        let lower = targets(&["/home/u/.config/cafe\u{301}.conf"]);
        let claims = [
            claim("m", "a.conf", FileMode::Symlink, &upper),
            claim("n", "b.conf", FileMode::Copy, &lower),
        ];
        validate_targets(&claims).expect_err("case and normalization must fold together");
    }

    #[test]
    fn sharp_s_stays_distinct_from_its_two_letter_spelling() {
        let sharp = targets(&["/home/u/.config/stra\u{df}e.conf"]);
        let spelled = targets(&["/home/u/.config/strasse.conf"]);
        let claims = [
            claim("m", "a.conf", FileMode::Symlink, &sharp),
            claim("n", "b.conf", FileMode::Copy, &spelled),
        ];
        validate_targets(&claims).expect("ß and ss name different files");
    }

    #[test]
    fn a_directory_entry_contains_a_target_spelled_in_another_case() {
        let outer = targets(&["/home/u/.config/App"]);
        let inner = targets(&["/home/u/.config/app/init.lua"]);
        let claims = [
            claim("m", "App", FileMode::SymlinkDir, &outer),
            claim("n", "init.lua", FileMode::Symlink, &inner),
        ];
        validate_targets(&claims)
            .expect_err("containment must ignore case, like the same-target check");
    }

    #[test]
    fn an_empty_claim_set_is_legal() {
        validate_targets(&[]).expect("a repository with no active entries validates");
    }

    #[test]
    fn same_target_error_renders_both_entries() {
        let a = targets(&["/home/u/.gitconfig"]);
        let b = targets(&["/home/u/.gitconfig"]);
        let claims = [
            claim("git", "gitconfig", FileMode::Symlink, &a),
            claim("work", "config/gitconfig", FileMode::Copy, &b),
        ];
        let err = validate_targets(&claims).expect_err("collision");
        insta::assert_snapshot!(err.to_string());
    }

    #[test]
    fn contained_target_error_renders_both_entries() {
        let outer = targets(&["/home/u/.claude/skills"]);
        let inner = targets(&["/home/u/.claude/skills/humanizer/SKILL.md"]);
        let claims = [
            claim("skills", "skills", FileMode::SymlinkDir, &outer),
            leaf_claim(
                "humanizer",
                "humanizer",
                FileMode::CopyTree,
                "/home/u/.claude/skills/humanizer",
                &inner,
            ),
        ];
        let err = validate_targets(&claims).expect_err("containment");
        insta::assert_snapshot!(err.to_string());
    }
}
