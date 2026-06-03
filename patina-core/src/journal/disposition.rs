//! How a planned target relates to the live filesystem at plan time
//! (REQ-001, REQ-013).
//!
//! A [`Disposition`] is the three-way classification every managed target
//! resolves to: it is being **created** for the first time, **updated**
//! over existing content that differs from what Patina would write, or it
//! is already **unchanged** and needs no write at all. The skip-if-satisfied
//! engine carries this marker on both the durable [`Plan`](super::Plan) (via
//! [`PlannedOperation`](super::PlannedOperation)) and the committed
//! [`ApplyRecord`](super::ApplyRecord) (via
//! [`ExpectedTarget`](super::ExpectedTarget)) so a re-apply, a crash
//! recovery, and a rollback all agree on which targets to leave alone.

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
    /// This is the single mapping site for the three words: the same
    /// values become the `--json` plan entry `state` field, so the human
    /// diff, the machine output, and any future surface read from here
    /// rather than re-spelling the `match`.
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
        // The single mapping site every label reader (--json state, the
        // human diff) depends on. Each variant maps to its own distinct
        // word, so a swapped or shared arm fails here.
        assert_eq!(Disposition::Create.label(), "create");
        assert_eq!(Disposition::Update.label(), "update");
        assert_eq!(Disposition::Unchanged.label(), "unchanged");
    }
}
