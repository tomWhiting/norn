//! Crate error types.
//!
//! Error enums are defined in this module family and re-exported from their
//! respective module's `mod.rs` via `pub use`.
//!
//! Every error exposes a typed retry classification through
//! [`NornError::class`] / [`ProviderError::class`], returning an
//! [`ErrorClass`]. The agent loop's internal retry policy
//! ([`RetryPolicy`](crate::agent_loop::retry::RetryPolicy)) is implemented in
//! terms of this public taxonomy, so embedders (e.g. durable-workflow
//! engines deciding retry-vs-terminal per activity) and the loop can never
//! disagree about what is retryable.

use std::time::Duration;

mod subsystems;
pub use subsystems::*;

/// Typed retry classification of an error.
///
/// Serializable so the classification can cross process and
/// activity boundaries (e.g. a durable-workflow engine recording why an
/// agent activity failed and whether its engine should retry it).
///
/// The four classes are exactly what norn's loop-level classifier
/// distinguishes: transient transport faults (retry with backoff),
/// provider rate limits (retry after the server-supplied delay),
/// authentication failures (terminal until credentials change), and
/// deterministic faults (retrying the identical request cannot succeed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum ErrorClass {
    /// Transient failure — safe to retry with backoff.
    Retryable {
        /// The transport-level category of the transient failure.
        kind: TransientKind,
    },

    /// The provider rate-limited the request — retry after the
    /// server-supplied delay, when one was provided.
    RateLimited {
        /// Provider-supplied delay before the request should be retried.
        retry_after: Option<Duration>,
    },

    /// Authentication or authorization failure — terminal until the
    /// credentials change; retrying with the same credentials cannot
    /// succeed.
    Auth,

    /// Deterministic failure — retrying the identical request cannot
    /// succeed (invalid input, exhausted quota, oversized context,
    /// configuration faults, policy vetoes, …).
    Terminal,
}

impl ErrorClass {
    /// Whether a retry can make progress.
    ///
    /// `true` for [`ErrorClass::Retryable`] (retry with backoff) and
    /// [`ErrorClass::RateLimited`] (retry after the delay); `false` for
    /// [`ErrorClass::Auth`] and [`ErrorClass::Terminal`].
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable { .. } | Self::RateLimited { .. })
    }

    /// The provider-supplied retry delay, when the error is
    /// [`ErrorClass::RateLimited`] and the provider sent one.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            Self::Retryable { .. } | Self::Auth | Self::Terminal => None,
        }
    }
}

/// Transport-level category of a [`ErrorClass::Retryable`] failure.
///
/// This is the single source of truth the loop's
/// [`RetryPolicy`](crate::agent_loop::retry::RetryPolicy) maps onto its
/// configurable retryable-error set, so policy filtering and the public
/// taxonomy can never drift apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransientKind {
    /// Network-level timeout (connect or read).
    Timeout,
    /// Mid-stream connection reset or transport disconnect.
    ConnectionReset,
    /// HTTP 5xx server error.
    ServerError {
        /// HTTP status code; `0` when the status could not be parsed
        /// from the provider's error message.
        status: u16,
    },
}

/// Structured failure modes for a locally managed OAuth credential.
///
/// These outcomes all require the credential owner to reconcile local state
/// before the request can safely proceed, but they are deliberately distinct:
/// callers must not mistake a persistence failure or concurrent write for an
/// authority rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthCredentialFailureKind {
    /// The credential is no longer accepted and login is required.
    Permanent,
    /// A rotated credential was not durably accepted by its owner.
    Undurable,
    /// The credential changed while an operation was in flight.
    Conflict,
    /// The authority may have rotated the credential without returning a
    /// lineage that can be accepted safely.
    Indeterminate,
}

impl std::fmt::Display for OAuthCredentialFailureKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Permanent => "permanent",
            Self::Undurable => "undurable",
            Self::Conflict => "conflict",
            Self::Indeterminate => "indeterminate",
        };
        formatter.write_str(label)
    }
}

/// Errors originating from LLM provider interactions.
///
/// Model truncation (`max_tokens` / `content_filter` stops) is **not** an
/// error: a truncated run is a stopped run with partial output, surfaced
/// as [`AgentStepResult::Truncated`](crate::agent_loop::config::AgentStepResult)
/// and [`AgentStopReason::Truncated`](crate::agent::AgentStopReason).
#[derive(Clone, Debug, thiserror::Error)]
pub enum ProviderError {
    /// Failed to establish a connection to the provider.
    ///
    /// Classification: [`ErrorClass::Retryable`] with the carried `kind`.
    /// The producer sets the kind structurally at the failure site
    /// ([`TransientKind::Timeout`] for connect/header deadlines,
    /// [`TransientKind::ConnectionReset`] for every other transport-level
    /// connection fault) — classification never inspects the reason text.
    #[error("connection failed: {reason}")]
    ConnectionFailed {
        /// Description of the connection failure.
        reason: String,
        /// Structured transport category of the failure, set by the
        /// producer.
        kind: TransientKind,
    },

