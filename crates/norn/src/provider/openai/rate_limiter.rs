//! Token-bucket rate limiter for provider request throttling.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

/// A token-bucket rate limiter with a server-imposed cooldown gate.
///
/// Permits are replenished at a fixed, configured interval. Callers
/// acquire a permit before making a request; if none is available the
/// call awaits asynchronously until the next replenishment.
///
/// A `429 Too Many Requests` response imposes a *cooldown*: a deadline
/// before which no permit is granted, regardless of availability. The
/// cooldown expires on its own, so throughput decays back to the
/// configured baseline — the baseline replenishment rate itself is
/// never modified by server feedback.
pub struct RateLimiter {
    semaphore: Arc<Semaphore>,
    permits_per_interval: u32,
    state: Mutex<RateLimiterState>,
}

struct RateLimiterState {
    interval: Duration,
    last_replenish: Instant,
    /// Server-imposed back-pressure window. While set and unexpired,
    /// `acquire` waits even when permits are available. Cleared lazily
    /// once the window elapses.
    cooldown: Option<Cooldown>,
}

/// A cooldown window stored as `(imposed_at, window)` rather than as an
/// absolute deadline.
///
/// The window value is server-controlled (`Retry-After`), so computing
/// `Instant::now() + window` could overflow `Instant`'s representable
/// range and panic. Storing the origin and the duration keeps every
/// computation in `Duration` space with saturating arithmetic — no
/// panic is possible for any header value, including `u64::MAX`
/// seconds.
struct Cooldown {
    imposed_at: Instant,
    window: Duration,
}

impl Cooldown {
    /// Remaining wait, saturating at zero once the window has elapsed.
    fn remaining(&self) -> Duration {
        self.window.saturating_sub(self.imposed_at.elapsed())
    }
}

impl RateLimiter {
    /// Creates a rate limiter that grants `permits_per_interval` permits
    /// every `interval`.
    pub fn new(permits_per_interval: u32, interval: Duration) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(permits_per_interval as usize)),
            permits_per_interval,
            state: Mutex::new(RateLimiterState {
                interval,
                last_replenish: Instant::now(),
                cooldown: None,
            }),
        }
    }

    /// Acquires a single permit. Blocks asynchronously until any active
    /// cooldown has expired and a permit is available (either already
    /// present or after replenishment).
    pub async fn acquire(&self) {
        loop {
            if let Some(wait) = self.cooldown_remaining().await {
                tokio::time::sleep(wait).await;
                continue;
            }

            if let Ok(permit) = self.semaphore.try_acquire() {
                permit.forget();
                return;
            }

            let sleep_dur = {
                let state = self.state.lock().await;
                let elapsed = state.last_replenish.elapsed();
                if elapsed >= state.interval {
                    drop(state);
                    self.replenish().await;
                    continue;
                }
                state.interval.saturating_sub(elapsed)
            };
            tokio::time::sleep(sleep_dur).await;
            self.replenish().await;
        }
    }

    /// Permits granted per replenishment interval.
    ///
    /// Test-only accessor used by NC-005 to assert that
    /// `ProviderConfig::rate_limit` flows into the limiter; exposing the
    /// configured ceiling lets callers verify wiring without driving the
    /// public `acquire()` loop over time.
    #[cfg(test)]
    #[must_use]
    pub fn permits_per_interval(&self) -> u32 {
        self.permits_per_interval
    }

    /// Replenishment interval.
    ///
    /// Test-only accessor mirroring [`Self::permits_per_interval`]: lets
    /// tests assert that `ProviderConfig::rate_limit_interval` flows
    /// into the limiter without driving the `acquire()` loop over time.
    #[cfg(test)]
    #[must_use]
    pub async fn interval(&self) -> Duration {
        self.state.lock().await.interval
    }

    /// Imposes a cooldown: no permit is granted until `retry_after` has
    /// elapsed from now. Called when the server returns `429 Too Many
    /// Requests`.
    ///
    /// The cooldown gates *all* callers, then expires on its own; the
    /// configured replenishment rate is untouched, so throughput returns
    /// to the baseline once the server stops pushing back. Overlapping
    /// cooldowns keep whichever window has the most time remaining.
    ///
    /// `retry_after` is server-controlled, so no `Instant + Duration`
    /// arithmetic is performed anywhere it flows: the window is stored
    /// as a `(imposed_at, window)` pair and compared with saturating
    /// `Duration` arithmetic, making a panic impossible for any header
    /// value (see [`Cooldown`]). Callers that need to bound the
    /// accepted window apply their configured ceiling *before* calling
    /// this method.
    pub async fn impose_cooldown(&self, retry_after: Duration) {
        let mut state = self.state.lock().await;
        let existing_remaining = state
            .cooldown
            .as_ref()
            .map_or(Duration::ZERO, Cooldown::remaining);
        if retry_after > existing_remaining {
            state.cooldown = Some(Cooldown {
                imposed_at: Instant::now(),
                window: retry_after,
            });
        }
    }

    /// Returns the remaining cooldown, clearing the gate lazily once the
    /// window has elapsed.
    async fn cooldown_remaining(&self) -> Option<Duration> {
        let mut state = self.state.lock().await;
        let remaining = state.cooldown.as_ref()?.remaining();
        if remaining.is_zero() {
            state.cooldown = None;
            return None;
        }
        Some(remaining)
    }

    async fn replenish(&self) {
        let mut state = self.state.lock().await;
        if state.last_replenish.elapsed() >= state.interval {
            self.semaphore
                .add_permits(self.permits_per_interval as usize);
            state.last_replenish = Instant::now();
        }
    }
}

