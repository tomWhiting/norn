//! Human phrasing for the provider-retry wait (retry-forever DESIGN D8,
//! commit C6).
//!
//! One formatter, used by every TUI retry surface: the streaming-status
//! row in the fixed panel and the per-agent activity column. Sharing it
//! keeps the two from drifting into different wordings for the same
//! event, and keeps the phrasing rules — which are presentation, not
//! policy — in one auditable place.
//!
//! ## Discipline
//!
//! `error_class` is the taxonomy label the engine put on the event
//! ([`AgentStreamRetry::error_class`](norn::provider::agent_event::AgentStreamRetry::error_class)):
//! a stable `snake_case` class such as `timeout` or `rate_limited`. It is
//! rendered verbatim and NEVER joined with provider free text — reasons
//! belong to the loud terminal error, not to an always-on progress
//! surface.
//!
//! An unbounded budget (`max_attempts: None`, the default policy) is
//! spelled out as the word `unbounded`; it is never rendered as a
//! sentinel number and never silently omitted, so a reader can always
//! tell "attempt 3 of 5" from "attempt 3, retrying until it succeeds".

use std::time::Duration;

use super::text::truncate_with_ellipsis;

/// Leading glyph of the retry-wait status row (U+27F3, clockwise gapped
/// circle arrow) — visually distinct from the generating dot at a
/// glance.
const RETRY_GLYPH: char = '\u{27f3}';

/// Prefix shared by every retry activity label.
///
/// The agent status panel uses it to recognise its own retry labels: a
/// retry label announces a wait that ends, so the next attempt's first
/// text/thinking delta is allowed to replace it, unlike a sticky tool
/// label.
pub const RETRY_ACTIVITY_PREFIX: &str = "retrying ";

/// Whole seconds a remaining wait is displayed as, rounded UP.
///
/// Rounding up keeps a live wait from displaying `0s` while it is still
/// running: only a wait that has actually elapsed reports zero. Callers
/// that gate repaints use this same value so the repaint key and the
/// rendered text can never disagree.
#[must_use]
pub fn retry_wait_secs(remaining: Duration) -> u64 {
    u64::try_from(remaining.as_millis().div_ceil(1_000)).unwrap_or(u64::MAX)
}

/// The wait phrase: `in 8s` while the backoff is still running, `now`
/// once it has elapsed and the attempt is being made.
fn wait_phrase(remaining: Duration) -> String {
    let secs = retry_wait_secs(remaining);
    if secs == 0 {
        "now".to_string()
    } else {
        format!("in {secs}s")
    }
}

/// The attempt-budget phrase: `attempt 3 of 5` when the policy is
/// bounded, `attempt 3, unbounded` when it retries until it succeeds or
/// the user cancels.
fn budget_phrase(attempt: u32, max_attempts: Option<u32>) -> String {
    match max_attempts {
        Some(max) => format!("attempt {attempt} of {max}"),
        None => format!("attempt {attempt}, unbounded"),
    }
}

/// The one retry label every TUI surface shows.
///
/// `remaining` is the wait still to run before the announced attempt —
/// the engine's sampled backoff at the moment the marker arrived, counted
/// down by the render tick.
///
/// Examples: `retrying in 8s (attempt 3 of 5, server_error)`,
/// `retrying in 2s (attempt 4, unbounded, rate_limited)`,
/// `retrying now (attempt 2, unbounded, timeout)`.
#[must_use]
pub fn retry_status_label(
    attempt: u32,
    max_attempts: Option<u32>,
    remaining: Duration,
    error_class: &str,
) -> String {
    format!(
        "{RETRY_ACTIVITY_PREFIX}{wait} ({budget}, {error_class})",
        wait = wait_phrase(remaining),
        budget = budget_phrase(attempt, max_attempts),
    )
}

/// The fixed panel's retry status row, fitted to `terminal_cols`.
///
/// The glyph plus the shared label; over-long terminals-narrow cases
/// truncate with the same single-codepoint ellipsis every other panel row
/// uses, so the row can never wrap and disturb the panel's height.
#[must_use]
pub fn retry_row_body(
    attempt: u32,
    max_attempts: Option<u32>,
    remaining: Duration,
    error_class: &str,
    terminal_cols: u16,
) -> String {
    let label = retry_status_label(attempt, max_attempts, remaining, error_class);
    truncate_with_ellipsis(&format!("{RETRY_GLYPH} {label}"), terminal_cols)
}

