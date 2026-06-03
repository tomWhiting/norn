//! Token-bucket rate limiter for provider request throttling.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

/// A token-bucket rate limiter.
///
/// Permits are replenished at a configurable interval. Callers acquire
/// a permit before making a request; if none is available the call
/// awaits asynchronously until the next replenishment.
pub struct RateLimiter {
    semaphore: Arc<Semaphore>,
    permits_per_interval: u32,
    state: Mutex<RateLimiterState>,
}

struct RateLimiterState {
    interval: Duration,
    last_replenish: Instant,
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
            }),
        }
    }

    /// Acquires a single permit. Blocks asynchronously until a permit
    /// is available (either already present or after replenishment).
    pub async fn acquire(&self) {
        loop {
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

    /// Updates the replenishment interval. Called when a 429 response
    /// includes a `Retry-After` header.
    pub async fn adjust_interval(&self, new_interval: Duration) {
        let mut state = self.state.lock().await;
        state.interval = new_interval;
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

    #[tokio::test]
    async fn adjust_interval_changes_replenishment_rate() {
        let limiter = RateLimiter::new(1, Duration::from_millis(500));
        limiter.acquire().await;

        limiter.adjust_interval(Duration::from_millis(50)).await;

        let start = Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(200),
            "expected < 200ms after adjustment, got {elapsed:?}"
        );
    }
}
