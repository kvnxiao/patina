//! Gitignore-syntax ignore rules for tree-mode entries.
//!
//! A `symlink-tree` or directory-`copy` entry enumerates its source tree on
//! every plan. Tools that write beside their inputs (`__pycache__`,
//! `.DS_Store`, `Thumbs.db`) drop files into that tree, and without a filter
//! every one of them becomes a leaf Patina offers to deploy. The rules built
//! here are the filter.
//!
//! The root manifest's repo-wide `[patina] ignore` is concatenated with the
//! entry's own, in that order. Because gitignore matching is last-match-wins,
//! a per-entry `!keep.log` overrides a repo-wide `*.log`, while a repo-wide
//! pattern still reaches every entry that declares none.
//!
//! Where this departs from git, deliberately:
//!
//! - **Matching is case-insensitive on every platform.** Git decides this per
//!   clone via `core.ignorecase`, which would make one manifest behave three
//!   ways across macOS, Linux, and Windows. The source bytes are identical on
//!   all three; the match follows them.
//! - **There are no per-directory ignore files.** Every pattern is authored in
//!   a manifest Patina already trusts. A `.gitignore` inside a remote checkout
//!   is third-party content and is never read (`docs/REMOTE_SOURCES.md`, "Trust
//!   boundaries").
//! - **Patterns anchor at the entry's source directory**, at both levels. A
//!   repo-wide `/build` means "`build` at the top of each entry's source",
//!   rather than one path at the repository root.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use ignore::gitignore::Gitignore;
use ignore::gitignore::GitignoreBuilder;

/// Failures from building a [`Gitignore`] out of manifest-declared patterns.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IgnoreRulesError {
    /// A declared pattern is not a valid glob. The pattern is quoted so the
    /// author can find it in the manifest without counting list positions.
    #[error("ignore pattern `{pattern}` is not a valid glob: {source}")]
    Pattern {
        /// The offending pattern exactly as authored.
        pattern: String,
        #[source]
        /// The underlying glob-parse failure.
        source: Box<ignore::Error>,
    },

    /// The assembled pattern set could not be compiled into a matcher.
    #[error("failed to compile the ignore rules for source `{source_root}`: {source}")]
    Build {
        /// The entry source directory the rules were anchored at.
        source_root: String,
        #[source]
        /// The underlying compile failure.
        source: Box<ignore::Error>,
    },
}

/// Build the matcher one tree-mode entry filters its source walk through.
///
/// `repo_wide` holds the root manifest's `[patina] ignore` patterns and `entry`
/// holds the entry's own. Both are added in that order, giving `entry` the last
/// word on a path they disagree about. `source_root` is the entry's canonical
/// source directory, which the patterns anchor at.
///
/// Two empty lists produce a matcher that matches nothing, the same value
/// [`none`] returns.
///
/// # Errors
///
/// Returns [`IgnoreRulesError::Pattern`] naming the first pattern that is not a
/// valid glob, and [`IgnoreRulesError::Build`] when the assembled set fails to
/// compile.
pub fn build(
    repo_wide: &[String],
    entry: &[String],
    source_root: &Utf8Path,
) -> Result<Gitignore, IgnoreRulesError> {
    let mut builder = GitignoreBuilder::new(source_root);
    // Set before any pattern is added. The builder folds case as it compiles
    // each line, and a later flip would leave the earlier lines case-sensitive.
    builder
        .case_insensitive(true)
        .map_err(|source| IgnoreRulesError::Build {
            source_root: source_root.to_string(),
            source: Box::new(source),
        })?;

    for pattern in repo_wide.iter().chain(entry) {
        builder
            .add_line(None, pattern)
            .map_err(|source| IgnoreRulesError::Pattern {
                pattern: pattern.clone(),
                source: Box::new(source),
            })?;
    }

    builder.build().map_err(|source| IgnoreRulesError::Build {
        source_root: source_root.to_string(),
        source: Box::new(source),
    })
}

/// The matcher for a walk that filters nothing.
///
/// The modes with no ignore list pass this. [`crate::walk_files`] says why it
/// takes a matcher rather than offering an unfiltered variant.
#[must_use]
pub fn none() -> Gitignore {
    Gitignore::empty()
}