/// Repaint key for a retry row.
///
/// Keyed on the same whole-second countdown the row renders, so the wait
/// repaints once per second and never churns the panel on sub-second tick
/// noise.
#[must_use]
pub fn retry_repaint_key(
    attempt: u32,
    max_attempts: Option<u32>,
    remaining: Duration,
    error_class: &str,
    terminal_cols: u16,
) -> String {
    format!(
        "retrying:{attempt}:{max}:{error_class}:{secs}:{terminal_cols}",
        max = max_attempts.map_or_else(|| "unbounded".to_string(), |max| max.to_string()),
        secs = retry_wait_secs(remaining),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_policy_names_the_attempt_budget() {
        assert_eq!(
            retry_status_label(3, Some(5), Duration::from_secs(8), "server_error"),
            "retrying in 8s (attempt 3 of 5, server_error)",
        );
    }

    #[test]
    fn unbounded_policy_says_so_instead_of_showing_a_sentinel() {
        let label = retry_status_label(4, None, Duration::from_secs(2), "rate_limited");
        assert_eq!(label, "retrying in 2s (attempt 4, unbounded, rate_limited)");
        assert!(
            !label.contains("of 0") && !label.contains("of 4294967295"),
            "an unbounded budget must never be rendered as a number: {label}",
        );
    }

    /// A sub-second backoff still reads as a live wait, never as `0s`.
    #[test]
    fn sub_second_waits_round_up_to_one_second() {
        assert_eq!(
            retry_status_label(2, None, Duration::from_millis(1), "timeout"),
            "retrying in 1s (attempt 2, unbounded, timeout)",
        );
        assert_eq!(retry_wait_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_wait_secs(Duration::from_millis(1_001)), 2);
    }

    /// Once the wait has elapsed the row says the attempt is happening —
    /// it does not keep counting a wait that already ended.
    #[test]
    fn an_elapsed_wait_reads_as_now() {
        assert_eq!(
            retry_status_label(2, Some(3), Duration::ZERO, "connection_reset"),
            "retrying now (attempt 2 of 3, connection_reset)",
        );
        assert_eq!(retry_wait_secs(Duration::ZERO), 0);
    }

    /// The status row is the label behind the wait glyph, and it is
    /// fitted to the terminal so a narrow window cannot wrap it into a
    /// second row and disturb the panel height.
    #[test]
    fn the_row_body_carries_the_glyph_and_fits_the_width() {
        let body = retry_row_body(3, Some(5), Duration::from_secs(8), "server_error", 120);
        assert_eq!(body, "⟳ retrying in 8s (attempt 3 of 5, server_error)");

        let narrow = retry_row_body(3, Some(5), Duration::from_secs(8), "server_error", 12);
        assert!(narrow.chars().count() <= 12, "narrow: {narrow:?}");
    }

    /// The repaint key moves only with the whole-second countdown, and
    /// distinguishes bounded from unbounded budgets.
    #[test]
    fn the_repaint_key_tracks_whole_seconds_and_the_budget() {
        let key = |remaining_ms: u64, max: Option<u32>| {
            retry_repaint_key(2, max, Duration::from_millis(remaining_ms), "timeout", 80)
        };
        assert_eq!(key(7_100, None), key(7_999, None));
        assert_ne!(key(7_000, None), key(6_000, None));
        assert_ne!(key(7_000, None), key(7_000, Some(5)));
        assert!(key(7_000, None).contains("unbounded"));
    }

    /// Every label starts with the prefix the status panel keys its
    /// replaceability rule on.
    #[test]
    fn every_label_carries_the_shared_prefix() {
        for (attempt, max, wait) in [(2_u32, None, 0_u64), (9, Some(9), 30)] {
            let label = retry_status_label(attempt, max, Duration::from_secs(wait), "timeout");
            assert!(label.starts_with(RETRY_ACTIVITY_PREFIX), "label: {label}");
        }
    }

    /// The class is the engine's taxonomy label, rendered verbatim — the
    /// formatter neither rewrites it nor appends provider text.
    #[test]
    fn the_error_class_is_rendered_verbatim() {
        let label = retry_status_label(2, None, Duration::from_secs(1), "connection_reset");
        assert!(label.ends_with(", connection_reset)"), "label: {label}");
    }
}