const _: fn() = || {
    fn check<T: Send + Sync>() {}
    check::<RateLimiter>();
};

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

    #[tokio::test]
    async fn acquire_within_budget_succeeds_immediately() {
        let limiter = RateLimiter::new(3, Duration::from_millis(100));
        limiter.acquire().await;
        limiter.acquire().await;
        limiter.acquire().await;
    }

    #[tokio::test]
    async fn acquire_blocks_until_replenishment() {
        let limiter = RateLimiter::new(1, Duration::from_millis(100));
        limiter.acquire().await;

        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(80),
            "expected >= 80ms wait, got {elapsed:?}"
        );
    }

    /// Regression test for the inverted 429 handler (REVIEW.md H5).
    ///
    /// The old `adjust_interval` API *replaced* the replenishment
    /// interval with `Retry-After`, so a header-less 429 (1s fallback)
    /// permanently turned 60 permits/min into 60 permits/sec. A 429
    /// cooldown must never make replenishment faster than the
    /// configured baseline.
    #[tokio::test]
    async fn cooldown_shorter_than_interval_does_not_accelerate_replenishment() {
        let limiter = RateLimiter::new(1, Duration::from_millis(400));
        limiter.acquire().await;

        // Simulate a header-less 429 fallback shorter than the interval.
        limiter.impose_cooldown(Duration::from_millis(50)).await;

        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(300),
            "429 cooldown must not speed up the baseline replenishment \
             interval (expected >= 300ms, got {elapsed:?})"
        );
    }

    #[tokio::test]
    async fn cooldown_gates_acquire_even_with_permits_available() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        limiter.impose_cooldown(Duration::from_millis(150)).await;

        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(120),
            "expected acquire to wait out the cooldown, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn throughput_decays_back_to_baseline_after_cooldown() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        limiter.impose_cooldown(Duration::from_millis(100)).await;
        limiter.acquire().await;

        // Cooldown has expired; subsequent acquires with available
        // permits must be immediate again — the baseline is restored.
        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(50),
            "expected immediate acquire after cooldown expiry, got {elapsed:?}"
        );
    }

    /// Regression test for the unbounded server-controlled cooldown
    /// (fix campaign Track V, finding 1): a `Retry-After` of
    /// `u64::MAX`-style magnitude previously reached
    /// `Instant::now() + retry_after`, which panics on overflow and
    /// unwinds the provider task — the consumer stream then ends with
    /// neither `Done` nor an error. The cooldown must be imposed
    /// without any panic and must still gate `acquire`.
    #[tokio::test]
    async fn absurd_cooldown_does_not_panic_and_still_gates() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        limiter.impose_cooldown(Duration::MAX).await;

        let gated = tokio::time::timeout(Duration::from_millis(100), limiter.acquire()).await;
        assert!(
            gated.is_err(),
            "acquire must still be gated by the (saturated) cooldown"
        );
    }

    #[tokio::test]
    async fn overlapping_cooldowns_keep_latest_deadline() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        limiter.impose_cooldown(Duration::from_millis(200)).await;
        limiter.impose_cooldown(Duration::from_millis(50)).await;

        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(160),
            "shorter overlapping cooldown must not shrink the gate, got {elapsed:?}"
        );
    }
}
