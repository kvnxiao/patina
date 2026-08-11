//! Plan-time validation that no two active entries fight over one target.
//!
//! Two shapes are rejected, both before a diff is rendered and therefore
//! before anything is written: two active entries resolving to the same
//! canonical target, and an active directory-mode entry whose target contains
//! another active entry's target. See `docs/REMOTE_SOURCES.md` under
//! "Target collision validation" for the normative rules.
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
    /// The entry's declared source, relative to its module directory: the
    /// string the author wrote, so the error text points at something
    /// greppable in the manifest.
    pub source: &'a Utf8Path,
    /// The entry's resolved executor mode.
    pub mode: FileMode,
    /// The entry's canonical targets, one element per declared target.
    pub targets: &'a [Utf8PathBuf],
}

impl TargetClaim<'_> {
    /// Whether this claim plants a whole directory at each of its targets, so
    /// every path beneath one of those targets falls inside its footprint.
    fn owns_a_directory(&self) -> bool {
        matches!(
            self.mode,
            FileMode::SymlinkDir | FileMode::SymlinkTree | FileMode::CopyTree
        )
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
         `{first_module}` and `{second_source}` in module `{second_module}`. Give them \
         distinct targets, or guard them with mutually exclusive `when` predicates"
    )]
    SameTarget {
        /// The canonical target both entries claim.
        target: Utf8PathBuf,
        /// Module of the entry declared first.
        first_module: String,
        /// Declared source of the entry declared first.
        first_source: Utf8PathBuf,
        /// Module of the entry declared second.
        second_module: String,
        /// Declared source of the entry declared second.
        second_source: Utf8PathBuf,
    },

    /// An active directory-mode entry's target directory contains another
    /// active entry's target. The directory entry owns everything under its
    /// target, so the inner entry's output would be planted over or replaced.
    #[error(
        "the directory entry `{outer_source}` in module `{outer_module}` deploys to \
         {outer_target}, which contains the target {inner_target} of `{inner_source}` in \
         module `{inner_module}`. A directory entry owns every path under its target, so \
         move one of the two"
    )]
    ContainedTarget {
        /// Module of the directory-mode entry.
        outer_module: String,
        /// Declared source of the directory-mode entry.
        outer_source: Utf8PathBuf,
        /// The directory-mode entry's canonical target.
        outer_target: Utf8PathBuf,
        /// Module of the entry whose target falls inside.
        inner_module: String,
        /// Declared source of the entry whose target falls inside.
        inner_source: Utf8PathBuf,
        /// The contained canonical target.
        inner_target: Utf8PathBuf,
    },
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
    let staked: Vec<StakedTarget<'_>> = claims
        .iter()
        .flat_map(|claim| {
            claim.targets.iter().map(move |target| StakedTarget {
                claim: *claim,
                target,
                key: comparison_key(target),
            })
        })
        .collect();
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
/// one spelling. The key is for comparison only; errors report the target as
/// the planner resolved it.
fn comparison_key(target: &Utf8Path) -> Utf8PathBuf {
    let mut missing: Vec<&str> = Vec::new();
    let mut cursor = target;
    loop {
        if cursor.exists() {
            let mut key = crate::paths::canonicalize(cursor)
                .unwrap_or_else(|_uncanonicalizable| cursor.to_path_buf());
            for component in missing.iter().rev() {
                key.push(component);
            }
            return key;
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(parent), Some(name)) if !parent.as_str().is_empty() => {
                missing.push(name);
                cursor = parent;
            }
            // Nothing on the path exists, so there is nothing to anchor on and
            // every target is already in its lexical spelling.
            _ => return target.to_path_buf(),
        }
    }
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
                second_module: stake.claim.module.to_owned(),
                second_source: stake.claim.source.to_path_buf(),
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
            targets,
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
    fn a_target_nested_under_a_directory_target_collides() {
        let outer = targets(&["/home/u/.config/nvim"]);
        let inner = targets(&["/home/u/.config/nvim/init.lua"]);
        let claims = [
            claim("nvim", "nvim", FileMode::CopyTree, &outer),
            claim("nvim-extra", "init.lua", FileMode::Symlink, &inner),
        ];
        let err = validate_targets(&claims).expect_err("nested under a directory target");
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
            claim("nvim", "nvim", FileMode::SymlinkTree, &outer),
        ];
        validate_targets(&claims).expect_err("order must not hide containment");
    }

    #[test]
    fn a_sibling_sharing_a_path_prefix_is_not_contained() {
        // `/home/u/.config/nvim-backup` shares the textual prefix
        // `/home/u/.config/nvim` but is not under it; component-wise
        // `starts_with` must not report it.
        let outer = targets(&["/home/u/.config/nvim"]);
        let sibling = targets(&["/home/u/.config/nvim-backup"]);
        let claims = [
            claim("nvim", "nvim", FileMode::CopyTree, &outer),
            claim("backup", "nvim-backup", FileMode::CopyTree, &sibling),
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
        // One entry declaring both a directory and something inside it is the
        // same hazard as two entries doing so.
        let fan_out = targets(&["/home/u/d", "/home/u/d/inner"]);
        let claims = [claim("m", "d", FileMode::CopyTree, &fan_out)];
        validate_targets(&claims).expect_err("a self-contained fan-out collides");
    }

    #[test]
    fn containment_is_found_when_the_two_targets_are_spelled_differently() {
        // The planner resolves a target by canonicalizing as much of its parent
        // chain as exists, so a deeper target under the same missing directories
        // can come back in a different spelling than its shallower neighbour.
        // That is what happens on a host where `$HOME` is behind a symbolic link
        // (macOS `/var` to `/private/var`) or carries an 8.3 short name.
        // A `..` hop stands in for the divergence, since `Path::components`
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
            claim("skills", "skills", FileMode::CopyTree, &outer),
            claim("extra", "note.md", FileMode::Copy, &inner),
        ];
        validate_targets(&claims)
            .expect_err("containment must be found regardless of how each target is spelled");
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
        let inner = targets(&["/home/u/.claude/skills/humanizer"]);
        let claims = [
            claim("skills", "skills", FileMode::SymlinkTree, &outer),
            claim("humanizer", "skills/humanizer", FileMode::CopyTree, &inner),
        ];
        let err = validate_targets(&claims).expect_err("containment");
        insta::assert_snapshot!(err.to_string());
    }
}
