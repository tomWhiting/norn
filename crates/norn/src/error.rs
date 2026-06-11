//! Crate error types.
//!
//! All error enums are defined here and re-exported from their respective
//! module's `mod.rs` via `pub use`.
//!
//! Every error exposes a typed retry classification through
//! [`NornError::class`] / [`ProviderError::class`], returning an
//! [`ErrorClass`]. The agent loop's internal retry policy
//! ([`RetryPolicy`](crate::r#loop::retry::RetryPolicy)) is implemented in
//! terms of this public taxonomy, so embedders (e.g. durable-workflow
//! engines deciding retry-vs-terminal per activity) and the loop can never
//! disagree about what is retryable.

use std::time::Duration;

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
/// [`RetryPolicy`](crate::r#loop::retry::RetryPolicy) maps onto its
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

/// Parse the leading status code out of an `HTTP <status>: <message>`
/// reason string. Returns `0` when no parseable status is present, which
/// [`TransientKind::ServerError`] documents as "status unknown".
fn parse_status_from_reason(reason: &str) -> u16 {
    let trimmed = reason.trim_start_matches("HTTP ");
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u16>().unwrap_or(0)
}

/// Top-level error type for the norn crate.
#[derive(Debug, thiserror::Error)]
pub enum NornError {
    /// An error originating from a provider (LLM backend).
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    /// An error during schema validation or enforcement.
    #[error("schema error: {0}")]
    Schema(#[from] Box<SchemaError>),

    /// An error during tool execution or validation.
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),

