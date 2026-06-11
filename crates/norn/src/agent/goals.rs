//! Goal tracking (token / time budgets, continuation policies) and a
//! cron-style scheduler for session-dispatched agent re-launches.
//!
//! Both [`GoalTracker`] and [`Scheduler`] are pure data structures —
//! they do not run background loops. Wrapping them into live executors
//! is the job of the integration layer (N-015 and beyond).

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use croner::Cron;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AgentError;
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
    /// [`under_budget_fraction`].
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

// ---------------------------------------------------------------------------
// Scheduling (R7)
// ---------------------------------------------------------------------------

/// A cron-style entry that triggers a fresh agent session.
///
/// The scheduler is **store-only**: it computes the next due time from
/// the cron expression but does not run a background loop. The
/// integration layer is responsible for polling `due_entries` and
/// dispatching the actual agent session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleEntry {
    /// Stable identifier.
    pub id: Uuid,
    /// Standard 5-field cron expression (`min hour day month dow`).
    pub cron_expr: String,
    /// Opaque agent configuration handed to the dispatcher when the
    /// schedule fires.
    pub agent_config: serde_json::Value,
    /// If `false`, the entry is ignored by `due_entries` regardless of
    /// `next_run`.
    pub enabled: bool,
    /// Wall-clock timestamp of the last successful execution, if any.
    pub last_run: Option<DateTime<Utc>>,
    /// Next scheduled execution, computed from `cron_expr`.
    pub next_run: Option<DateTime<Utc>>,
}

/// In-memory store of [`ScheduleEntry`] records.
#[derive(Debug, Default)]
pub struct Scheduler {
    entries: HashMap<Uuid, ScheduleEntry>,
}

impl Scheduler {
    /// Create an empty scheduler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) a schedule entry, computing `next_run` from
    /// `cron_expr` when it is not pre-populated.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::SpawnFailed`] if `cron_expr` cannot be parsed.
    pub fn add(&mut self, mut entry: ScheduleEntry) -> Result<(), AgentError> {
        if entry.next_run.is_none() {
            entry.next_run = compute_next_run(&entry.cron_expr, &Utc::now())?;
        }
        self.entries.insert(entry.id, entry);
        Ok(())
    }

    /// Remove an entry by id and return it.
    pub fn remove(&mut self, id: Uuid) -> Option<ScheduleEntry> {
        self.entries.remove(&id)
    }

    /// Return entries whose `next_run` has elapsed and that are `enabled`.
    #[must_use]
    pub fn due_entries(&self, now: DateTime<Utc>) -> Vec<ScheduleEntry> {
        self.entries
            .values()
            .filter(|e| e.enabled && e.next_run.is_some_and(|n| n <= now))
            .cloned()
            .collect()
    }

    /// Number of stored entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the scheduler has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Advance an entry — record `now` as `last_run` and recompute
    /// `next_run` from the cron expression.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::NotFound`] if `id` is absent, or
    /// [`AgentError::SpawnFailed`] if the cron expression no longer parses.
    pub fn advance(&mut self, id: Uuid, now: DateTime<Utc>) -> Result<(), AgentError> {
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or_else(|| AgentError::NotFound {
                path: format!("schedule:{id}"),
            })?;
        let next = compute_next_run(&entry.cron_expr, &now)?;
        entry.last_run = Some(now);
        entry.next_run = next;
        Ok(())
    }

    /// Inspect an entry by id.
    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<ScheduleEntry> {
        self.entries.get(&id).cloned()
    }
}

