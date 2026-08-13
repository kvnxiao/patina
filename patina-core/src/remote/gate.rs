//! The update gate: the four checks a candidate tip must clear before a pin
//! moves.
//!
//! Bumping a pin is the moment third-party code changes what lands on your
//! machines, so it is the moment Patina slows down. `docs/REMOTE_SOURCES.md`
//! "The update gate" specifies the four checks, their order, which ones reject
//! outright versus prompt, and what the gate cannot stop.
//!
//! [`evaluate`] is pure over [`GateInputs`], so every branch is testable
//! without a clock, a network, or a repository.

use crate::config::DEFAULT_MIN_AGE;
use crate::config::RemoteSpec;
use std::time::Duration;

/// How far ahead of the local clock a committer time may sit before it is
/// refused outright. An hour absorbs ordinary clock skew between machines
/// without accepting a timestamp from next week.
pub const FUTURE_TOLERANCE: Duration = Duration::from_hours(1);

/// The gate compares one candidate tip against one existing pin.
#[derive(Debug, Clone, Copy)]
pub struct GateInputs {
    /// The candidate tip's committer time, Unix seconds.
    pub candidate_epoch: i64,
    /// The local clock, Unix seconds.
    pub now_epoch: i64,
    /// Whether the candidate descends from the pinned rev. `None` when there is
    /// no pin yet, so there is no history to have been rewritten.
    pub descends_from_pin: Option<bool>,
    /// The existing pin's `updated_at`, Unix seconds. `None` for a first pin,
    /// or when the recorded value could not be read as a timestamp.
    pub pinned_updated_at: Option<i64>,
    /// The effective age floor for this remote (see [`effective_min_age`]).
    pub min_age: Duration,
    /// Whether this would be the remote's first pin. Adopting a remote is a
    /// deliberate act. The user reviews its content in the consent diff, so
    /// it is exempt from the age gate. The gate exists to slow down
    /// unattended bumps.
    pub first_pin: bool,
    /// `--now`: skip the age gate for this run only. Every other check still
    /// applies.
    pub bypass_age: bool,
}

/// A check that tripped but is recoverable with explicit confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GateConcern {
    /// The candidate does not descend from the pinned rev: upstream history was
    /// rewritten (a force-push), not merely extended.
    HistoryRewritten,
    /// The candidate's committer time predates the moment the current pin was
    /// recorded.
    Backdated {
        /// The candidate's committer time, Unix seconds.
        candidate_epoch: i64,
        /// The pin's `updated_at`, Unix seconds.
        pinned_updated_at: i64,
    },
}

impl GateConcern {
    /// A one-line description for the confirmation prompt.
    #[must_use = "the description is what the user is asked to confirm"]
    pub fn describe(self) -> String {
        match self {
            Self::HistoryRewritten => "upstream history was rewritten: the candidate commit is \
                 not a descendant of the pinned rev"
                .to_owned(),
            Self::Backdated { .. } => "the candidate commit is dated earlier than the moment the \
                 current pin was recorded"
                .to_owned(),
        }
    }
}

/// The gate's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GateOutcome {
    /// The candidate is already the pinned rev; there is nothing to bump.
    AlreadyPinned,
    /// Every check passed. Bump the pin.
    Allowed,
    /// Confirm before bumping. Concerns are listed in evaluation order.
    NeedsConfirmation(Vec<GateConcern>),
    /// A hard reject: the committer time is implausibly far in the future.
    RejectedFuture {
        /// The candidate's committer time, Unix seconds.
        candidate_epoch: i64,
        /// The local clock at evaluation, Unix seconds.
        now_epoch: i64,
    },
    /// The candidate is younger than the age floor. The pin is left unchanged
    /// and the run proceeds.
    Cooldown {
        /// When the candidate becomes eligible, Unix seconds.
        eligible_at: i64,
    },
}

/// The age floor in force for one remote: its own `min_age`, else the root
/// `[remotes] min_age`, else the shipped 72 hours.
#[must_use = "the effective floor is what the age gate compares against"]
pub fn effective_min_age(spec: &RemoteSpec, global: Option<Duration>) -> Duration {
    spec.min_age.or(global).unwrap_or(DEFAULT_MIN_AGE)
}