    /// An error from the rules engine.
    #[error("rules error: {0}")]
    Rules(#[from] RulesError),

    /// An error from the agent registry or coordination.
    #[error("agent error: {0}")]
    Agent(#[from] AgentError),

    /// An error from the session event store or context editing.
    #[error("session error: {0}")]
    Session(#[from] SessionError),

    /// An error from an integration layer.
    #[error("integration error: {0}")]
    Integration(#[from] IntegrationError),

    /// A configuration error.
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    /// An error from the skill loader.
    #[error("skill error: {0}")]
    Skill(#[from] SkillError),

    /// A lifecycle hook vetoed the operation.
    ///
    /// Classification: [`ErrorClass::Terminal`] — a hook block is a
    /// deliberate policy veto; replaying the same operation re-triggers it.
    #[error("hook blocked {hook_type:?}: {reason}")]
    HookBlocked {
        /// Which hook category produced the block.
        hook_type: HookType,
        /// Reason the hook supplied for the block.
        reason: String,
    },
}

impl NornError {
    /// Typed retry classification of this error.
    ///
    /// [`NornError::Provider`] delegates to [`ProviderError::class`] —
    /// transport-level provider faults are the only errors that classify
    /// as retryable. Every other variant ([`Schema`](Self::Schema),
    /// [`Tool`](Self::Tool), [`Rules`](Self::Rules),
    /// [`Agent`](Self::Agent), [`Session`](Self::Session),
    /// [`Integration`](Self::Integration), [`Config`](Self::Config),
    /// [`Skill`](Self::Skill), [`HookBlocked`](Self::HookBlocked)) is
    /// [`ErrorClass::Terminal`]: they describe deterministic faults in the
    /// request, configuration, local state, or operator policy that
    /// re-running the identical operation cannot resolve. This mirrors the
    /// agent loop's own retry behaviour, which only ever retries provider
    /// errors.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Provider(provider_err) => provider_err.class(),
            Self::Schema(_)
            | Self::Tool(_)
            | Self::Rules(_)
            | Self::Agent(_)
            | Self::Session(_)
            | Self::Integration(_)
            | Self::Config(_)
            | Self::Skill(_)
            | Self::HookBlocked { .. } => ErrorClass::Terminal,
        }
    }

    /// Whether a retry of the failed operation can make progress.
    /// Shorthand for `self.class().is_retryable()`.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.class().is_retryable()
    }
}

/// Identifies which lifecycle hook category vetoed an operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookType {
    /// A pre-tool hook blocked the call.
    PreTool,
    /// A [`PreLlmHook`](crate::integration::hooks::PreLlmHook) blocked the call.
    PreLlm,
    /// A user-prompt hook blocked the call.
    UserPrompt,
    /// A stop hook blocked the call.
    Stop,
    /// A sub-agent stop hook blocked the call.
    SubagentStop,
    /// A pre-compaction hook blocked the call.
    PreCompaction,
}

/// Errors originating from LLM provider interactions.
///
/// Model truncation (`max_tokens` / `content_filter` stops) is **not** an
/// error: a truncated run is a stopped run with partial output, surfaced
/// as [`AgentStepResult::Truncated`](crate::r#loop::config::AgentStepResult)
/// and [`AgentStopReason::Truncated`](crate::agent::AgentStopReason).
#[derive(Clone, Debug, thiserror::Error)]
pub enum ProviderError {
    /// Failed to establish a connection to the provider.
    ///
    /// Classification: [`ErrorClass::Retryable`] —
    /// [`TransientKind::Timeout`] when the reason mentions a timeout,
    /// otherwise [`TransientKind::ConnectionReset`].
    #[error("connection failed: {reason}")]
    ConnectionFailed {
        /// Description of the connection failure.
        reason: String,
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

    /// The provider returned a rate limit response.
    ///
    /// Classification: [`ErrorClass::RateLimited`], carrying `retry_after`
    /// so engines can honour the provider's delay hint. The loop's default
    /// [`RetryPolicy`](crate::r#loop::retry::RetryPolicy) deliberately does
    /// **not** retry it (the provider layer owns 429 retry internally), but
    /// the class tells embedders a delayed retry can succeed.
    #[error("rate limited")]
    RateLimited {
        /// Duration to wait before retrying, if provided by the provider.
        retry_after: Option<Duration>,
    },

    /// An error occurred during the streaming response.
    ///
    /// Classification: [`ErrorClass::Retryable`] with
    /// [`TransientKind::ServerError`] for `HTTP 5xx`-prefixed reasons and
    /// [`TransientKind::Timeout`] for timeout reasons; every other stream
    /// error is [`ErrorClass::Terminal`] (the provider itself reported a
    /// non-transient failure).
    #[error("stream error: {reason}")]
    StreamError {
        /// Description of the stream failure.
        reason: String,
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

    /// The requested feature is not supported by this provider.
    ///
    /// Classification: [`ErrorClass::Terminal`] — capability gaps do not
    /// resolve on retry.
    #[error("unsupported feature: {feature}")]
    UnsupportedFeature {
        /// Name of the unsupported feature.
        feature: String,
    },

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
}

impl ProviderError {
    /// Typed retry classification of this provider error.
    ///
    /// This is the single source of truth for retryability: the agent
    /// loop's [`RetryPolicy`](crate::r#loop::retry::RetryPolicy) derives
    /// its internal categorisation from this method, and embedders use it
    /// to drive their own retry-vs-terminal decisions. Per-variant
    /// classifications are documented on each variant.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::ConnectionFailed { reason } => ErrorClass::Retryable {
                kind: if reason.to_lowercase().contains("timed out") {
                    TransientKind::Timeout
                } else {
                    TransientKind::ConnectionReset
                },
            },
            Self::StreamInterrupted { .. } => ErrorClass::Retryable {
                kind: TransientKind::ConnectionReset,
            },
            Self::StreamError { reason } => {
                if reason.starts_with("HTTP 5") {
                    ErrorClass::Retryable {
                        kind: TransientKind::ServerError {
                            status: parse_status_from_reason(reason),
                        },
                    }
                } else if reason.to_lowercase().contains("timed out") {
                    ErrorClass::Retryable {
                        kind: TransientKind::Timeout,
                    }
                } else {
                    ErrorClass::Terminal
                }
            }
            Self::RateLimited { retry_after } => ErrorClass::RateLimited {
                retry_after: *retry_after,
            },
            Self::AuthenticationFailed { .. } => ErrorClass::Auth,
            Self::ResponseParseError { .. }
            | Self::UnsupportedFeature { .. }
            | Self::ContextWindowExceeded
            | Self::QuotaExceeded
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

/// Errors during schema validation and enforcement.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// Model output did not conform to the declared schema.
    #[error("validation failed: {errors:?}")]
    ValidationFailed {
        /// The expected JSON schema.
        schema: serde_json::Value,
        /// The actual model output that failed validation.
        output: serde_json::Value,
        /// Validation error descriptions.
        errors: Vec<String>,
    },

    /// Schema enforcement exhausted its retry budget.
    #[error("schema unreachable after {attempts} attempts: {validation_errors:?}")]
    Unreachable {
        /// The best output produced across all attempts, if any.
        best_attempt: Option<serde_json::Value>,
        /// Validation errors from the final attempt.
        validation_errors: Vec<String>,
        /// Total number of attempts made.
        attempts: u32,
    },

    /// The schema itself is malformed or invalid.
    #[error("invalid schema: {reason}")]
    InvalidSchema {
        /// Description of why the schema is invalid.
        reason: String,
    },
}

/// Errors during tool execution or validation.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// A pre-validation check prevented tool execution.
    ///
    /// Carries the full structured payload (machine-readable kind, the
    /// model-facing message, and free-form detail — including any guidance
    /// folded under `detail.guidance`) so the block survives into the
    /// dispatch path and the persisted `ToolResult` event without
    /// collapsing to a string. `Display` renders the model-facing
    /// message-plus-guidance so logs stay readable.
    #[error("pre-validation failed: {}", .payload.model_message())]
    PreValidationFailed {
        /// Structured description of the pre-validation block.
        payload: crate::tool::failure::ToolErrorPayload,
    },

