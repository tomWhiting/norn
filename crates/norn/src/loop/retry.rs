//! Provider-call retry policy with exponential backoff and full jitter.
//!
//! Retry responsibility is split by layer. The `OpenAI` provider owns the
//! *server-directed* `429` loop: it honours `Retry-After` internally with a
//! bounded budget and, once that budget is exhausted, returns
//! [`ProviderError::RateLimited`]. This module sits one layer up at the agent
//! loop boundary and owns every transient failure the provider surfaces —
//! connectivity faults, `5xx` responses, and the exhausted-budget
//! `RateLimited` error alike.
//!
//! The law (owner-ratified 2026-07-25): transient failures retry
//! **indefinitely** — exponential backoff, full jitter, a 60s ceiling — until
//! the call succeeds or the user cancels. Bounded-retry-then-die is banned:
//! [`RetryPolicy::max_attempts`] defaults to `None` (unbounded), and a custom
//! bounded policy is bounded by *attempts only*, never by a derived total
//! duration. Non-transient failures fail the turn loudly at the first
//! attempt; [`ErrorClass::Auth`] and [`ErrorClass::Terminal`] errors can
//! never be opted into retry by any policy.
//!
//! `RateLimited` is in the default retryable set (design D2). The previous
//! contract excluded it to avoid "double retry" across the provider's bounded
//! budget and the loop's bounded budget; that rationale inverts under
//! unbounded gentle retry, where an exhausted 429 budget would otherwise
//! kill a turn on a transient condition. When the provider supplies a
//! `Retry-After`, the loop's wait honours it as a **floor**:
//! `wait = max(sampled_wait, retry_after)`. The server's explicit instruction
//! is never clamped by [`RetryPolicy::backoff_ceiling`] — the ceiling bounds
//! *our own guess*, not the provider's stated requirement.

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::error::{ErrorClass, NornError, ProviderError, TransientKind};
use crate::r#loop::assembly::AssembledResponse;

/// Default initial backoff duration before the first retry.
pub const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Default exponential backoff multiplier between successive retries.
pub const DEFAULT_BACKOFF_MULTIPLIER: f64 = 2.0;
/// Default saturation ceiling for the computed backoff base.
///
/// Owner-ratified 2026-07-25. The backoff base grows exponentially until it
/// reaches this value and then stays there for every subsequent attempt, so
/// an unbounded retry loop settles into a steady one-request-per-minute
/// whisper instead of drifting towards hour-long waits. A server-supplied
/// `Retry-After` is deliberately *not* clamped by this ceiling.
pub const DEFAULT_BACKOFF_CEILING: Duration = Duration::from_secs(60);
/// Default jitter setting: full jitter on.
///
/// Owner-ratified 2026-07-25. `wait = uniform(0, base]` is the canonical AWS
/// "full jitter" shape: it halves the expected wait while removing the
/// synchronised retry storms a fixed schedule produces when many agents fail
/// against the same backend at the same instant.
pub const DEFAULT_JITTER: bool = true;

/// Categorisation of provider errors that should be retried automatically.
///
/// The `ServerError` variant carries an optional `status` so callers can
/// either match any 5xx (the policy default constructed via
/// [`RetryPolicy::default`]) or restrict retries to specific status codes
/// when a stricter policy is desired. A `status` of `0` is treated as the
/// wildcard "any 5xx" matcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryableError {
    /// Network-level timeout (read or connect).
    NetworkTimeout,
    /// Mid-stream connection reset or transport disconnect.
    ConnectionReset,
    /// HTTP 5xx server error. `status: 0` matches any 5xx.
    ServerError {
        /// HTTP status code, or `0` to match any 5xx.
        status: u16,
    },
    /// Provider rate-limit response, surfaced after the provider's own
    /// server-directed `Retry-After` budget was exhausted. In the default
    /// policy: a rate limit is a transient condition, and the loop retries
    /// it with the server's stated delay as a floor.
    RateLimited,
}

/// Retry policy applied around every provider call site.
///
/// The default is **unbounded**: transient failures retry forever with
/// jittered exponential backoff capped at [`DEFAULT_BACKOFF_CEILING`], until
/// the call succeeds or the caller's cancellation token fires. A bounded
/// policy sets [`Self::max_attempts`] to the *total* number of attempts it
/// will allow, the first one included.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Total attempts including the first. `None` — the default — retries
    /// indefinitely. `Some(1)` disables retry. `Some(0)` is a configuration
    /// error rejected by
    /// [`validate_settings`](crate::config::validate_settings); should one
    /// reach the loop anyway it is treated defensively as a single attempt.
    pub max_attempts: Option<u32>,
    /// Backoff base before the first retry.
    pub initial_backoff: Duration,
    /// Multiplier applied to the backoff base between successive retries.
    /// Values below `1.0` are rejected at config validation and clamped to
    /// `1.0` here, so the schedule can never shrink.
    pub backoff_multiplier: f64,
    /// Saturation ceiling for the computed backoff base:
    /// `base_n = min(initial * multiplier^(n-1), ceiling)`. Never exceeded
    /// by the policy's own schedule; a server-supplied `Retry-After` is a
    /// separate, un-clamped floor.
    pub backoff_ceiling: Duration,
    /// Whether to apply full jitter (`wait = uniform(0, base]`) to the
    /// computed base. `false` waits the exact base.
    pub jitter: bool,
    /// Error categories considered retryable. An empty list disables
    /// retry entirely.
    pub retryable_errors: Vec<RetryableError>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: None,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            backoff_multiplier: DEFAULT_BACKOFF_MULTIPLIER,
            backoff_ceiling: DEFAULT_BACKOFF_CEILING,
            jitter: DEFAULT_JITTER,
            retryable_errors: vec![
                RetryableError::NetworkTimeout,
                RetryableError::ConnectionReset,
                RetryableError::ServerError { status: 0 },
                RetryableError::RateLimited,
            ],
        }
    }
}

