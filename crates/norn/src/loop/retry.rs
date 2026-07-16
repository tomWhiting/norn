//! Provider-call retry policy with exponential backoff.
//!
//! N-023 R1. Retry responsibility is split by layer. The `OpenAI` provider
//! owns `429` rate-limit retry: it retries internally with `Retry-After`
//! backoff and, once its budget is exhausted, returns
//! [`ProviderError::RateLimited`]. This module sits one layer up at the agent
//! loop boundary and owns *transient connectivity* failures and `5xx`
//! responses, giving them a bounded retry budget without burning iterations
//! or falling out of the step. `RateLimited` is therefore excluded from the
//! default policy to prevent double retry (provider × loop = up to 9 HTTP
//! requests on sustained 429); it remains available for custom policies that
//! deliberately want loop-level 429 retry.

use std::future::Future;
use std::time::Duration;

use crate::error::{ErrorClass, NornError, ProviderError, TransientKind};
use crate::r#loop::assembly::AssembledResponse;

/// Default maximum retry attempts (excluding the original call).
pub const DEFAULT_MAX_RETRIES: u32 = 2;
/// Default initial backoff duration before the first retry.
pub const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Default exponential backoff multiplier between successive retries.
pub const DEFAULT_BACKOFF_MULTIPLIER: f64 = 2.0;

/// Saturation bound for backoff math, in whole seconds: `u64::MAX / 2`
/// (roughly 292 billion years). Overflowing or non-finite backoff
/// computations clamp here instead of panicking inside
/// [`Duration::from_secs_f64`].
const BACKOFF_SATURATION_SECS: u64 = u64::MAX / 2;

/// The exact `f64` that `BACKOFF_SATURATION_SECS` rounds to: 2^63.
/// Comparing against this exactly-representable literal avoids a lossy
/// integer-to-float cast while preserving the same comparison the cast
/// produced (`u64::MAX / 2` rounds up to 2^63 in `f64`).
const BACKOFF_SATURATION_SECS_F64: f64 = 9_223_372_036_854_775_808.0;

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
    /// Provider rate-limit response. Provider handles 429 internally with
    /// `Retry-After` backoff. Available for custom policies that need
    /// loop-level rate-limit retry, but excluded from the default policy to
    /// prevent double retry.
    RateLimited,
}