/// Whether a [`crate::walk_files`] walk under `rules` drops `rel`, a path
/// relative to the walk root.
///
/// A caller that walks unfiltered asks this to learn which leaves the filtered
/// walk skipped. [`Gitignore::matched`] on the leaf alone is wrong here: a
/// `__pycache__/` pattern does not match `__pycache__/x.pyc`.
///
/// Ancestors decide first, top down, then the leaf, the same order the walk
/// prunes in. `["build/", "!build/keep.txt"]` therefore drops `build/keep.txt`,
/// because the walk never enters `build` to reach the negation.
/// [`Gitignore::matched_path_or_any_parents`] tests the leaf first and rescues
/// it. A caller using that one marks the leaf managed, the executor never
/// materializes it, and the target is never reaped.
#[must_use]
pub fn prunes(rules: &Gitignore, rel: &Utf8Path) -> bool {
    let mut prefix = Utf8PathBuf::new();
    let mut components = rel.components().peekable();
    while let Some(component) = components.next() {
        prefix.push(component);
        let is_dir = components.peek().is_some();
        if rules.matched(&prefix, is_dir).is_ignore() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(rules: &Gitignore, path: &str) -> bool {
        rules.matched(path, false).is_ignore()
    }

    #[test]
    fn an_entry_negation_overrides_a_repo_wide_pattern() {
        let rules = build(
            &["*.log".to_owned()],
            &["!keep.log".to_owned()],
            Utf8Path::new("/repo/mod/src"),
        )
        .expect("rules build");

        assert!(matches(&rules, "noise.log"));
        assert!(
            rules.matched("keep.log", false).is_whitelist(),
            "the entry list is added after the repo-wide one, so it wins"
        );
    }

    #[test]
    fn a_repo_wide_pattern_applies_to_an_entry_that_declares_none() {
        let rules = build(
            &[".DS_Store".to_owned()],
            &[],
            Utf8Path::new("/repo/mod/src"),
        )
        .expect("rules build");

        assert!(matches(&rules, ".DS_Store"));
        assert!(matches(&rules, "nested/.DS_Store"));
    }

    #[test]
    fn matching_folds_case() {
        let rules = build(
            &["thumbs.db".to_owned()],
            &["*.pyc".to_owned()],
            Utf8Path::new("/repo/mod/src"),
        )
        .expect("rules build");

        assert!(matches(&rules, "Thumbs.db"));
        assert!(matches(&rules, "FOO.PYC"));
    }

    #[test]
    fn a_trailing_slash_pattern_matches_a_directory_and_not_a_file() {
        let rules = build(
            &[],
            &["__pycache__/".to_owned()],
            Utf8Path::new("/repo/mod/src"),
        )
        .expect("rules build");

        assert!(rules.matched("__pycache__", true).is_ignore());
        assert!(
            !rules.matched("__pycache__", false).is_ignore(),
            "a trailing-slash pattern is directory-only, as in git"
        );
    }

    #[test]
    fn a_leading_slash_anchors_at_the_entry_source_not_the_repository() {
        let rules = build(&["/build".to_owned()], &[], Utf8Path::new("/repo/mod/src"))
            .expect("rules build");

        assert!(matches(&rules, "build"));
        assert!(
            !matches(&rules, "nested/build"),
            "an anchored pattern binds to the top of the entry's own source"
        );
    }

    #[test]
    fn a_malformed_glob_names_the_offending_pattern() {
        // A reversed character range is among the little the glob parser
        // rejects outright; an unclosed `[` is accepted as a literal.
        let err = build(
            &[],
            &["log[9-1].txt".to_owned()],
            Utf8Path::new("/repo/mod/src"),
        )
        .expect_err("a reversed character range is not a valid glob");

        assert!(
            matches!(err, IgnoreRulesError::Pattern { ref pattern, .. } if pattern == "log[9-1].txt"),
            "got {err:?}"
        );
    }

    #[test]
    fn prunes_drops_a_leaf_under_an_ignored_directory() {
        let rules = build(
            &[],
            &["__pycache__/".to_owned()],
            Utf8Path::new("/repo/mod/src"),
        )
        .expect("rules build");

        assert!(prunes(&rules, Utf8Path::new("__pycache__/mod.pyc")));
        assert!(prunes(&rules, Utf8Path::new("pkg/__pycache__/mod.pyc")));
        assert!(!prunes(&rules, Utf8Path::new("pkg/mod.py")));
    }

    #[test]
    fn prunes_lets_an_ancestor_outrank_a_leaf_negation() {
        let rules = build(
            &[],
            &["build/".to_owned(), "!build/keep.txt".to_owned()],
            Utf8Path::new("/repo/mod/src"),
        )
        .expect("rules build");

        assert!(prunes(&rules, Utf8Path::new("build/keep.txt")));
        assert!(
            rules
                .matched_path_or_any_parents("build/keep.txt", false)
                .is_whitelist(),
            "the disagreement is the point: `prunes` follows the walk order, this method \
             tests the leaf first"
        );
    }

    #[test]
    fn prunes_honours_a_negation_the_walk_can_reach() {
        let rules = build(
            &["*.log".to_owned()],
            &["!keep.log".to_owned()],
            Utf8Path::new("/repo/mod/src"),
        )
        .expect("rules build");

        assert!(prunes(&rules, Utf8Path::new("noise.log")));
        assert!(!prunes(&rules, Utf8Path::new("keep.log")));
    }

    #[test]
    fn the_empty_matcher_matches_nothing() {
        let rules = none();

        assert!(!matches(&rules, "anything.pyc"));
        assert!(!rules.matched("__pycache__", true).is_ignore());
    }
}