impl RetryPolicy {
    /// Determine whether an error matches the configured retryable set.
    ///
    /// Derived from the public [`ProviderError::class`] taxonomy: errors
    /// whose class is [`ErrorClass::Auth`] or [`ErrorClass::Terminal`]
    /// (authentication failures, parse errors, unsupported features,
    /// exhausted quota, oversized context, invalid requests) are never
    /// retryable regardless of the configured set.
    #[must_use]
    pub fn classifies_as_retryable(&self, err: &ProviderError) -> bool {
        let category = classify_provider_error(err);
        let Some(observed) = category else {
            return false;
        };
        self.retryable_errors.iter().any(|allowed| match allowed {
            RetryableError::ServerError { status: 0 } => {
                matches!(observed, RetryableError::ServerError { .. })
            }
            other => other == &observed,
        })
    }

    /// The backoff base after `failed_attempt` consecutive failures
    /// (`failed_attempt` is 1-based): `min(initial * multiplier^(n-1),
    /// ceiling)`.
    ///
    /// Total function: a non-finite, negative, or absurd multiplier, an
    /// initial backoff at [`Duration::MAX`], and an attempt index at the
    /// integer bound all saturate at [`Self::backoff_ceiling`] instead of
    /// panicking inside the float-to-`Duration` conversion.
    #[must_use]
    pub fn backoff_base(&self, failed_attempt: u32) -> Duration {
        if self.initial_backoff.is_zero() {
            return Duration::ZERO;
        }
        // `f64::max` propagates the non-NaN operand, so a NaN multiplier
        // degrades to a flat (1.0) schedule rather than poisoning the math.
        let multiplier = self.backoff_multiplier.max(1.0);
        let exponent = i32::try_from(failed_attempt.saturating_sub(1)).unwrap_or(i32::MAX);
        let scaled = self.initial_backoff.as_secs_f64() * multiplier.powi(exponent);
        let ceiling_secs = self.backoff_ceiling.as_secs_f64();
        if !scaled.is_finite() || scaled >= ceiling_secs {
            return self.backoff_ceiling;
        }
        Duration::try_from_secs_f64(scaled).unwrap_or(self.backoff_ceiling)
    }
}

/// Project the public [`ProviderError::class`] taxonomy onto the policy's
/// matchable [`RetryableError`] categories. `None` means "never retryable"
/// — [`ErrorClass::Auth`] and [`ErrorClass::Terminal`] errors cannot be
/// opted into retry by any policy. The taxonomy is the single source of
/// truth: this function carries no classification logic of its own.
fn classify_provider_error(err: &ProviderError) -> Option<RetryableError> {
    match err.class() {
        ErrorClass::Retryable { kind } => Some(match kind {
            TransientKind::Timeout => RetryableError::NetworkTimeout,
            TransientKind::ConnectionReset => RetryableError::ConnectionReset,
            TransientKind::ServerError { status } => RetryableError::ServerError { status },
        }),
        ErrorClass::RateLimited { .. } => Some(RetryableError::RateLimited),
        ErrorClass::Auth | ErrorClass::Terminal => None,
    }
}

/// Stable `snake_case` label for a provider error's taxonomy class.
///
/// Derived exclusively from [`ProviderError::class`] — a closed vocabulary
/// with no provider-supplied text in it. This is what retry visibility
/// events carry: house disclosure law keeps provider free text out of the
/// always-on event stream, where reasons belong only to the loud terminal
/// error (already house-sanitized).
#[must_use]
pub fn error_class_label(err: &ProviderError) -> &'static str {
    match err.class() {
        ErrorClass::Retryable { kind } => match kind {
            TransientKind::Timeout => "timeout",
            TransientKind::ConnectionReset => "connection_reset",
            TransientKind::ServerError { .. } => "server_error",
        },
        ErrorClass::RateLimited { .. } => "rate_limited",
        ErrorClass::Auth => "auth",
        ErrorClass::Terminal => "terminal",
    }
}

/// Facts about one retry decision, handed to the observer immediately
/// **before** the wait it announces begins.
///
/// [`Self::error_class`] is a `&'static str` by construction: the type
/// itself forbids smuggling provider free text into a visibility event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryNotice {
    /// 1-based index of the attempt that will follow the wait (`2` is the
    /// first retry).
    pub next_attempt: u32,
    /// Effective total attempt budget, `None` when unbounded.
    pub max_attempts: Option<u32>,
    /// The actual wait about to be taken — sampled jitter, with any
    /// server-supplied `Retry-After` already applied as a floor.
    pub delay: Duration,
    /// Stable taxonomy class label of the failure being retried.
    pub error_class: &'static str,
}

/// Terminal outcome of a retry loop.
///
/// Cancellation is a first-class outcome, distinct from failure: the step
/// surfaces it as
/// [`AgentStepResult::Cancelled`](crate::agent_loop::config::AgentStepResult),
/// never as a provider error.
#[derive(Debug)]
#[must_use]
pub enum RetryOutcome {
    /// The provider call succeeded.
    Completed(Box<AssembledResponse>),
    /// The cancellation token fired before an attempt or during an
    /// inter-attempt wait. No further attempt was made.
    Cancelled,
    /// The call failed with an error no retry can fix, or the policy's
    /// attempt budget was exhausted. Carries the final error.
    Failed(NornError),
}

/// Samples the actual inter-attempt wait from a computed backoff base.
///
/// Sealed inside this module (design D3): the retry layer owns its
/// randomness so no global RNG state can leak into scheduler or replay
/// paths. Tests inject a deterministic implementation.
pub(crate) trait JitterSource: Send + Sync {
    /// Return a wait in `(0, base]`. A zero base yields zero.
    fn sample(&self, base: Duration) -> Duration;
}

