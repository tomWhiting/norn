//! Crate error types.
//!
//! All error enums are defined here and re-exported from their respective
//! module's `mod.rs` via `pub use`.

use std::time::Duration;

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
    #[error("hook blocked {hook_type:?}: {reason}")]
    HookBlocked {
        /// Which hook category produced the block.
        hook_type: HookType,
        /// Reason the hook supplied for the block.
        reason: String,
    },
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
#[derive(Clone, Debug, thiserror::Error)]
pub enum ProviderError {
    /// Failed to establish a connection to the provider.
    #[error("connection failed: {reason}")]
    ConnectionFailed {
        /// Description of the connection failure.
        reason: String,
    },

    /// Authentication with the provider was rejected.
    #[error("authentication failed: {reason}")]
    AuthenticationFailed {
        /// Description of the authentication failure.
        reason: String,
    },

    /// The provider returned a rate limit response.
    #[error("rate limited")]
    RateLimited {
        /// Duration to wait before retrying, if provided by the provider.
        retry_after: Option<Duration>,
    },

    /// An error occurred during the streaming response.
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
    /// [`StreamError`]: ProviderError::StreamError
    #[error("stream interrupted: {reason}")]
    StreamInterrupted {
        /// Description of how the stream was interrupted.
        reason: String,
    },

    /// Failed to parse the provider's response.
    #[error("response parse error: {reason}")]
    ResponseParseError {
        /// Description of the parse failure.
        reason: String,
    },

    /// The requested feature is not supported by this provider.
    #[error("unsupported feature: {feature}")]
    UnsupportedFeature {
        /// Name of the unsupported feature.
        feature: String,
    },

    /// The request exceeded the model's context window.
    ///
    /// Classified from a `response.failed` SSE event carrying the
    /// `context_length_exceeded` error code. Never retryable — re-sending the
    /// same oversized request cannot succeed.
    #[error("context window exceeded")]
    ContextWindowExceeded,

    /// The account has insufficient quota or billing credit.
    ///
    /// Classified from a `response.failed` SSE event carrying the
    /// `insufficient_quota` error code. Never retryable — the failure is an
    /// account-level condition that retries cannot resolve.
    #[error("quota exceeded")]
    QuotaExceeded,

    /// The provider rejected the request as invalid.
    ///
    /// Classified from a `response.failed` SSE event carrying the
    /// `invalid_prompt` or `cyber_policy` error code. Never retryable — the
    /// request content itself is the fault.
    #[error("invalid request: {message}")]
    InvalidRequest {
        /// Human-readable description of why the request was rejected.
        message: String,
    },

    /// The model stopped deterministically before completing its output:
    /// it hit its maximum output-token limit (`max_tokens`) or the
    /// provider's content filter cut the response off (`content_filter`).
    ///
    /// Never retryable — re-sending the identical request reproduces the
    /// same stop. Distinct from [`StreamInterrupted`], which covers
    /// transient transport disconnections that a retry can resolve.
    /// (Stopgap classification until truncation is surfaced as a typed
    /// step outcome in Phase 2.)
    ///
    /// [`StreamInterrupted`]: ProviderError::StreamInterrupted
    #[error("response truncated (stop_reason={stop_reason}): {reason}")]
    Truncated {
        /// Which deterministic stop cut the response off
        /// (`max_tokens` or `content_filter`).
        stop_reason: String,
        /// Human-readable description of the truncation.
        reason: String,
    },
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
    #[error("pre-validation failed: {reason}")]
    PreValidationFailed {
        /// Description of the pre-validation failure.
        reason: String,
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
