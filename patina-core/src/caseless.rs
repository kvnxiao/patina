//! One spelling for a name that two filesystems (or two authors) may spell
//! differently.
//!
//! Two consumers need the same answer. The target-collision check must reach
//! the same verdict on Linux as on the case-insensitive filesystems macOS and
//! Windows ship by default. The root manifest's remote registry turns names
//! into directory names under the per-machine cache. Folding both
//! through one function is what keeps a manifest's meaning a property of the
//! manifest rather than of the machine reading it.

use unicode_normalization::UnicodeNormalization;

/// Fold `value` to one case and one Unicode normal form.
///
/// Normalizing on both sides of the case mapping is Unicode's canonical
/// caseless match: the mapping can leave its own output unnormalized, so a
/// single trailing pass would not converge. Lowercase mapping rather than true
/// case folding, because APFS applies simple case folding and likewise leaves
/// `ß` alone; full folding maps it to `ss` and merges two names macOS keeps
/// apart.
#[must_use = "the folded form is the comparison key, not a display string"]
pub fn fold(value: &str) -> String {
    // ASCII has no decompositions, so the tables cannot change it.
    if value.is_ascii() {
        return value.to_lowercase();
    }
    let normalized: String = value.nfc().collect();
    normalized.to_lowercase().nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_only_differences_fold_together() {
        assert_eq!(fold("Config.TOML"), fold("config.toml"));
    }

    #[test]
    fn normalization_only_differences_fold_together() {
        // `café` precomposed against `e` plus a combining acute.
        assert_eq!(fold("caf\u{e9}"), fold("cafe\u{301}"));
    }

    #[test]
    fn case_and_normalization_fold_together() {
        // The uppercase precomposed form needs both passes to reach the
        // lowercase decomposed one: a single trailing normalization would not.
        assert_eq!(fold("CAF\u{c9}"), fold("cafe\u{301}"));
    }

    #[test]
    fn sharp_s_stays_distinct_from_its_two_letter_spelling() {
        assert_ne!(fold("stra\u{df}e"), fold("strasse"));
    }
}
