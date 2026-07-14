//! Goal tracking: token / time budgets and continuation policies.
//!
//! [`GoalTracker`] is a pure data structure — it does not run a background
//! loop. Wrapping it into a live executor is the job of the integration
//! layer. In-session scheduling (relative wake-ups, time-of-day, looping
//! intervals, cron expressions) lives in [`crate::schedule`], not here.

use std::time::Duration;

use crate::provider::Usage;

// ---------------------------------------------------------------------------
// Goal tracking (R6)
// ---------------------------------------------------------------------------

/// What an agent should do when it exhausts a budget.
#[derive(Clone, Debug, PartialEq)]
pub enum ContinuationPolicy {
    /// Terminate the agent immediately.
    Stop,
    /// Emit a handoff summary using the supplied prompt and then stop.
    Handoff {
        /// Prompt to use when generating the handoff summary.
        summary_prompt: String,
    },
    /// Continue running. If `recapitulate` is `true`, the agent first
    /// reproduces a brief progress recap before proceeding.
    Continue {
        /// Whether to emit a progress recap before continuing.
        recapitulate: bool,
    },
}

/// A single goal with optional token / time budgets and a continuation
/// policy describing what to do when a budget is breached.
#[derive(Clone, Debug)]
pub struct Goal {
    /// Free-form description of the goal.
    pub description: String,
    /// Total-token budget for the goal, if any.
    pub token_budget: Option<u64>,
    /// Wall-clock budget for the goal, if any.
    pub time_budget: Option<Duration>,
    /// What to do once a budget is exhausted.
    pub continuation: ContinuationPolicy,
}

/// Signal produced by [`GoalTracker::check`].
#[derive(Clone, Debug)]
pub enum GoalSignal {
    /// Budgets are under the warning threshold.
    OnTrack,
    /// Usage has crossed the warning threshold but remains under the budget.
    BudgetWarning {
        /// Fraction of the maximum budget consumed, in `[warning_threshold, 1.0)`.
        pct_used: f64,
    },
    /// Budget exhausted — the continuation policy should be enacted.
    BudgetExceeded {
        /// The configured continuation policy.
        policy: ContinuationPolicy,
    },
}

/// Track usage against a [`Goal`] and emit [`GoalSignal`]s.
#[derive(Clone, Debug)]
pub struct GoalTracker {
    goal: Goal,
    warning_threshold: f64,
}

impl GoalTracker {
    /// Default warning threshold (80% of budget).
    pub const DEFAULT_WARNING_THRESHOLD: f64 = 0.80;

    /// Construct a tracker with the default 80% warning threshold.
    #[must_use]
    pub fn new(goal: Goal) -> Self {
        Self {
            goal,
            warning_threshold: Self::DEFAULT_WARNING_THRESHOLD,
        }
    }

