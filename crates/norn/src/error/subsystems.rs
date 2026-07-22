//! Non-provider error families exposed by [`crate::error`].

use std::fmt;

use super::{ErrorClass, ProviderError};

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
    /// Classification: [`ErrorClass::Terminal`] - a hook block is a deliberate
    /// policy veto; replaying the same operation re-triggers it.
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
    /// [`NornError::Provider`] delegates to [`ProviderError::class`] -
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
    /// model-facing message, and free-form detail - including any guidance
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

    /// The process or system descriptor pool was exhausted.
    #[error(transparent)]
    DescriptorExhausted(Box<crate::resource::DescriptorExhaustion>),

    /// Norn's safe active-descriptor budget could not admit the operation.
    #[error(transparent)]
    DescriptorAdmission(Box<crate::resource::DescriptorAdmissionError>),

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

    /// Durable pending-message authority is malformed or contradictory.
    #[error("pending-message replay is invalid: {reason}")]
    PendingMessageReplayInvalid {
        /// Payload-free description of the violated durable invariant.
        reason: String,
    },

    /// A pre-D8 queue row cannot be assigned to this durable mailbox.
    #[error(
        "session resume requires operator action: an unresolved pre-D8 pending agent-message record has no durable mailbox ownership"
    )]
    PreD8PendingMessageOwnershipUnknown,

    /// Terminal teardown retained accepted messages whose queue write failed.
    #[error(
        "terminal agent-message persistence is unresolved for {pending_count} accepted message(s); retry through the pending-message recovery authority"
    )]
    TerminalPendingMessagesUnresolved {
        /// Number of exact queued records retained for durable retry.
        pending_count: usize,
    },

    /// Credential-scoped session state has no stable provider identity.
    #[error("credential-scoped session state requires a stable provider identity")]
    ProviderStateIdentityRequired,

    /// Session provider state belongs to another credential or authority.
    #[error("session provider state belongs to a different credential or authority")]
    ProviderStateIdentityMismatch,

    /// The process or system descriptor pool was exhausted.
    #[error(transparent)]
    DescriptorExhausted(Box<crate::resource::DescriptorExhaustion>),
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

    /// A direct MCP client configuration supplied an invalid connection bound.
    #[error("invalid MCP client setting '{setting}': value must be positive")]
    McpInvalidClientSetting {
        /// Stable local setting name; never server-controlled content.
        setting: &'static str,
    },

    /// An inbound MCP frame exceeded its configured per-message byte bound.
    #[error("MCP {transport} inbound message exceeded the {limit_bytes}-byte limit")]
    McpInboundMessageTooLarge {
        /// Local transport label; never server-controlled content.
        transport: &'static str,
        /// Configured maximum accepted message size.
        limit_bytes: usize,
    },

    /// An MCP request exceeded its explicitly configured deadline.
    #[error("MCP {transport} request exceeded the configured {timeout_ms} ms deadline")]
    McpRequestTimedOut {
        /// Local transport label; never server-controlled content.
        transport: &'static str,
        /// Configured request timeout in milliseconds.
        timeout_ms: u64,
    },

    /// A structured error returned by a remote MCP server.
    #[error("{0}")]
    McpRemote(
        #[from]
        #[source]
        McpRemoteError,
    ),

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

/// A JSON-RPC error returned by a remote MCP server.
///
/// The remote message is retained as private, non-serializable state, but is
/// intentionally excluded from default error and debug rendering because a
/// server can echo configured credentials into it.
pub struct McpRemoteError {
    code: i64,
    message: String,
}

impl McpRemoteError {
    pub(crate) const fn new(code: i64, message: String) -> Self {
        Self { code, message }
    }

    /// Numeric JSON-RPC error code supplied by the server.
    #[must_use]
    pub const fn code(&self) -> i64 {
        self.code
    }

    #[cfg(test)]
    pub(crate) const fn untrusted_message(&self) -> &str {
        self.message.as_str()
    }
}

impl fmt::Debug for McpRemoteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpRemoteError")
            .field("code", &self.code)
            .field("message", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for McpRemoteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.code;
        write!(formatter, "MCP server returned JSON-RPC error code {code}")?;
        if self.message.is_empty() {
            Ok(())
        } else {
            formatter.write_str(" (remote message withheld)")
        }
    }
}

impl std::error::Error for McpRemoteError {}

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