/// Run the four checks in order.
///
/// A candidate that fails the age gate reports [`GateOutcome::Cooldown`], even
/// when an earlier check also raised a concern. The pin is not moving either
/// way, and re-reporting a rewrite the user cannot yet act on would be noise.
/// The concerns surface on the run where the candidate is actually eligible.
#[must_use = "the outcome decides whether the pin moves"]
pub fn evaluate(inputs: GateInputs) -> GateOutcome {
    // The future check runs first because a nonsensical timestamp makes every
    // later comparison meaningless.
    let tolerance = i64::try_from(FUTURE_TOLERANCE.as_secs()).unwrap_or(i64::MAX);
    if inputs.candidate_epoch.saturating_sub(inputs.now_epoch) > tolerance {
        return GateOutcome::RejectedFuture {
            candidate_epoch: inputs.candidate_epoch,
            now_epoch: inputs.now_epoch,
        };
    }

    let mut concerns = Vec::new();

    if inputs.descends_from_pin == Some(false) {
        concerns.push(GateConcern::HistoryRewritten);
    }

    if let Some(pinned_updated_at) = inputs.pinned_updated_at
        && inputs.candidate_epoch < pinned_updated_at
    {
        concerns.push(GateConcern::Backdated {
            candidate_epoch: inputs.candidate_epoch,
            pinned_updated_at,
        });
    }

    if !inputs.first_pin && !inputs.bypass_age {
        let floor = i64::try_from(inputs.min_age.as_secs()).unwrap_or(i64::MAX);
        let eligible_at = inputs.candidate_epoch.saturating_add(floor);
        if eligible_at > inputs.now_epoch {
            return GateOutcome::Cooldown { eligible_at };
        }
    }

    if concerns.is_empty() {
        GateOutcome::Allowed
    } else {
        GateOutcome::NeedsConfirmation(concerns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A candidate that clears every check: a week old, descended from the pin,
    /// newer than the pin's `updated_at`, under a 72-hour floor.
    const NOW: i64 = 1_800_000_000;
    const WEEK: i64 = 7 * 24 * 60 * 60;

    fn clean() -> GateInputs {
        GateInputs {
            candidate_epoch: NOW - WEEK,
            now_epoch: NOW,
            descends_from_pin: Some(true),
            pinned_updated_at: Some(NOW - 2 * WEEK),
            min_age: DEFAULT_MIN_AGE,
            first_pin: false,
            bypass_age: false,
        }
    }

    #[test]
    fn a_clean_candidate_is_allowed() {
        assert_eq!(evaluate(clean()), GateOutcome::Allowed);
    }

    #[test]
    fn a_committer_time_far_in_the_future_is_rejected() {
        let inputs = GateInputs {
            candidate_epoch: NOW + 2 * 60 * 60,
            ..clean()
        };
        assert!(matches!(
            evaluate(inputs),
            GateOutcome::RejectedFuture { .. }
        ));
    }

    #[test]
    fn a_committer_time_inside_the_skew_tolerance_is_not_rejected() {
        // The guard for the check above: an hour of clock skew between the
        // committer's machine and this one is ordinary, not an attack.
        let inputs = GateInputs {
            candidate_epoch: NOW + 30 * 60,
            first_pin: true,
            ..clean()
        };
        assert_eq!(
            evaluate(inputs),
            GateOutcome::Allowed,
            "half an hour ahead must pass the future check"
        );
    }

    #[test]
    fn a_rewritten_history_needs_confirmation() {
        let inputs = GateInputs {
            descends_from_pin: Some(false),
            ..clean()
        };
        assert_eq!(
            evaluate(inputs),
            GateOutcome::NeedsConfirmation(vec![GateConcern::HistoryRewritten])
        );
    }

    #[test]
    fn a_candidate_dated_before_the_pin_was_recorded_needs_confirmation() {
        let inputs = GateInputs {
            candidate_epoch: NOW - 3 * WEEK,
            pinned_updated_at: Some(NOW - 2 * WEEK),
            ..clean()
        };
        assert_eq!(
            evaluate(inputs),
            GateOutcome::NeedsConfirmation(vec![GateConcern::Backdated {
                candidate_epoch: NOW - 3 * WEEK,
                pinned_updated_at: NOW - 2 * WEEK,
            }])
        );
    }

    #[test]
    fn a_candidate_dated_exactly_at_the_pin_timestamp_is_not_backdated() {
        // The backdating comparison is strict. A commit made in the same
        // second the pin was recorded is not evidence of a rollback.
        // Prompting on it would put a confirmation in front of an ordinary
        // re-pin.
        assert_eq!(
            evaluate(GateInputs {
                candidate_epoch: NOW - 2 * WEEK,
                pinned_updated_at: Some(NOW - 2 * WEEK),
                ..clean()
            }),
            GateOutcome::Allowed
        );
    }

    #[test]
    fn both_recoverable_concerns_are_reported_in_evaluation_order() {
        let inputs = GateInputs {
            candidate_epoch: NOW - 3 * WEEK,
            descends_from_pin: Some(false),
            pinned_updated_at: Some(NOW - 2 * WEEK),
            ..clean()
        };
        assert_eq!(
            evaluate(inputs),
            GateOutcome::NeedsConfirmation(vec![
                GateConcern::HistoryRewritten,
                GateConcern::Backdated {
                    candidate_epoch: NOW - 3 * WEEK,
                    pinned_updated_at: NOW - 2 * WEEK,
                },
            ])
        );
    }

    #[test]
    fn a_candidate_younger_than_the_floor_is_held_and_reports_when_it_is_eligible() {
        let inputs = GateInputs {
            candidate_epoch: NOW - 60 * 60,
            ..clean()
        };
        assert_eq!(
            evaluate(inputs),
            GateOutcome::Cooldown {
                eligible_at: NOW - 60 * 60 + 72 * 60 * 60,
            }
        );
    }

    #[test]
    fn a_candidate_exactly_at_the_floor_is_eligible() {
        let inputs = GateInputs {
            candidate_epoch: NOW - 72 * 60 * 60,
            ..clean()
        };
        assert_eq!(evaluate(inputs), GateOutcome::Allowed);
    }

    #[test]
    fn the_first_pin_is_exempt_from_the_age_gate() {
        let inputs = GateInputs {
            candidate_epoch: NOW - 60,
            descends_from_pin: None,
            pinned_updated_at: None,
            first_pin: true,
            ..clean()
        };
        assert_eq!(
            evaluate(inputs),
            GateOutcome::Allowed,
            "adopting a remote is a deliberate act reviewed in the consent diff"
        );
    }

    #[test]
    fn now_bypasses_the_age_gate_but_not_the_other_checks() {
        let young_and_rewritten = GateInputs {
            candidate_epoch: NOW - 60,
            descends_from_pin: Some(false),
            bypass_age: true,
            ..clean()
        };
        assert_eq!(
            evaluate(young_and_rewritten),
            GateOutcome::NeedsConfirmation(vec![GateConcern::HistoryRewritten]),
            "`--now` waives age only; the rewrite must still be confirmed"
        );

        let young_and_future = GateInputs {
            candidate_epoch: NOW + 10 * 60 * 60,
            bypass_age: true,
            ..clean()
        };
        assert!(
            matches!(
                evaluate(young_and_future),
                GateOutcome::RejectedFuture { .. }
            ),
            "`--now` must not waive the future check"
        );
    }

    #[test]
    fn a_per_remote_floor_overrides_the_global_one() {
        let spec = RemoteSpec {
            name: crate::config::RemoteName::parse("r").expect("a legal remote name"),
            url: "u".to_owned(),
            git_ref: None,
            min_age: Some(Duration::from_secs(0)),
        };
        assert_eq!(
            effective_min_age(&spec, Some(Duration::from_hours(24))),
            Duration::from_secs(0)
        );
    }

    #[test]
    fn the_global_floor_applies_when_the_remote_declares_none() {
        let spec = RemoteSpec {
            name: crate::config::RemoteName::parse("r").expect("a legal remote name"),
            url: "u".to_owned(),
            git_ref: None,
            min_age: None,
        };
        assert_eq!(
            effective_min_age(&spec, Some(Duration::from_hours(24))),
            Duration::from_hours(24)
        );
        assert_eq!(effective_min_age(&spec, None), DEFAULT_MIN_AGE);
    }

    #[test]
    fn a_zero_floor_admits_a_brand_new_commit() {
        let inputs = GateInputs {
            candidate_epoch: NOW,
            min_age: Duration::from_secs(0),
            ..clean()
        };
        assert_eq!(evaluate(inputs), GateOutcome::Allowed);
    }
}