    /// The tool's execution phase failed.
    #[error("execution failed: {reason}")]
    ExecutionFailed {
        /// Description of the execution failure.
        reason: String,
    },

    /// A post-validation check failed after execution.
    #[error("post-validation failed: {reason}")]
    PostValidationFailed {
        /// Description of the post-validation failure.
        reason: String,
        /// Structured tool output captured before the post-validation check
        /// failed; `None` when no commit happened (currently always `Some`
        /// because both Gate sites are post-execute).
        committed_output: Option<serde_json::Value>,
    },

    /// The requested tool was not found in the registry.
    #[error("tool not found: {name}")]
    ToolNotFound {
        /// Name of the tool that was not found.
        name: String,
    },

    /// A required [`ToolContext`](crate::tool::context::ToolContext)
    /// extension was not published by the embedder.
    ///
    /// Produced by
    /// [`ToolContext::require_extension`](crate::tool::context::ToolContext::require_extension);
    /// `extension` is the full Rust type name of the missing extension.
    #[error("required tool-context extension not configured: {extension}")]
    MissingExtension {
        /// Full type name of the extension that was not configured.
        extension: String,
    },
}

impl ToolError {
    /// Build a [`ToolError::PreValidationFailed`] carrying a typed payload
    /// with the given kind and message and no detail.
    ///
    /// Shorthand for the common construct-site shape; blocks that carry
    /// guidance or detail go through
    /// [`BlockDecision`](crate::tool::lifecycle::BlockDecision) and its
    /// `From<BlockDecision> for ToolError` conversion instead.
    #[must_use]
    pub fn pre_validation(
        kind: crate::tool::failure::ToolErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self::PreValidationFailed {
            payload: crate::tool::failure::ToolErrorPayload::new(kind, message),
        }
    }
}

/// Errors from the agent registry and coordination.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Failed to spawn an agent.
    #[error("spawn failed: {reason}")]
    SpawnFailed {
        /// Description of the spawn failure.
        reason: String,
    },

    /// The referenced agent was not found.
    #[error("agent not found: {path}")]
    NotFound {
        /// Agent path that was not found.
        path: String,
    },

    /// The agent's mailbox channel was closed.
    #[error("mailbox closed for agent: {path}")]
    MailboxClosed {
        /// Agent path whose mailbox is closed.
        path: String,
    },

    /// An invalid agent path was provided.
    #[error("invalid agent path: {path}")]
    PathInvalid {
        /// The invalid path.
        path: String,
    },
}

/// Errors from the session event store and context editing.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Failed to append an event to the store.
    #[error("event append failed: {reason}")]
    EventAppendFailed {
        /// Description of the append failure.
        reason: String,
    },

    /// An invalid event ID was referenced.
    #[error("invalid event ID: {id}")]
    InvalidEventId {
        /// The invalid event ID.
        id: String,
    },

    /// A storage-level error occurred.
    #[error("storage error: {reason}")]
    StorageError {
        /// Description of the storage failure.
        reason: String,
    },
}

/// Errors from the rules engine.
#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    /// Failed to parse a rule definition.
    #[error("rule parse failed: {reason}")]
    ParseFailed {
        /// Description of the parse failure.
        reason: String,
    },

    /// An error occurred evaluating a trigger condition.
    #[error("trigger evaluation error: {reason}")]
    TriggerEvalError {
        /// Description of the trigger evaluation failure.
        reason: String,
    },

    /// Failed to deliver a rule via its configured delivery mode.
    #[error("rule delivery failed: {reason}")]
    DeliveryFailed {
        /// Description of the delivery failure.
        reason: String,
    },
}

