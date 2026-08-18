//! How a planned target relates to the live filesystem at plan time.
//!
//! A [`Disposition`] classifies a managed target one of three ways. The
//! target is **created** for the first time, **updated** because existing
//! content differs from what Patina would write, or **unchanged** and needs
//! no write. The skip-if-satisfied
//! engine carries this marker in two places: the durable [`Plan`](super::Plan)
//! via [`PlannedOperation`](super::PlannedOperation), and the committed
//! [`ApplyRecord`](super::ApplyRecord) via
//! [`ExpectedTarget`](super::ExpectedTarget). A re-apply, a crash recovery,
//! and a rollback therefore all agree on which targets to leave alone.

use serde::Deserialize;
use serde::Serialize;

/// How a planned target relates to the live filesystem.
///
/// The variants ride the `postcard` wire on
/// [`PlannedOperation`](super::PlannedOperation) and
/// [`ExpectedTarget`](super::ExpectedTarget), so adding or reordering them
/// is an on-disk format change.
///
/// # Examples
///
/// ```
/// use patina_core::Disposition;
///
/// assert_eq!(Disposition::Create.label(), "create");
/// assert_eq!(Disposition::Update.label(), "update");
/// assert_eq!(Disposition::Unchanged.label(), "unchanged");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    /// The target does not yet exist; applying creates it.
    Create,
    /// The target exists but differs from what Patina would write;
    /// applying overwrites it.
    Update,
    /// The target already matches what Patina would write; applying
    /// leaves it untouched.
    Unchanged,
}

impl Disposition {
    /// The stable lowercase word for this disposition.
    ///
    /// Map each disposition to the word used by the `--json` plan entry's
    /// `state` field. The human diff and machine output read from here rather
    /// than re-spelling the `match`.
    ///
    /// # Examples
    ///
    /// ```
    /// use patina_core::Disposition;
    ///
    /// assert_eq!(Disposition::Unchanged.label(), "unchanged");
    /// ```
    #[must_use = "the label is the stable wire/JSON word for this disposition"]
    pub fn label(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Unchanged => "unchanged",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_maps_each_variant_to_its_stable_word() {
        // This mapping is the single source every label reader uses: the
        // --json state field and the human diff. Each variant maps to a
        // distinct word, so a swapped or shared arm fails this test.
        assert_eq!(Disposition::Create.label(), "create");
        assert_eq!(Disposition::Update.label(), "update");
        assert_eq!(Disposition::Unchanged.label(), "unchanged");
    }
}