/// Bounded retry policy applied around every provider call site.
///
/// `total` retries are computed as `max_retries`; with the default
/// multiplier of 2.0 and an initial backoff of 1s, the bounded waits are
/// 1s + 2s = 3s for the default `max_retries = 2`. The policy short-
/// circuits if the cumulative back-off would exceed the cap derived from
/// `max_retries * initial_backoff * multiplier^max_retries` to keep total
/// elapsed time bounded.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Maximum retry attempts after the first failure.
    pub max_retries: u32,
    /// Backoff duration before the first retry.
    pub initial_backoff: Duration,
    /// Multiplier applied to the backoff between retries.
    pub backoff_multiplier: f64,
    /// Error categories considered retryable. An empty list disables
    /// retry entirely.
    pub retryable_errors: Vec<RetryableError>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            backoff_multiplier: DEFAULT_BACKOFF_MULTIPLIER,
            // RateLimited is deliberately excluded: the provider owns 429
            // retry (Retry-After backoff, internal budget). Including it here
            // would double-retry an already-exhausted rate limit. Custom
            // policies may opt in by adding RetryableError::RateLimited.
            retryable_errors: vec![
                RetryableError::NetworkTimeout,
                RetryableError::ConnectionReset,
                RetryableError::ServerError { status: 0 },
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

    /// Total retry duration cap used to short-circuit policies that would
    /// otherwise wait unboundedly. Computed as `max_retries *
    /// initial_backoff * multiplier^max_retries` and saturating on
    /// overflow.
    #[must_use]
    pub fn total_duration_cap(&self) -> Duration {
        let multiplier = self
            .backoff_multiplier
            .max(1.0)
            .powi(self.max_retries.cast_signed());
        let scale = f64::from(self.max_retries).max(1.0) * multiplier;
        let initial_secs = self.initial_backoff.as_secs_f64();
        let total = initial_secs * scale;
        if total.is_finite() && total < BACKOFF_SATURATION_SECS_F64 {
            Duration::from_secs_f64(total)
        } else {
            Duration::from_secs(BACKOFF_SATURATION_SECS)
        }
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

/// Execute `call` and retry it on transient `ProviderError` classifications
/// per `policy`. `call` is a `FnMut` because each retry rebuilds the
/// provider request (and the provider's `stream` consumes it). The
/// returned future is generic, so the closure may borrow data from the
/// surrounding scope (e.g. `&dyn Provider`).
///
/// # Errors
///
/// Returns the final [`NornError`] after the retry budget is exhausted, or
/// the first non-retryable error encountered.
pub async fn retry_with_backoff<F, Fut>(
    policy: &RetryPolicy,
    mut call: F,
) -> Result<AssembledResponse, NornError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<AssembledResponse, NornError>>,
{
    let cap = policy.total_duration_cap();
    let mut total_elapsed = Duration::ZERO;
    let mut backoff = policy.initial_backoff;
    let mut attempt: u32 = 0;
    loop {
        match call().await {
            Ok(response) => return Ok(response),
            Err(NornError::Provider(provider_err)) => {
                if attempt >= policy.max_retries || !policy.classifies_as_retryable(&provider_err) {
                    return Err(NornError::Provider(provider_err));
                }
                attempt = attempt.saturating_add(1);
                let next_elapsed = total_elapsed.saturating_add(backoff);
                if next_elapsed > cap {
                    return Err(NornError::Provider(provider_err));
                }
                let backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX);
                tracing::warn!(
                    attempt,
                    error = %provider_err,
                    backoff_ms,
                    "provider call failed; retrying",
                );
                tokio::time::sleep(backoff).await;
                total_elapsed = next_elapsed;
                backoff = scale_backoff(backoff, policy.backoff_multiplier);
            }
            Err(other) => return Err(other),
        }
    }
}

fn scale_backoff(current: Duration, multiplier: f64) -> Duration {
    let scaled = current.as_secs_f64() * multiplier.max(1.0);
    if scaled.is_finite() && scaled < BACKOFF_SATURATION_SECS_F64 {
        Duration::from_secs_f64(scaled)
    } else {
        Duration::from_secs(BACKOFF_SATURATION_SECS)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ok_response() -> AssembledResponse {
        AssembledResponse {
            response_items: Vec::new(),
            text: "ok".to_string(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }
    }

    #[test]
    fn default_policy_matches_brief_constants() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 2);
        assert_eq!(policy.initial_backoff, Duration::from_secs(1));
        assert!((policy.backoff_multiplier - 2.0).abs() < f64::EPSILON);
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
        // RateLimited is NOT in the default set: the provider owns 429 retry,
        // so including it here would double-retry. (Was previously asserted to
        // be present — that encoded the old, wrong double-retry contract.)
        assert!(
            !policy
                .retryable_errors
                .contains(&RetryableError::RateLimited)
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

    #[tokio::test]
    async fn retries_until_success() {
        tokio::time::pause();
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy {
            initial_backoff: Duration::from_millis(10),
            ..RetryPolicy::default()
        };
        let result = retry_with_backoff(&policy, || {
            let count = attempts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if count < 2 {
                    Err(NornError::Provider(ProviderError::StreamError {
                        reason: "HTTP 503: try again".to_string(),
                        transient: Some(TransientKind::ServerError { status: 503 }),
                    }))
                } else {
                    Ok(ok_response())
                }
            })
        })
        .await;
        assert!(result.is_ok(), "expected success, got {result:?}");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stops_after_budget() {
        tokio::time::pause();
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy {
            initial_backoff: Duration::from_millis(1),
            max_retries: 2,
            ..RetryPolicy::default()
        };
        let result = retry_with_backoff(&policy, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(NornError::Provider(ProviderError::StreamError {
                    reason: "HTTP 502: bad gateway".to_string(),
                    transient: Some(TransientKind::ServerError { status: 502 }),
                }))
            })
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_retryable_returns_immediately() {
        let attempts = AtomicUsize::new(0);
        let policy = RetryPolicy::default();
        let result = retry_with_backoff(&policy, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(NornError::Provider(ProviderError::AuthenticationFailed {
                    reason: "bad key".to_string(),
                }))
            })
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
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

    #[test]
    fn rate_limited_not_retryable_under_default() {
        let policy = RetryPolicy::default();
        let err = ProviderError::RateLimited { retry_after: None };
        assert!(!policy.classifies_as_retryable(&err));
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
}