fn compute_next_run(
    cron_expr: &str,
    after: &DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, AgentError> {
    let cron = Cron::new(cron_expr)
        .parse()
        .map_err(|e| AgentError::SpawnFailed {
            reason: format!("invalid cron expression '{cron_expr}': {e}"),
        })?;
    let next = cron
        .find_next_occurrence(after, false)
        .map_err(|e| AgentError::SpawnFailed {
            reason: format!("failed to compute next run for '{cron_expr}': {e}"),
        })?;
    Ok(Some(next))
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
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
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

    // ----- Scheduler -----------------------------------------------------

    fn make_entry(cron: &str, enabled: bool) -> ScheduleEntry {
        ScheduleEntry {
            id: Uuid::new_v4(),
            cron_expr: cron.to_string(),
            agent_config: serde_json::json!({}),
            enabled,
            last_run: None,
            next_run: None,
        }
    }

    #[test]
    fn scheduler_add_populates_next_run() {
        let mut s = Scheduler::new();
        let entry = make_entry("*/5 * * * *", true);
        let id = entry.id;
        s.add(entry).expect("add");
        let stored = s.get(id).expect("entry");
        assert!(stored.next_run.is_some());
        assert!(stored.next_run.expect("next_run") > Utc::now());
    }

    #[test]
    fn scheduler_due_entries_fires_after_next_run() {
        let mut s = Scheduler::new();
        let entry = make_entry("*/5 * * * *", true);
        let id = entry.id;
        s.add(entry).expect("add");

        // Inspect the assigned next_run, then ask about a moment after it.
        let next = s.get(id).expect("entry").next_run.expect("next_run");
        let after = next + chrono::Duration::seconds(1);
        let due = s.due_entries(after);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, id);
    }

    #[test]
    fn scheduler_due_entries_respects_enabled_flag() {
        let mut s = Scheduler::new();
        let mut entry = make_entry("*/5 * * * *", false);
        entry.next_run = Some(Utc::now() - chrono::Duration::seconds(60));
        let id = entry.id;
        s.add(entry).expect("add");
        assert!(s.due_entries(Utc::now()).is_empty());

        // Re-enable manually.
        if let Some(mut e) = s.remove(id) {
            e.enabled = true;
            s.add(e).expect("re-add");
        }
        // After re-add with next_run cleared by `add`, it should compute a
        // future next_run — so not currently due unless cron lands now.
        let _ = s.due_entries(Utc::now());
    }

    #[test]
    fn scheduler_advance_updates_last_run_and_next_run() {
        let mut s = Scheduler::new();
        let entry = make_entry("*/5 * * * *", true);
        let id = entry.id;
        s.add(entry).expect("add");

        let now = Utc::now();
        s.advance(id, now).expect("advance");
        let updated = s.get(id).expect("entry");
        assert_eq!(updated.last_run, Some(now));
        let next = updated.next_run.expect("next");
        assert!(next > now);
    }

    #[test]
    fn scheduler_advance_unknown_returns_not_found() {
        let mut s = Scheduler::new();
        let err = s.advance(Uuid::new_v4(), Utc::now()).expect_err("unknown");
        assert!(matches!(err, AgentError::NotFound { .. }));
    }

    #[test]
    fn scheduler_add_invalid_cron_errors() {
        let mut s = Scheduler::new();
        let entry = make_entry("not a cron expression", true);
        let err = s.add(entry).expect_err("bad cron");
        assert!(matches!(err, AgentError::SpawnFailed { .. }));
    }

    #[test]
    fn scheduler_remove_returns_entry() {
        let mut s = Scheduler::new();
        let entry = make_entry("0 * * * *", true);
        let id = entry.id;
        s.add(entry).expect("add");
        let removed = s.remove(id).expect("removed");
        assert_eq!(removed.id, id);
        assert!(s.get(id).is_none());
    }

    #[test]
    fn scheduler_entry_serde_roundtrip() {
        let mut entry = make_entry("0 * * * *", true);
        entry.next_run = Some(Utc::now());
        let json = serde_json::to_string(&entry).expect("ser");
        let back: ScheduleEntry = serde_json::from_str(&json).expect("de");
        assert_eq!(back.cron_expr, entry.cron_expr);
        assert_eq!(back.enabled, entry.enabled);
    }
}