/// Errors from integration layers.
#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    /// An error from the Claude Runner integration.
    #[error("Claude Runner error: {reason}")]
    ClaudeRunnerError {
        /// Description of the Claude Runner failure.
        reason: String,
    },

    /// An error from the MCP client or server.
    #[error("MCP error: {reason}")]
    McpError {
        /// Description of the MCP failure.
        reason: String,
    },

    /// An error from the Rhai scripting integration.
    #[error("Rhai error: {reason}")]
    RhaiError {
        /// Description of the Rhai failure.
        reason: String,
    },

    /// An error from a lifecycle hook.
    #[error("hook error: {reason}")]
    HookError {
        /// Description of the hook failure.
        reason: String,
    },
}

/// Errors from loading or parsing a skill file.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// The frontmatter delimiters were missing or malformed.
    #[error("frontmatter error: {0}")]
    Frontmatter(#[from] crate::util::FrontmatterError),

    /// The YAML frontmatter did not deserialize cleanly.
    #[error("invalid YAML frontmatter: {reason}")]
    YamlParse {
        /// Description of the YAML parse failure.
        reason: String,
    },

    /// A filesystem operation against the skill file failed.
    #[error("skill I/O error: {reason}")]
    Io {
        /// Description of the I/O failure.
        reason: String,
    },

    /// The frontmatter parsed but the required `description` was missing.
    #[error("skill is missing a description")]
    MissingDescription,

    /// The skill path could not be interpreted as a UTF-8 name.
    #[error("invalid skill path: {reason}")]
    InvalidPath {
        /// Description of why the path is invalid.
        reason: String,
    },
}