    /// Authentication with the provider was rejected.
    ///
    /// Classification: [`ErrorClass::Auth`] — terminal until the
    /// credentials change.
    #[error("authentication failed: {reason}")]
    AuthenticationFailed {
        /// Description of the authentication failure.
        reason: String,
    },

    /// A locally managed OAuth credential could not be used safely.
    ///
    /// Classification: [`ErrorClass::Auth`] because the credential owner must
    /// reconcile or replace local state before this request can proceed. The
    /// structured `kind` preserves the operation outcome across the provider
    /// boundary instead of flattening it into an authentication string.
    #[error("OAuth credential failure ({kind}): {reason}")]
    OAuthCredentialFailure {
        /// Credential lifecycle outcome.
        kind: OAuthCredentialFailureKind,
        /// Non-disclosing description of the failure.
        reason: String,
    },

    /// A credential-bearing request received a redirect response.
    ///
    /// Redirects are disabled so credentials, account headers, and request
    /// bodies cannot be replayed to another destination. The response's
    /// `Location` and body are deliberately absent from this error.
    ///
    /// Classification: [`ErrorClass::Terminal`] — retrying the same request
    /// cannot override the local redirect policy.
    #[error(
        "credential-bearing {backend} request received HTTP {status}; redirects are not followed by policy"
    )]
    RedirectPolicyRefused {
        /// HTTP redirect status returned by the original destination.
        status: u16,
        /// Locally authored backend label.
        backend: &'static str,
    },

    /// The provider returned a rate limit response.
    ///
    /// Classification: [`ErrorClass::RateLimited`], carrying `retry_after`
    /// so engines can honour the provider's delay hint. The loop's default
    /// [`RetryPolicy`](crate::agent_loop::retry::RetryPolicy) deliberately does
    /// **not** retry it (the provider layer owns 429 retry internally), but
    /// the class tells embedders a delayed retry can succeed.
    #[error("rate limited")]
    RateLimited {
        /// Duration to wait before retrying, if provided by the provider.
        retry_after: Option<Duration>,
    },

    /// An error occurred during the streaming response.
    ///
    /// Classification: [`ErrorClass::Retryable`] with the carried
    /// [`TransientKind`] when `transient` is `Some`, otherwise
    /// [`ErrorClass::Terminal`] (the provider itself reported a
    /// non-transient failure). The producer sets `transient` structurally
    /// at the failure site — an HTTP 5xx status, a stall-deadline expiry —
    /// so classification never inspects the reason text.
    #[error("stream error: {reason}")]
    StreamError {
        /// Description of the stream failure.
        reason: String,
        /// Structured transient classification set by the producer.
        /// `Some(kind)` marks the error retryable with that transport
        /// category; `None` marks it terminal.
        transient: Option<TransientKind>,
    },

    /// The stream was interrupted after at least one chunk was received.
    ///
    /// Distinct from [`StreamError`], which covers errors signalled by the
    /// provider itself (e.g. `response.failed` SSE frames). `StreamInterrupted`
    /// covers transport-level disconnections mid-stream — the HTTP body ended
    /// before the provider's terminal event arrived.
    ///
    /// Classification: [`ErrorClass::Retryable`] with
    /// [`TransientKind::ConnectionReset`].
    ///
    /// [`StreamError`]: ProviderError::StreamError
    #[error("stream interrupted: {reason}")]
    StreamInterrupted {
        /// Description of how the stream was interrupted.
        reason: String,
    },

    /// Failed to parse the provider's response.
    ///
    /// Classification: [`ErrorClass::Terminal`] — a response the client
    /// cannot parse indicates a contract mismatch, not a transient fault;
    /// blind retries would loop on the same malformed shape.
    #[error("response parse error: {reason}")]
    ResponseParseError {
        /// Description of the parse failure.
        reason: String,
    },

    /// The outgoing request could not be serialized or assembled into the
    /// provider's wire shape (payload serialization failure, a replay item
    /// missing its correlation identifier, a malformed payload object).
    ///
    /// Classification: [`ErrorClass::Terminal`] — the fault is in the
    /// locally-constructed request; re-sending the identical request
    /// cannot succeed.
    #[error("request serialization failed: {reason}")]
    RequestSerializationFailed {
        /// Description of the serialization failure.
        reason: String,
    },

    /// The requested feature is not supported by this provider.
    ///
    /// Classification: [`ErrorClass::Terminal`] — capability gaps do not
    /// resolve on retry.
    #[error("unsupported feature: {feature}")]
    UnsupportedFeature {
        /// Name of the unsupported feature.
        feature: String,
    },

    /// The Responses stream carried an event outside the pinned public and
    /// Codex manifests.
    ///
    /// The authority-controlled discriminator is deliberately absent from
    /// this error. The lossless raw envelope is emitted separately before
    /// this terminal outcome.
    #[error("unsupported Responses stream event")]
    UnsupportedResponseEvent,

    /// The Responses output requested client-side action that Norn does not
    /// implement end to end, or used an unknown output-item discriminator.
    ///
    /// The lossless item is emitted on the canonical event lane before this
    /// terminal outcome; its provider-controlled discriminator is not copied
    /// into ordinary error text.
    #[error("unsupported Responses output item")]
    UnsupportedResponseItem,

    /// The Responses stream carried response-scoped media for which Norn has
    /// no persisted canonical artifact yet.
    ///
    /// The raw event is emitted before this terminal outcome. Keeping this
    /// distinct from [`UnsupportedResponseEvent`](Self::UnsupportedResponseEvent)
    /// records that the event is known while its end-to-end media contract is
    /// not implemented.
    #[error("unsupported Responses media output")]
    UnsupportedResponseMedia,

    /// A pinned Responses event violated identity, sequencing, completion,
    /// or terminal reconciliation rules.
    #[error("Responses protocol violation: {source}")]
    ResponseProtocolViolation {
        /// Typed non-disclosing reconciliation failure.
        #[source]
        source: crate::provider::openai::response_reconciler::ResponseReconciliationError,
    },

    /// Credential-scoped provider state was requested without a stable identity.
    ///
    /// No identity or credential material is carried by this error.
    #[error("credential-scoped provider state requires a stable provider identity")]
    ProviderStateIdentityRequired,

    /// Provider state is already bound to another credential or authority.
    ///
    /// No identity or credential material is carried by this error.
    #[error("provider state belongs to a different credential or authority")]
    ProviderStateIdentityMismatch,

    /// Provider reasoning state cannot be reconstructed for a full replay.
    ///
    /// The error deliberately carries no response ID, reasoning summary, item
    /// identifier, or encrypted payload. Retrying the same local view cannot
    /// restore the missing provider state.
    #[error("provider reasoning state is unavailable for full replay")]
    ProviderStateReplayUnavailable,

    /// Durable provider-state provenance is malformed or internally inconsistent.
    ///
    /// The error deliberately carries no event ID, response ID, payload, or
    /// storage disposition. Retrying cannot make an invalid local timeline safe.
    #[error("provider state provenance is invalid")]
    ProviderStateProvenanceInvalid,

    /// The request exceeded the model's context window.
    ///
    /// Classified from a `response.failed` SSE event carrying the
    /// `context_length_exceeded` error code.
    ///
    /// Classification: [`ErrorClass::Terminal`] — re-sending the same
    /// oversized request cannot succeed.
    #[error("context window exceeded")]
    ContextWindowExceeded,

    /// The account has insufficient quota or billing credit.
    ///
    /// Classified from a `response.failed` SSE event carrying the
    /// `insufficient_quota` error code.
    ///
    /// Classification: [`ErrorClass::Terminal`] — the failure is an
    /// account-level condition that retries cannot resolve.
    #[error("quota exceeded")]
    QuotaExceeded,

    /// The provider rejected the request as invalid.
    ///
    /// Classified from a `response.failed` SSE event carrying the
    /// `invalid_prompt` or `cyber_policy` error code.
    ///
    /// Classification: [`ErrorClass::Terminal`] — the request content
    /// itself is the fault.
    #[error("invalid request: {message}")]
    InvalidRequest {
        /// Human-readable description of why the request was rejected.
        message: String,
    },

    /// The process-wide safe descriptor budget cannot admit this provider.
    #[error(transparent)]
    DescriptorAdmission(Box<crate::resource::DescriptorAdmissionError>),
}