    /// Override the warning threshold. Values are clamped to `[0.0, 1.0]`.
    #[must_use]
    pub fn with_warning_threshold(mut self, threshold: f64) -> Self {
        self.warning_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Inspect the underlying [`Goal`].
    #[must_use]
    pub fn goal(&self) -> &Goal {
        &self.goal
    }

    /// Inspect the current warning threshold.
    #[must_use]
    pub fn warning_threshold(&self) -> f64 {
        self.warning_threshold
    }

    /// Evaluate the goal against the supplied usage and elapsed time.
    ///
    /// Combined budget percentage uses `max(token_pct, time_pct)`.
    /// Token consumption is computed as `input_tokens + output_tokens`
    /// (cache-only tokens are excluded so the warning fires on chargeable
    /// throughput, not pre-warmed context). Budget exhaustion is decided
    /// by exact integer comparison (`used >= budget`); the fractional
    /// percentage is only computed for the under-budget warning band, via
    /// `under_budget_fraction`.
    #[must_use]
    pub fn check(&self, usage: &Usage, elapsed: Duration) -> GoalSignal {
        let token_pct = self.goal.token_budget.map_or(0.0_f64, |budget| {
            if budget == 0 {
                return 0.0;
            }
            let used = usage.input_tokens.saturating_add(usage.output_tokens);
            if used >= budget {
                1.0
            } else {
                under_budget_fraction(used, budget)
            }
        });
        let time_pct = self.goal.time_budget.map_or(0.0_f64, |budget| {
            if budget.is_zero() {
                return 0.0;
            }
            elapsed.as_secs_f64() / budget.as_secs_f64()
        });
        let pct = token_pct.max(time_pct);

        if pct >= 1.0 {
            GoalSignal::BudgetExceeded {
                policy: self.goal.continuation.clone(),
            }
        } else if pct >= self.warning_threshold {
            GoalSignal::BudgetWarning { pct_used: pct }
        } else {
            GoalSignal::OnTrack
        }
    }
}

/// Fraction `used / budget` for the under-budget warning band, computed
/// without lossy numeric casts.
///
/// Callers guarantee `used < budget` (and therefore `budget > 0`) and
/// handle the at-or-over-budget
/// case with an exact integer comparison before calling. Both operands are
/// reduced by the same power-of-two shift until the budget fits in `u32`,
/// then converted through the lossless `u32 -> f64` [`From`] impl:
///
/// - for budgets below 2^32 the shift is zero and the result is the
///   correctly rounded `f64` quotient — exact integer inputs, one rounded
///   division;
/// - for larger budgets the floor-shift discards equal low-order bits from
///   both operands, bounding the absolute error of the quotient by 2^-31
///   (i.e. at budget scale; a tiny numerator against a huge budget may lose
///   all its bits and floor to 0.0, which is fine for the threshold
///   comparison this feeds) — documented precision, never silent wrap or
///   truncation.
fn under_budget_fraction(used: u64, budget: u64) -> f64 {
    let significant_bits = 64 - budget.leading_zeros();
    let shift = significant_bits.saturating_sub(32);
    match (u32::try_from(used >> shift), u32::try_from(budget >> shift)) {
        (Ok(used_reduced), Ok(budget_reduced)) if budget_reduced > 0 => {
            f64::from(used_reduced) / f64::from(budget_reduced)
        }
        // Unreachable by construction: `budget >> shift` has at most 32
        // significant bits (and at least one, since `budget > used >= 0`
        // implies `budget > 0`), and `used < budget` keeps the reduced
        // numerator within u32 as well. Saturate to the budget boundary
        // rather than panic so a logic regression surfaces as an early
        // budget signal instead of an abort.
        _ => 1.0,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    // ----- GoalTracker ---------------------------------------------------

    fn usage_with_tokens(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        }
    }

    #[test]
    fn goal_tracker_under_threshold_is_on_track() {
        let tracker = GoalTracker::new(Goal {
            description: "compose reply".to_string(),
            token_budget: Some(1000),
            time_budget: None,
            continuation: ContinuationPolicy::Stop,
        });
        let signal = tracker.check(&usage_with_tokens(100, 0), Duration::ZERO);
        assert!(matches!(signal, GoalSignal::OnTrack));
    }

    #[test]
    fn goal_tracker_warning_at_80pct() {
        let tracker = GoalTracker::new(Goal {
            description: "compose reply".to_string(),
            token_budget: Some(1000),
            time_budget: None,
            continuation: ContinuationPolicy::Stop,
        });
        let signal = tracker.check(&usage_with_tokens(800, 0), Duration::ZERO);
        let GoalSignal::BudgetWarning { pct_used } = signal else {
            panic!("expected BudgetWarning");
        };
        assert!(
            (0.79..=0.81).contains(&pct_used),
            "expected ~0.8, got {pct_used}"
        );
    }

    #[test]
    fn goal_tracker_exceeded_carries_policy() {
        let policy = ContinuationPolicy::Handoff {
            summary_prompt: "wrap up please".to_string(),
        };
        let tracker = GoalTracker::new(Goal {
            description: "long task".to_string(),
            token_budget: Some(1000),
            time_budget: None,
            continuation: policy.clone(),
        });
        let signal = tracker.check(&usage_with_tokens(1100, 0), Duration::ZERO);
        let GoalSignal::BudgetExceeded { policy: out_policy } = signal else {
            panic!("expected BudgetExceeded");
        };
        assert_eq!(out_policy, policy);
    }

    #[test]
    fn goal_tracker_time_budget_dominates_when_token_under() {
        let tracker = GoalTracker::new(Goal {
            description: "timed".to_string(),
            token_budget: Some(1_000_000),
            time_budget: Some(Duration::from_secs(10)),
            continuation: ContinuationPolicy::Continue { recapitulate: true },
        });
        let signal = tracker.check(&usage_with_tokens(1, 1), Duration::from_secs(11));
        assert!(matches!(signal, GoalSignal::BudgetExceeded { .. }));
    }

    #[test]
    fn goal_tracker_custom_threshold() {
        let tracker = GoalTracker::new(Goal {
            description: "task".to_string(),
            token_budget: Some(1000),
            time_budget: None,
            continuation: ContinuationPolicy::Stop,
        })
        .with_warning_threshold(0.5);
        let signal = tracker.check(&usage_with_tokens(500, 0), Duration::ZERO);
        assert!(matches!(signal, GoalSignal::BudgetWarning { .. }));
    }

    #[test]
    fn goal_tracker_no_budget_is_on_track() {
        let tracker = GoalTracker::new(Goal {
            description: "unbounded".to_string(),
            token_budget: None,
            time_budget: None,
            continuation: ContinuationPolicy::Stop,
        });
        let signal = tracker.check(
            &usage_with_tokens(1_000_000, 1_000_000),
            Duration::from_secs(3600),
        );
        assert!(matches!(signal, GoalSignal::OnTrack));
    }

    #[test]
    fn goal_tracker_ratio_just_below_threshold_stays_on_track() {
        // 800_000_005 / 1_000_000_007 ≈ 0.799999998 — strictly below the
        // 0.80 threshold. The old `u64 as f32` casts rounded both operands
        // (f32 ulp is 64 at this magnitude) and emitted a spurious warning
        // here; the exact integer-backed ratio must stay on track.
        let tracker = GoalTracker::new(Goal {
            description: "precise".to_string(),
            token_budget: Some(1_000_000_007),
            time_budget: None,
            continuation: ContinuationPolicy::Stop,
        });
        let signal = tracker.check(&usage_with_tokens(800_000_005, 0), Duration::ZERO);
        assert!(matches!(signal, GoalSignal::OnTrack), "got {signal:?}");
    }

    #[test]
    fn goal_tracker_exceeded_exactly_at_budget() {
        // used == budget is decided by integer comparison, not float
        // rounding, so the boundary itself is exact.
        let tracker = GoalTracker::new(Goal {
            description: "boundary".to_string(),
            token_budget: Some(1_000),
            time_budget: None,
            continuation: ContinuationPolicy::Stop,
        });
        let signal = tracker.check(&usage_with_tokens(1_000, 0), Duration::ZERO);
        assert!(matches!(signal, GoalSignal::BudgetExceeded { .. }));
    }

    #[test]
    fn goal_tracker_huge_budget_warning_band_is_accurate() {
        // Budgets above 2^32 take the shift-reduction path in
        // under_budget_fraction; the relative error is bounded by 2^-31,
        // so an 85% ratio must surface as a warning with pct ≈ 0.85.
        let budget = u64::MAX;
        let used = u64::MAX / 20 * 17; // ≈ 85% of budget
        let tracker = GoalTracker::new(Goal {
            description: "huge".to_string(),
            token_budget: Some(budget),
            time_budget: None,
            continuation: ContinuationPolicy::Stop,
        });
        let signal = tracker.check(&usage_with_tokens(used, 0), Duration::ZERO);
        let GoalSignal::BudgetWarning { pct_used } = signal else {
            panic!("expected BudgetWarning, got {signal:?}");
        };
        assert!(
            (pct_used - 0.85).abs() < 1e-6,
            "expected ~0.85, got {pct_used}"
        );
    }

    #[test]
    fn goal_tracker_token_sum_saturates_instead_of_wrapping() {
        // input + output saturates at u64::MAX rather than wrapping to a
        // small number that would mask exhaustion.
        let tracker = GoalTracker::new(Goal {
            description: "saturate".to_string(),
            token_budget: Some(1_000),
            time_budget: None,
            continuation: ContinuationPolicy::Stop,
        });
        let signal = tracker.check(&usage_with_tokens(u64::MAX, u64::MAX), Duration::ZERO);
        assert!(matches!(signal, GoalSignal::BudgetExceeded { .. }));
    }

    #[test]
    fn under_budget_fraction_exact_below_two_pow_32() {
        assert!((under_budget_fraction(800, 1_000) - 0.8).abs() < f64::EPSILON);
        assert!(under_budget_fraction(1, u64::from(u32::MAX)) > 0.0);
        assert!(under_budget_fraction(0, 1_000) < f64::EPSILON);
    }

    #[test]
    fn under_budget_fraction_shift_path_bounds_error() {
        // Budget above 2^32: both operands are floor-shifted equally, so
        // the result is within 2^-31 of the true ratio. With both operands
        // divisible by the shift amount the result is exact.
        let budget = 5_u64 << 38;
        let used = 4_u64 << 38; // exactly 80%, low bits zero
        assert!((under_budget_fraction(used, budget) - 0.8).abs() < f64::EPSILON);

        // One token below a huge budget rounds to the boundary itself
        // (documented 2^-31 coarseness of the shift path).
        let near = under_budget_fraction(budget - 1, budget);
        assert!((near - 1.0).abs() < 1e-9, "got {near}");
    }
}