/// Full-jitter sampler seeded once from the operating system's entropy
/// source at construction.
///
/// Entropy failure is handled, never fatal: the sampler records the
/// failure, warns once, and degrades to the deterministic schedule
/// (`wait == base`). That is a documented degradation to the policy's own
/// values, not an invented one.
pub(crate) struct OsJitter {
    state: std::sync::Mutex<JitterState>,
}

enum JitterState {
    /// Seeded generator, ready to sample.
    Seeded(Box<rand::rngs::StdRng>),
    /// The OS refused to supply seed material; jitter is disabled for the
    /// lifetime of this sampler.
    EntropyUnavailable,
}

impl OsJitter {
    /// Seed a sampler from OS entropy.
    pub(crate) fn new() -> Self {
        let state = if let Some(rng) = seed_from_os() {
            JitterState::Seeded(Box::new(rng))
        } else {
            tracing::warn!(
                "operating system entropy unavailable; retry backoff jitter is disabled \
                 and waits fall back to the deterministic backoff schedule",
            );
            JitterState::EntropyUnavailable
        };
        Self {
            state: std::sync::Mutex::new(state),
        }
    }
}

fn seed_from_os() -> Option<rand::rngs::StdRng> {
    use rand::{SeedableRng as _, TryRngCore as _};
    let mut seed = <rand::rngs::StdRng as rand::SeedableRng>::Seed::default();
    match rand::rngs::OsRng.try_fill_bytes(seed.as_mut()) {
        Ok(()) => Some(rand::rngs::StdRng::from_seed(seed)),
        Err(_error) => None,
    }
}

impl JitterSource for OsJitter {
    fn sample(&self, base: Duration) -> Duration {
        use rand::Rng as _;
        let nanos = u64::try_from(base.as_nanos()).unwrap_or(u64::MAX);
        if nanos == 0 {
            return Duration::ZERO;
        }
        let Ok(mut state) = self.state.lock() else {
            // A poisoned sampler lock means a previous sampler panicked
            // while holding it. Degrade to the deterministic schedule
            // rather than propagating a panic into the retry loop.
            tracing::warn!(
                "retry jitter sampler lock was poisoned; falling back to the deterministic \
                 backoff schedule for this wait",
            );
            return base;
        };
        match &mut *state {
            JitterState::Seeded(rng) => Duration::from_nanos(rng.random_range(1..=nanos)),
            JitterState::EntropyUnavailable => base,
        }
    }
}

/// Execute `call` and retry it on transient [`ProviderError`]
/// classifications per `policy`, until it succeeds, fails
/// non-transiently, exhausts a bounded attempt budget, or `cancel` fires.
///
/// `call` is a `FnMut` because each attempt replays the frozen provider
/// request (the provider's `stream` consumes it). The returned future is
/// generic, so the closure may borrow from the surrounding scope (e.g.
/// `&dyn Provider`).
///
/// `observer` is invoked once per retry decision, immediately **before**
/// the wait it describes, with the actual sampled delay.
///
/// `cancel` makes interruptibility structural rather than incidental:
/// every inter-attempt wait races the token in a `biased` select with the
/// cancel arm first, and the token is re-checked before each attempt. With
/// `cancel: None` the wait is uninterruptible by design — a caller that
/// hosts an unbounded loop without a token must supply one. A configured
/// `step_timeout` remains a valid terminator: it is user-configured, so it
/// counts as user intent.
pub async fn retry_with_backoff<Call, Fut, Observer>(
    policy: &RetryPolicy,
    cancel: Option<&CancellationToken>,
    observer: Observer,
    call: Call,
) -> RetryOutcome
where
    Call: FnMut() -> Fut,
    Fut: Future<Output = Result<AssembledResponse, NornError>>,
    Observer: FnMut(&RetryNotice),
{
    let jitter = OsJitter::new();
    retry_with_sampler(policy, cancel, &jitter, observer, call).await
}

/// [`retry_with_backoff`] with an explicit jitter source, so tests can pin
/// the exact wait the loop takes.
pub(crate) async fn retry_with_sampler<Call, Fut, Observer>(
    policy: &RetryPolicy,
    cancel: Option<&CancellationToken>,
    jitter: &dyn JitterSource,
    mut observer: Observer,
    mut call: Call,
) -> RetryOutcome
where
    Call: FnMut() -> Fut,
    Fut: Future<Output = Result<AssembledResponse, NornError>>,
    Observer: FnMut(&RetryNotice),
{
    // `Some(0)` is rejected at config validation; if one reaches the loop
    // anyway, make one attempt and report its failure rather than spinning
    // or returning a fabricated success.
    let budget = policy.max_attempts.map(|max| max.max(1));
    let mut attempt: u32 = 0;
    loop {
        if is_cancelled(cancel) {
            return RetryOutcome::Cancelled;
        }
        attempt = attempt.saturating_add(1);
        let provider_err = match call().await {
            Ok(response) => return RetryOutcome::Completed(Box::new(response)),
            Err(NornError::Provider(err)) => err,
            Err(other) => return RetryOutcome::Failed(other),
        };
        let budget_exhausted = budget.is_some_and(|max| attempt >= max);
        if budget_exhausted || !policy.classifies_as_retryable(&provider_err) {
            return RetryOutcome::Failed(NornError::Provider(provider_err));
        }

        let base = policy.backoff_base(attempt);
        let mut delay = if policy.jitter {
            jitter.sample(base)
        } else {
            base
        };
        // The server's own instruction is a floor and is honoured in full:
        // `backoff_ceiling` caps the policy's guess, never a stated
        // `Retry-After`.
        if let Some(retry_after) = provider_err.class().retry_after() {
            delay = delay.max(retry_after);
        }

        let notice = RetryNotice {
            next_attempt: attempt.saturating_add(1),
            max_attempts: budget,
            delay,
            error_class: error_class_label(&provider_err),
        };
        tracing::warn!(
            attempt = notice.next_attempt,
            max_attempts = ?notice.max_attempts,
            error_class = notice.error_class,
            error = %provider_err,
            delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            "provider call failed; retrying after backoff",
        );
        observer(&notice);

        if wait_or_cancel(delay, cancel).await == WaitOutcome::Cancelled {
            return RetryOutcome::Cancelled;
        }
    }
}