/// Configuration errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The configuration is invalid.
    #[error("invalid config: {reason}")]
    InvalidConfig {
        /// Description of why the configuration is invalid.
        reason: String,
    },

    /// A required configuration field is missing.
    #[error("missing field: {field}")]
    MissingField {
        /// Name of the missing field.
        field: String,
    },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn retryable(kind: TransientKind) -> ErrorClass {
        ErrorClass::Retryable { kind }
    }

    // -- ProviderError: pinned classification per variant -------------------

    #[test]
    fn connection_failed_timeout_classifies_retryable_timeout() {
        let err = ProviderError::ConnectionFailed {
            reason: "request timed out after 30s".to_string(),
        };
        assert_eq!(err.class(), retryable(TransientKind::Timeout));
        assert!(err.is_retryable());
    }

    #[test]
    fn connection_failed_other_classifies_retryable_reset() {
        let err = ProviderError::ConnectionFailed {
            reason: "connection refused".to_string(),
        };
        assert_eq!(err.class(), retryable(TransientKind::ConnectionReset));
    }

    #[test]
    fn stream_interrupted_classifies_retryable_reset() {
        let err = ProviderError::StreamInterrupted {
            reason: "body ended mid-stream".to_string(),
        };
        assert_eq!(err.class(), retryable(TransientKind::ConnectionReset));
    }

    #[test]
    fn stream_error_5xx_classifies_retryable_server_error_with_status() {
        let err = ProviderError::StreamError {
            reason: "HTTP 503: service unavailable".to_string(),
        };
        assert_eq!(
            err.class(),
            retryable(TransientKind::ServerError { status: 503 })
        );
    }

    #[test]
    fn stream_error_5xx_without_parseable_status_uses_zero() {
        // "HTTP 5" prefix matches but the digits do not parse as one code.
        let err = ProviderError::StreamError {
            reason: "HTTP 5xx upstream failure".to_string(),
        };
        assert_eq!(
            err.class(),
            retryable(TransientKind::ServerError { status: 5 })
        );
    }

    #[test]
    fn stream_error_timeout_classifies_retryable_timeout() {
        let err = ProviderError::StreamError {
            reason: "read timed out waiting for chunk".to_string(),
        };
        assert_eq!(err.class(), retryable(TransientKind::Timeout));
    }

    #[test]
    fn stream_error_other_classifies_terminal() {
        let err = ProviderError::StreamError {
            reason: "provider stream ended without a Done event".to_string(),
        };
        assert_eq!(err.class(), ErrorClass::Terminal);
        assert!(!err.is_retryable());
    }

    #[test]
    fn rate_limited_classifies_rate_limited_with_delay_hint() {
        let err = ProviderError::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        };
        let class = err.class();
        assert_eq!(
            class,
            ErrorClass::RateLimited {
                retry_after: Some(Duration::from_secs(30)),
            }
        );
        assert!(class.is_retryable());
        assert_eq!(class.retry_after(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn rate_limited_without_delay_still_classifies_rate_limited() {
        let err = ProviderError::RateLimited { retry_after: None };
        assert_eq!(err.class(), ErrorClass::RateLimited { retry_after: None });
        assert_eq!(err.class().retry_after(), None);
    }

    #[test]
    fn authentication_failed_classifies_auth() {
        let err = ProviderError::AuthenticationFailed {
            reason: "401 Unauthorized".to_string(),
        };
        assert_eq!(err.class(), ErrorClass::Auth);
        assert!(!err.is_retryable());
    }

    #[test]
    fn response_parse_error_classifies_terminal() {
        let err = ProviderError::ResponseParseError {
            reason: "unexpected JSON shape".to_string(),
        };
        assert_eq!(err.class(), ErrorClass::Terminal);
    }

    #[test]
    fn unsupported_feature_classifies_terminal() {
        let err = ProviderError::UnsupportedFeature {
            feature: "custom_grammar".to_string(),
        };
        assert_eq!(err.class(), ErrorClass::Terminal);
    }

    #[test]
    fn context_window_exceeded_classifies_terminal() {
        assert_eq!(
            ProviderError::ContextWindowExceeded.class(),
            ErrorClass::Terminal
        );
    }

    #[test]
    fn quota_exceeded_classifies_terminal() {
        assert_eq!(ProviderError::QuotaExceeded.class(), ErrorClass::Terminal);
    }

    #[test]
    fn invalid_request_classifies_terminal() {
        let err = ProviderError::InvalidRequest {
            message: "bad prompt".to_string(),
        };
        assert_eq!(err.class(), ErrorClass::Terminal);
    }

    // -- NornError: provider delegation, everything else terminal -----------

    #[test]
    fn norn_error_delegates_provider_classification() {
        let err = NornError::Provider(ProviderError::StreamInterrupted {
            reason: "reset".to_string(),
        });
        assert_eq!(err.class(), retryable(TransientKind::ConnectionReset));
        assert!(err.is_retryable());
    }

    #[test]
    fn norn_error_non_provider_variants_classify_terminal() {
        let cases: Vec<NornError> = vec![
            NornError::Schema(Box::new(SchemaError::InvalidSchema {
                reason: "not an object".to_string(),
            })),
            NornError::Tool(ToolError::ToolNotFound {
                name: "missing".to_string(),
            }),
            NornError::Rules(RulesError::ParseFailed {
                reason: "bad rule".to_string(),
            }),
            NornError::Agent(AgentError::NotFound {
                path: "/x".to_string(),
            }),
            NornError::Session(SessionError::StorageError {
                reason: "disk gone".to_string(),
            }),
            NornError::Integration(IntegrationError::HookError {
                reason: "hook failed".to_string(),
            }),
            NornError::Config(ConfigError::MissingField {
                field: "model".to_string(),
            }),
            NornError::Skill(SkillError::MissingDescription),
            NornError::HookBlocked {
                hook_type: HookType::PreTool,
                reason: "policy".to_string(),
            },
        ];
        for err in cases {
            assert_eq!(err.class(), ErrorClass::Terminal, "{err}");
            assert!(!err.is_retryable(), "{err}");
        }
    }

    // -- ErrorClass: serialization round-trips across boundaries ------------

    #[test]
    fn error_class_serde_round_trips_every_shape() {
        let cases = vec![
            ErrorClass::Retryable {
                kind: TransientKind::Timeout,
            },
            ErrorClass::Retryable {
                kind: TransientKind::ConnectionReset,
            },
            ErrorClass::Retryable {
                kind: TransientKind::ServerError { status: 502 },
            },
            ErrorClass::RateLimited {
                retry_after: Some(Duration::from_millis(1500)),
            },
            ErrorClass::RateLimited { retry_after: None },
            ErrorClass::Auth,
            ErrorClass::Terminal,
        ];
        for class in cases {
            let json = serde_json::to_string(&class).expect("serialize");
            let back: ErrorClass = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, class, "round trip failed for {json}");
        }
    }

    #[test]
    fn error_class_serializes_with_stable_tag() {
        let json = serde_json::to_value(ErrorClass::Retryable {
            kind: TransientKind::ServerError { status: 503 },
        })
        .expect("serialize");
        assert_eq!(json["class"], "retryable");
        assert_eq!(json["kind"]["kind"], "server_error");
        assert_eq!(json["kind"]["status"], 503);

        let json = serde_json::to_value(ErrorClass::Terminal).expect("serialize");
        assert_eq!(json["class"], "terminal");
    }
}