impl ProviderError {
    /// Typed retry classification of this provider error.
    ///
    /// This is the single source of truth for retryability: the agent
    /// loop's [`RetryPolicy`](crate::agent_loop::retry::RetryPolicy) derives
    /// its internal categorisation from this method, and embedders use it
    /// to drive their own retry-vs-terminal decisions. Per-variant
    /// classifications are documented on each variant.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::ConnectionFailed { kind, .. } => ErrorClass::Retryable { kind: *kind },
            Self::StreamInterrupted { .. } => ErrorClass::Retryable {
                kind: TransientKind::ConnectionReset,
            },
            Self::StreamError { transient, .. } => {
                transient.map_or(ErrorClass::Terminal, |kind| ErrorClass::Retryable { kind })
            }
            Self::RateLimited { retry_after } => ErrorClass::RateLimited {
                retry_after: *retry_after,
            },
            Self::AuthenticationFailed { .. } | Self::OAuthCredentialFailure { .. } => {
                ErrorClass::Auth
            }
            Self::ResponseParseError { .. }
            | Self::RequestSerializationFailed { .. }
            | Self::UnsupportedFeature { .. }
            | Self::UnsupportedResponseEvent
            | Self::UnsupportedResponseItem
            | Self::UnsupportedResponseMedia
            | Self::ResponseProtocolViolation { .. }
            | Self::ProviderStateIdentityRequired
            | Self::ProviderStateIdentityMismatch
            | Self::ProviderStateReplayUnavailable
            | Self::ProviderStateProvenanceInvalid
            | Self::RedirectPolicyRefused { .. }
            | Self::ContextWindowExceeded
            | Self::QuotaExceeded
            | Self::DescriptorAdmission(_)
            | Self::InvalidRequest { .. } => ErrorClass::Terminal,
        }
    }

    /// Whether a retry of the provider call can make progress.
    /// Shorthand for `self.class().is_retryable()`.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.class().is_retryable()
    }
}

#[cfg(test)]
mod tests;