fn is_cancelled(cancel: Option<&CancellationToken>) -> bool {
    cancel.is_some_and(CancellationToken::is_cancelled)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitOutcome {
    /// The full wait elapsed.
    Elapsed,
    /// The cancellation token fired during the wait.
    Cancelled,
}

/// Sleep for `delay`, racing the cancellation token with the cancel arm
/// first so a token that fires while the timer is also ready resolves as
/// cancellation.
async fn wait_or_cancel(delay: Duration, cancel: Option<&CancellationToken>) -> WaitOutcome {
    tokio::select! {
        biased;
        () = cancelled_or_pending(cancel) => WaitOutcome::Cancelled,
        () = tokio::time::sleep(delay) => WaitOutcome::Elapsed,
    }
}

/// Resolve when the token fires; never resolve when no token is
/// configured.
async fn cancelled_or_pending(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::too_many_lines
)]
mod tests {
    use super::*;
    use crate::r#loop::assembly::AssembledResponse;
    use crate::provider::events::StopReason;
    use crate::provider::usage::Usage;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio_util::sync::CancellationToken;

    fn ok_response() -> AssembledResponse {
        AssembledResponse {
            response_items: Vec::new(),
            refusal: None,
            text: "ok".to_string(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
            response_audio: None,
        }
    }

    fn transient_503() -> NornError {
        NornError::Provider(ProviderError::StreamError {
            reason: "HTTP 503: try again".to_string(),
            transient: Some(TransientKind::ServerError { status: 503 }),
        })
    }

    /// Deterministic jitter source for tests: records every base it is
    /// asked to sample and returns either the base itself (identity) or a
    /// fixed override, so a test can pin the exact wait the loop uses.
    struct RecordingJitter {
        bases: Mutex<Vec<Duration>>,
        fixed: Option<Duration>,
    }

    impl RecordingJitter {
        fn identity() -> Self {
            Self {
                bases: Mutex::new(Vec::new()),
                fixed: None,
            }
        }

        fn fixed(wait: Duration) -> Self {
            Self {
                bases: Mutex::new(Vec::new()),
                fixed: Some(wait),
            }
        }

        fn bases(&self) -> Vec<Duration> {
            self.bases.lock().expect("recording jitter lock").clone()
        }
    }

    impl JitterSource for RecordingJitter {
        fn sample(&self, base: Duration) -> Duration {
            self.bases.lock().expect("recording jitter lock").push(base);
            self.fixed.unwrap_or(base)
        }
    }

    /// Collects every [`RetryNotice`] the loop emits together with the
    /// paused-clock instant at which it was emitted, so a test can prove
    /// the event precedes the wait it announces.
    #[derive(Default)]
    struct NoticeLog {
        entries: Mutex<Vec<(tokio::time::Instant, RetryNotice)>>,
    }

    impl NoticeLog {
        fn record(&self, notice: &RetryNotice) {
            self.entries
                .lock()
                .expect("notice log lock")
                .push((tokio::time::Instant::now(), notice.clone()));
        }

        fn notices(&self) -> Vec<RetryNotice> {
            self.entries
                .lock()
                .expect("notice log lock")
                .iter()
                .map(|(_instant, notice)| notice.clone())
                .collect()
        }

        fn instants(&self) -> Vec<tokio::time::Instant> {
            self.entries
                .lock()
                .expect("notice log lock")
                .iter()
                .map(|(instant, _notice)| *instant)
                .collect()
        }
    }

    // -- Policy shape ----------------------------------------------------

    /// The ratified defaults (owner-ratified 2026-07-25): unbounded
    /// attempts, 1s initial backoff, 2x growth, 60s ceiling, full jitter
    /// on, and `RateLimited` inside the default retryable set.
    #[test]
    fn default_policy_matches_ratified_constants() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts, None, "the default is unbounded retry");
        assert_eq!(policy.initial_backoff, Duration::from_secs(1));
        assert!((policy.backoff_multiplier - 2.0).abs() < f64::EPSILON);
        assert_eq!(policy.backoff_ceiling, Duration::from_secs(60));
        assert!(policy.jitter, "full jitter is on by default");
        assert!(
            policy
                .retryable_errors
                .contains(&RetryableError::NetworkTimeout)
        );
        assert!(
            policy
                .retryable_errors
                .contains(&RetryableError::ConnectionReset)
        );
        assert!(
            policy
                .retryable_errors
                .iter()
                .any(|e| matches!(e, RetryableError::ServerError { status: 0 }))
        );
        assert!(
            policy
                .retryable_errors
                .contains(&RetryableError::RateLimited),
            "RateLimited joined the default set (design D2): a rate limit is \
             a transient condition and the loop must not die on it",
        );
    }

    #[test]
    fn classifies_5xx_as_server_error() {
        let policy = RetryPolicy::default();
        let err = ProviderError::StreamError {
            reason: "HTTP 503: service unavailable".to_string(),
            transient: Some(TransientKind::ServerError { status: 503 }),
        };
        assert!(policy.classifies_as_retryable(&err));
    }

    #[test]
    fn classifies_timeout_as_network_timeout() {
        let policy = RetryPolicy::default();
        let err = ProviderError::ConnectionFailed {
            reason: "request timed out after 30s".to_string(),
            kind: TransientKind::Timeout,
        };
        assert!(policy.classifies_as_retryable(&err));
    }

    /// Single-source-of-truth invariant: the loop's policy classifier is a
    /// pure projection of the public [`ProviderError::class`] taxonomy.
    /// Any error the taxonomy calls `Auth` or `Terminal` must be refused
    /// by *every* policy — including one that opts into all retryable
    /// categories — and any error the taxonomy calls `Retryable` must be
    /// accepted by the maximally permissive policy.
    ///
    /// (Replaces `truncated_never_retries`: deterministic stops no longer
    /// exist as a `ProviderError` at all — truncation is a typed
    /// `AgentStepResult::Truncated` stop outcome, so it can never reach the
    /// retry classifier. The runner's truncation tests pin that behaviour.)
    #[test]
    fn policy_classification_is_a_projection_of_the_public_taxonomy() {
        use crate::error::ErrorClass;
        let permissive = maximally_permissive_policy();
        let cases = vec![
            ProviderError::ConnectionFailed {
                reason: "request timed out".to_string(),
                kind: TransientKind::Timeout,
            },
            ProviderError::ConnectionFailed {
                reason: "connection refused".to_string(),
                kind: TransientKind::ConnectionReset,
            },
            ProviderError::StreamInterrupted {
                reason: "reset".to_string(),
            },
            ProviderError::StreamError {
                reason: "HTTP 502: bad gateway".to_string(),
                transient: Some(TransientKind::ServerError { status: 502 }),
            },
            ProviderError::StreamError {
                reason: "protocol violation".to_string(),
                transient: None,
            },
            ProviderError::RateLimited { retry_after: None },
            ProviderError::AuthenticationFailed {
                reason: "401".to_string(),
            },
            ProviderError::ResponseParseError {
                reason: "bad json".to_string(),
            },
            ProviderError::RequestSerializationFailed {
                reason: "unserializable payload".to_string(),
            },
            ProviderError::UnsupportedFeature {
                feature: "x".to_string(),
            },
            ProviderError::ContextWindowExceeded,
            ProviderError::QuotaExceeded,
            ProviderError::InvalidRequest {
                message: "bad prompt".to_string(),
            },
        ];
        for err in cases {
            let expected = err.class().is_retryable();
            assert_eq!(
                permissive.classifies_as_retryable(&err),
                expected,
                "policy and taxonomy disagree for {err:?} (class {:?})",
                err.class(),
            );
            if matches!(err.class(), ErrorClass::Auth | ErrorClass::Terminal) {
                assert!(
                    !permissive.classifies_as_retryable(&err),
                    "terminal-class error must be refused by every policy: {err:?}"
                );
            }
        }
    }

    #[test]
    fn auth_failure_never_retries() {
        let policy = RetryPolicy::default();
        let err = ProviderError::AuthenticationFailed {
            reason: "401 Unauthorized".to_string(),
        };
        assert!(!policy.classifies_as_retryable(&err));
    }

    #[test]
    fn unsupported_feature_never_retries() {
        let policy = RetryPolicy::default();
        let err = ProviderError::UnsupportedFeature {
            feature: "custom_grammar".to_string(),
        };
        assert!(!policy.classifies_as_retryable(&err));
    }

    /// A custom policy that lists every retryable category — used to prove the
    /// three terminal client-fault variants are non-retryable regardless of
    /// policy, since `classify_provider_error` returns `None` for them.
    fn maximally_permissive_policy() -> RetryPolicy {
        RetryPolicy {
            retryable_errors: vec![
                RetryableError::NetworkTimeout,
                RetryableError::ConnectionReset,
                RetryableError::ServerError { status: 0 },
                RetryableError::RateLimited,
            ],
            ..RetryPolicy::default()
        }
    }

    /// Inverted from the pre-retry-forever contract
    /// (`rate_limited_not_retryable_under_default`). The old exclusion
    /// existed to avoid double-retrying a bounded provider budget; under
    /// unbounded gentle retry an exhausted 429 budget must not kill the
    /// turn (design D2).
    #[test]
    fn rate_limited_is_retryable_under_the_default_policy() {
        let policy = RetryPolicy::default();
        let err = ProviderError::RateLimited { retry_after: None };
        assert!(policy.classifies_as_retryable(&err));
    }

    #[test]
    fn rate_limited_retryable_when_added_to_custom_policy() {
        let policy = RetryPolicy {
            retryable_errors: vec![RetryableError::RateLimited],
            ..RetryPolicy::default()
        };
        let err = ProviderError::RateLimited { retry_after: None };
        assert!(policy.classifies_as_retryable(&err));
    }

    #[test]
    fn context_window_exceeded_never_retryable() {
        let err = ProviderError::ContextWindowExceeded;
        assert!(!RetryPolicy::default().classifies_as_retryable(&err));
        assert!(!maximally_permissive_policy().classifies_as_retryable(&err));
    }

    #[test]
    fn quota_exceeded_never_retryable() {
        let err = ProviderError::QuotaExceeded;
        assert!(!RetryPolicy::default().classifies_as_retryable(&err));
        assert!(!maximally_permissive_policy().classifies_as_retryable(&err));
    }

    #[test]
    fn invalid_request_never_retryable() {
        let err = ProviderError::InvalidRequest {
            message: "bad prompt".to_string(),
        };
        assert!(!RetryPolicy::default().classifies_as_retryable(&err));
        assert!(!maximally_permissive_policy().classifies_as_retryable(&err));
    }

    #[test]
    fn server_overloaded_structured_503_is_retryable_under_default() {
        // Proves the structured SSE classification (`transient:
        // Some(ServerError { status: 503 })`) round-trips through the
        // loop-level classifier as a retryable ServerError under the
        // default policy. Without the structured kind the StreamError
        // would fall through `classify_provider_error` to `None` and
        // never retry.
        let policy = RetryPolicy::default();
        let err = ProviderError::StreamError {
            reason: "server is overloaded".to_string(),
            transient: Some(TransientKind::ServerError { status: 503 }),
        };
        assert!(policy.classifies_as_retryable(&err));
    }

    #[test]
    fn stream_error_without_transient_is_never_retryable() {
        // The reason text carries zero weight: even a reason that spells
        // out `HTTP 503` stays terminal when the producer set no
        // structured transient kind.
        let err = ProviderError::StreamError {
            reason: "HTTP 503: slow down".to_string(),
            transient: None,
        };
        assert!(!RetryPolicy::default().classifies_as_retryable(&err));
        assert!(!maximally_permissive_policy().classifies_as_retryable(&err));
    }

    // -- Backoff schedule ------------------------------------------------

    /// `base_n = min(initial * multiplier^(n-1), ceiling)`, saturating at
    /// the ratified 60s ceiling and never beyond it.
    #[test]
    fn backoff_base_saturates_at_the_ceiling() {
        let policy = RetryPolicy::default();
        let bases: Vec<Duration> = (1..=10).map(|n| policy.backoff_base(n)).collect();
        assert_eq!(
            bases,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(32),
                Duration::from_secs(60),
                Duration::from_secs(60),
                Duration::from_secs(60),
                Duration::from_secs(60),
            ],
        );
    }

    /// Pathological policies must saturate, never panic: a non-finite or
    /// absurd multiplier, an enormous initial backoff, and an attempt
    /// index at the integer bound all clamp to the ceiling.
    #[test]
    fn backoff_math_is_panic_free_for_pathological_policies() {
        let ceiling = Duration::from_secs(60);
        for multiplier in [f64::NAN, f64::INFINITY, -1.0, 0.0, 1e300] {
            let policy = RetryPolicy {
                backoff_multiplier: multiplier,
                backoff_ceiling: ceiling,
                ..RetryPolicy::default()
            };
            for attempt in [1_u32, 2, 1_000, u32::MAX] {
                assert!(
                    policy.backoff_base(attempt) <= ceiling,
                    "multiplier {multiplier} attempt {attempt} exceeded the ceiling",
                );
            }
        }
        let huge = RetryPolicy {
            initial_backoff: Duration::MAX,
            ..RetryPolicy::default()
        };
        assert_eq!(huge.backoff_base(1), huge.backoff_ceiling);
        let zero = RetryPolicy {
            initial_backoff: Duration::ZERO,
            ..RetryPolicy::default()
        };
        assert_eq!(zero.backoff_base(9), Duration::ZERO);
    }

    /// Full jitter: every sampled wait lands in `(0, base]` — never zero,
    /// never above the base.
    #[test]
    fn full_jitter_samples_within_the_half_open_base_interval() {
        let jitter = OsJitter::new();
        let base = Duration::from_secs(60);
        for _ in 0..1_000 {
            let sampled = jitter.sample(base);
            assert!(sampled > Duration::ZERO, "full jitter must not sample zero");
            assert!(sampled <= base, "full jitter must not exceed the base");
        }
        assert_eq!(jitter.sample(Duration::ZERO), Duration::ZERO);
    }

    // -- Attempt budget --------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn unbounded_policy_survives_a_long_failure_streak() {
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts, None);
        let outcome = retry_with_backoff(
            &policy,
            None,
            |_notice| {},
            || {
                let count = attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if count < 50 {
                        Err(transient_503())
                    } else {
                        Ok(ok_response())
                    }
                })
            },
        )
        .await;
        assert!(
            matches!(outcome, RetryOutcome::Completed(_)),
            "an unbounded policy must outlive a 50-failure outage",
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 51);
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_policy_stops_after_its_total_attempt_budget() {
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy {
            max_attempts: Some(3),
            ..RetryPolicy::default()
        };
        let outcome = retry_with_backoff(
            &policy,
            None,
            |_notice| {},
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(transient_503()) })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Failed(_)));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "max_attempts counts TOTAL attempts including the first",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn single_attempt_policy_never_retries() {
        let attempts = AtomicUsize::new(0);
        let notices = NoticeLog::default();
        let policy = RetryPolicy {
            max_attempts: Some(1),
            ..RetryPolicy::default()
        };
        let outcome = retry_with_backoff(
            &policy,
            None,
            |notice| notices.record(notice),
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(transient_503()) })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Failed(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(
            notices.notices().is_empty(),
            "no wait is announced when no retry follows",
        );
    }

    /// `Some(0)` is a configuration error (rejected by
    /// `validate_settings`). A policy that reaches the loop with it anyway
    /// must still make one attempt and fail loudly — never spin, never
    /// panic, never silently succeed.
    #[tokio::test(start_paused = true)]
    async fn zero_attempt_policy_defensively_makes_exactly_one_attempt() {
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy {
            max_attempts: Some(0),
            ..RetryPolicy::default()
        };
        let outcome = retry_with_backoff(
            &policy,
            None,
            |_notice| {},
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(transient_503()) })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Failed(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_until_success() {
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy::default();
        let outcome = retry_with_backoff(
            &policy,
            None,
            |_notice| {},
            || {
                let count = attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if count < 2 {
                        Err(transient_503())
                    } else {
                        Ok(ok_response())
                    }
                })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Completed(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    /// A non-transient failure fails loudly at attempt 1 under every
    /// policy, unbounded or not.
    #[tokio::test(start_paused = true)]
    async fn non_retryable_fails_loudly_at_the_first_attempt() {
        let attempts = AtomicUsize::new(0);
        let notices = NoticeLog::default();
        let policy = RetryPolicy::default();
        let outcome = retry_with_backoff(
            &policy,
            None,
            |notice| notices.record(notice),
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Err(NornError::Provider(ProviderError::AuthenticationFailed {
                        reason: "bad key".to_string(),
                    }))
                })
            },
        )
        .await;
        match outcome {
            RetryOutcome::Failed(NornError::Provider(ProviderError::AuthenticationFailed {
                ..
            })) => {}
            other => panic!("expected the typed auth failure, got {other:?}"),
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(notices.notices().is_empty());
    }

    /// A non-provider error is never the retry brain's business: it
    /// surfaces immediately.
    #[tokio::test(start_paused = true)]
    async fn non_provider_error_is_returned_untouched() {
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy::default();
        let outcome = retry_with_backoff(
            &policy,
            None,
            |_notice| {},
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Err(NornError::Session(
                        crate::error::SessionError::StorageError {
                            reason: "disk gone".to_string(),
                        },
                    ))
                })
            },
        )
        .await;
        assert!(matches!(
            outcome,
            RetryOutcome::Failed(NornError::Session(_)),
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    // -- Wait computation ------------------------------------------------

    /// The waits the loop actually takes are the ceiling-saturated bases,
    /// handed to the injected sampler in order.
    #[tokio::test(start_paused = true)]
    async fn sampler_sees_every_ceiling_saturated_base_in_order() {
        let jitter = RecordingJitter::identity();
        let policy = RetryPolicy {
            max_attempts: Some(12),
            ..RetryPolicy::default()
        };
        let outcome = retry_with_sampler(
            &policy,
            None,
            &jitter,
            |_notice| {},
            || Box::pin(async { Err(transient_503()) }),
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Failed(_)));
        assert_eq!(
            jitter.bases(),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(32),
                Duration::from_secs(60),
                Duration::from_secs(60),
                Duration::from_secs(60),
                Duration::from_secs(60),
                Duration::from_secs(60),
            ],
            "11 waits for 12 attempts, saturating at the 60s ceiling",
        );
    }

    /// With jitter off the wait is exactly the base and the sampler is
    /// never consulted at all.
    #[tokio::test(start_paused = true)]
    async fn jitter_off_waits_the_exact_base_without_sampling() {
        let jitter = RecordingJitter::identity();
        let notices = NoticeLog::default();
        let policy = RetryPolicy {
            max_attempts: Some(4),
            jitter: false,
            ..RetryPolicy::default()
        };
        let started = tokio::time::Instant::now();
        let outcome = retry_with_sampler(
            &policy,
            None,
            &jitter,
            |notice| notices.record(notice),
            || Box::pin(async { Err(transient_503()) }),
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Failed(_)));
        assert!(
            jitter.bases().is_empty(),
            "jitter: false must not consult the sampler",
        );
        let delays: Vec<Duration> = notices
            .notices()
            .into_iter()
            .map(|notice| notice.delay)
            .collect();
        assert_eq!(
            delays,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
        );
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(7),
            "the loop slept exactly the announced schedule",
        );
    }

    /// A server-stated `Retry-After` is a floor, not a replacement: it
    /// wins over a smaller sampled wait.
    #[tokio::test(start_paused = true)]
    async fn retry_after_floor_beats_a_smaller_sampled_wait() {
        let jitter = RecordingJitter::fixed(Duration::from_millis(5));
        let notices = NoticeLog::default();
        let policy = RetryPolicy {
            max_attempts: Some(2),
            ..RetryPolicy::default()
        };
        let started = tokio::time::Instant::now();
        let outcome = retry_with_sampler(
            &policy,
            None,
            &jitter,
            |notice| notices.record(notice),
            || {
                Box::pin(async {
                    Err(NornError::Provider(ProviderError::RateLimited {
                        retry_after: Some(Duration::from_secs(30)),
                    }))
                })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Failed(_)));
        let notices = notices.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].delay, Duration::from_secs(30));
        assert_eq!(notices[0].error_class, "rate_limited");
        assert_eq!(started.elapsed(), Duration::from_secs(30));
    }

    /// Reviewer ruling (2026-07-25): the 60s ceiling caps our own guessed
    /// backoff, never the server's explicit instruction. A five-minute
    /// `Retry-After` is honored in full.
    #[tokio::test(start_paused = true)]
    async fn retry_after_beyond_the_ceiling_is_honored_in_full() {
        let jitter = RecordingJitter::identity();
        let notices = NoticeLog::default();
        let policy = RetryPolicy {
            max_attempts: Some(2),
            ..RetryPolicy::default()
        };
        assert_eq!(policy.backoff_ceiling, Duration::from_secs(60));
        let started = tokio::time::Instant::now();
        let outcome = retry_with_sampler(
            &policy,
            None,
            &jitter,
            |notice| notices.record(notice),
            || {
                Box::pin(async {
                    Err(NornError::Provider(ProviderError::RateLimited {
                        retry_after: Some(Duration::from_secs(300)),
                    }))
                })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Failed(_)));
        let notices = notices.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].delay,
            Duration::from_secs(300),
            "a server-stated Retry-After is never clamped to the ceiling",
        );
        assert_eq!(started.elapsed(), Duration::from_secs(300));
    }

    /// A smaller `Retry-After` never shortens the policy's own backoff.
    #[tokio::test(start_paused = true)]
    async fn retry_after_smaller_than_the_sampled_wait_does_not_shorten_it() {
        let jitter = RecordingJitter::identity();
        let notices = NoticeLog::default();
        let policy = RetryPolicy {
            max_attempts: Some(2),
            initial_backoff: Duration::from_secs(10),
            ..RetryPolicy::default()
        };
        let outcome = retry_with_sampler(
            &policy,
            None,
            &jitter,
            |notice| notices.record(notice),
            || {
                Box::pin(async {
                    Err(NornError::Provider(ProviderError::RateLimited {
                        retry_after: Some(Duration::from_secs(1)),
                    }))
                })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Failed(_)));
        assert_eq!(notices.notices()[0].delay, Duration::from_secs(10));
    }

    // -- Visibility ------------------------------------------------------

    /// The notice is emitted BEFORE the wait it announces, carries the
    /// actual sampled delay, the next attempt index, the effective
    /// budget, and a taxonomy class label.
    #[tokio::test(start_paused = true)]
    async fn notice_precedes_the_wait_and_carries_the_actual_delay() {
        let jitter = RecordingJitter::fixed(Duration::from_secs(7));
        let notices = NoticeLog::default();
        let policy = RetryPolicy {
            max_attempts: Some(3),
            ..RetryPolicy::default()
        };
        let started = tokio::time::Instant::now();
        let outcome = retry_with_sampler(
            &policy,
            None,
            &jitter,
            |notice| notices.record(notice),
            || {
                Box::pin(async {
                    Err(NornError::Provider(ProviderError::ConnectionFailed {
                        reason: "connection reset by peer".to_string(),
                        kind: TransientKind::ConnectionReset,
                    }))
                })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Failed(_)));
        let emitted = notices.notices();
        assert_eq!(emitted.len(), 2, "two waits precede three attempts");
        assert_eq!(emitted[0].next_attempt, 2);
        assert_eq!(emitted[1].next_attempt, 3);
        assert!(emitted.iter().all(|n| n.max_attempts == Some(3)));
        assert!(
            emitted
                .iter()
                .all(|n| n.delay == Duration::from_secs(7) && n.error_class == "connection_reset"),
        );
        let instants = notices.instants();
        assert_eq!(
            instants[0], started,
            "the first notice is emitted before any sleep",
        );
        assert_eq!(
            instants[1],
            started + Duration::from_secs(7),
            "the second notice is emitted before the second sleep, not after it",
        );
        assert_eq!(started.elapsed(), Duration::from_secs(14));
    }

    /// An unbounded policy reports `max_attempts: None` on every notice —
    /// the value the wire shape must render as JSON `null`.
    #[tokio::test(start_paused = true)]
    async fn unbounded_policy_reports_no_attempt_ceiling_on_the_notice() {
        let jitter = RecordingJitter::identity();
        let notices = NoticeLog::default();
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy::default();
        let outcome = retry_with_sampler(
            &policy,
            None,
            &jitter,
            |notice| notices.record(notice),
            || {
                let count = attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if count < 2 {
                        Err(transient_503())
                    } else {
                        Ok(ok_response())
                    }
                })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Completed(_)));
        let emitted = notices.notices();
        assert_eq!(emitted.len(), 2);
        assert!(emitted.iter().all(|notice| notice.max_attempts.is_none()));
    }

    /// The class label is derived from the taxonomy alone — a stable
    /// `snake_case` vocabulary that can never carry provider free text.
    #[test]
    fn error_class_labels_come_from_the_taxonomy_only() {
        let cases = [
            (
                ProviderError::ConnectionFailed {
                    reason: "secret internal hostname".to_string(),
                    kind: TransientKind::Timeout,
                },
                "timeout",
            ),
            (
                ProviderError::StreamInterrupted {
                    reason: "secret internal hostname".to_string(),
                },
                "connection_reset",
            ),
            (
                ProviderError::StreamError {
                    reason: "secret internal hostname".to_string(),
                    transient: Some(TransientKind::ServerError { status: 502 }),
                },
                "server_error",
            ),
            (
                ProviderError::RateLimited {
                    retry_after: Some(Duration::from_secs(1)),
                },
                "rate_limited",
            ),
            (
                ProviderError::AuthenticationFailed {
                    reason: "secret token".to_string(),
                },
                "auth",
            ),
            (ProviderError::QuotaExceeded, "terminal"),
        ];
        for (err, expected) in cases {
            let label = error_class_label(&err);
            assert_eq!(label, expected, "wrong label for {err:?}");
            assert!(
                !label.contains("secret"),
                "the label must never echo provider text",
            );
        }
    }

    // -- Cancellation ----------------------------------------------------

    /// A token tripped mid-sleep stops the loop at once: the outcome is
    /// `Cancelled`, no further attempt is made, and the loop does not wait
    /// out the remainder of the backoff.
    #[tokio::test(start_paused = true)]
    async fn cancel_mid_sleep_stops_immediately_with_a_cancelled_outcome() {
        let token = CancellationToken::new();
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy {
            initial_backoff: Duration::from_secs(600),
            jitter: false,
            ..RetryPolicy::default()
        };
        let started = tokio::time::Instant::now();
        let canceller = {
            let token = token.clone();
            async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                token.cancel();
            }
        };
        let retry = retry_with_backoff(
            &policy,
            Some(&token),
            |_notice| {},
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(transient_503()) })
            },
        );
        let (outcome, ()) = tokio::join!(retry, canceller);
        assert!(
            matches!(outcome, RetryOutcome::Cancelled),
            "cancellation is never a provider failure",
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "no attempt may follow the cancel",
        );
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(5),
            "the loop woke on the token, not on the 600s backoff",
        );
    }

    /// A token already tripped when the loop starts prevents even the
    /// first attempt.
    #[tokio::test(start_paused = true)]
    async fn pre_cancelled_token_makes_no_attempt_at_all() {
        let token = CancellationToken::new();
        token.cancel();
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy::default();
        let outcome = retry_with_backoff(
            &policy,
            Some(&token),
            |_notice| {},
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(ok_response()) })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Cancelled));
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
    }

    /// A token tripped between attempts (while the loop is not sleeping
    /// on it) is still observed at the top of the loop.
    #[tokio::test(start_paused = true)]
    async fn cancel_between_attempts_stops_before_the_next_call() {
        let token = CancellationToken::new();
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy {
            initial_backoff: Duration::ZERO,
            jitter: false,
            ..RetryPolicy::default()
        };
        let outcome = retry_with_backoff(
            &policy,
            Some(&token),
            |_notice| {},
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                token.cancel();
                Box::pin(async { Err(transient_503()) })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Cancelled));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    /// `cancel: None` must never park the loop: the wait still elapses and
    /// the loop still finishes.
    #[tokio::test(start_paused = true)]
    async fn absent_cancel_token_never_blocks_the_wait() {
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy {
            jitter: false,
            ..RetryPolicy::default()
        };
        let started = tokio::time::Instant::now();
        let outcome = retry_with_backoff(
            &policy,
            None,
            |_notice| {},
            || {
                let count = attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if count < 1 {
                        Err(transient_503())
                    } else {
                        Ok(ok_response())
                    }
                })
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Completed(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(started.elapsed(), Duration::from_secs(1));
    }
}
