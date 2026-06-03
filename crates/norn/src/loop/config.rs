//! Configuration and result types for the agent loop runner.
//!
//! Houses [`AgentLoopConfig`], [`AgentStepResult`], and [`ToolExecutor`]
//! extracted from `runner.rs` to keep that module within the 500-line
//! production-code limit (CO5).

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::error::ToolError;
use crate::provider::usage::Usage;
use crate::tool::context::ToolContext;
use crate::tool::follow_up::FollowUpAction;

/// How the loop carries conversation state between provider calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConversationStateMode {
    /// Use provider-managed threading when the selected provider supports it,
    /// otherwise replay local prompt history.
    #[default]
    Auto,
    /// Replay the local prompt view on every provider call.
    ManualReplay,
    /// Use provider-managed response threading when the provider supports it.
    ProviderThreaded,
}

/// Executes tools by name with JSON arguments.
///
/// Implementations route tool calls to their concrete handlers. The trait
/// is object-safe and uses `async_trait` for async execution behind `dyn`.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute the named tool with the given arguments.
    ///
    /// Returns the structured output on success, or a [`ToolError`] describing
    /// what went wrong (pre-validation, execution, post-validation, or not found).
    async fn execute(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<Value, ToolError>;

    /// Execute the named tool and return lifecycle metadata captured during
    /// dispatch. Implementations that do not expose metadata fall back to
    /// [`Self::execute`] with no follow-ups or post-validate outcome.
    async fn execute_with_outcome(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<DispatchOutcome, ToolError> {
        let content = self.execute(name, call_id, arguments).await?;
        Ok(DispatchOutcome {
            content,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
        })
    }

    /// Returns the shared [`ToolContext`] the executor passes to every
    /// dispatched tool call, if one exists.
    ///
    /// The default implementation returns `None`. Concrete executors with
    /// an internal `ToolContext` (e.g. [`ToolRegistry`](crate::tool::registry::ToolRegistry))
    /// override this to expose their context so callers can publish
    /// extensions (e.g. a diagnostic collector) before dispatch.
    fn shared_context(&self) -> Option<Arc<ToolContext>> {
        None
    }
}

/// Configuration for a single agent loop step.
///
/// Iteration-monitor configuration moved off this struct in N-017 and now
/// lives on [`LoopContext::iteration_monitor`], so it can be assembled by
/// the same caller that wires the rules engine and hook registry.
#[derive(Clone, Debug)]
pub struct AgentLoopConfig {
    /// Combined budget for schema validation retries and nudges.
    ///
    /// Each failed validation or text-stop-without-schema-call consumes one
    /// unit. When exhausted, the loop returns [`AgentStepResult::SchemaUnreachable`].
    pub schema_attempt_budget: u32,

    /// Optional hard cap on total provider round-trips within a single step.
    pub max_iterations: Option<u32>,

    /// Optional outer wall-clock cap on the entire `run_agent_step` call.
    /// When set, the loop body is wrapped in [`tokio::time::timeout`] and
    /// elapsing the duration produces [`AgentStepResult::TimedOut`] with
    /// whatever partial output the model produced.
    pub step_timeout: Option<Duration>,

    /// Optional client-side context-window budget used by the token
    /// estimator. When set together with
    /// [`LoopContext::token_estimator`](crate::r#loop::loop_context::LoopContext)
    /// the loop emits a `loop.token_warning` custom session event whenever
    /// the estimated prompt tokens exceed this limit. Advisory only — the
    /// provider call still runs.
    pub context_window_limit: Option<u64>,

    /// Optional fraction of `context_window_limit` at which to fire
    /// auto-compaction (e.g. `0.75` for 75%). Requires both
    /// `context_window_limit` and a configured token estimator and context
    /// edits tracker on the loop context to take effect. Compaction fires
    /// at most once per `run_agent_step` call.
    pub auto_compact_threshold_pct: Option<f64>,

    /// Number of recent assistant turns to retain when auto-compaction
    /// fires. Older events are summarised into a single
    /// [`SessionEvent::Compaction`](crate::session::events::SessionEvent::Compaction).
    pub auto_compact_keep_recent_turns: usize,

    /// Name of the schema enforcement tool presented to the model.
    pub schema_tool_name: String,

    /// Prompt cache key sent to the provider. When set, enables
    /// deterministic prompt caching across calls within the same session.
    pub cache_key: Option<String>,

    /// Conversation state policy used when constructing provider requests.
    pub conversation_state: ConversationStateMode,

    /// Absolute provider-side compaction threshold in rendered tokens.
    ///
    /// This is distinct from [`Self::auto_compact_threshold_pct`], which is
    /// local and expressed as a fraction of [`Self::context_window_limit`].
    pub server_compaction_threshold_tokens: Option<u64>,
}

/// Full result of dispatching one tool through its lifecycle.
#[derive(Clone, Debug)]
pub struct DispatchOutcome {
    /// Model-facing structured output.
    pub content: Value,
    /// Full follow-up actions registered by the tool.
    pub follow_ups: Vec<FollowUpAction>,
    /// Serialized post-validation outcome, when validation ran.
    pub post_validate_outcome: Option<Value>,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            schema_attempt_budget: 3,
            max_iterations: None,
            step_timeout: None,
            context_window_limit: None,
            auto_compact_threshold_pct: None,
            auto_compact_keep_recent_turns: 10,
            schema_tool_name: "structured_output".to_string(),
            cache_key: None,
            conversation_state: ConversationStateMode::Auto,
            server_compaction_threshold_tokens: None,
        }
    }
}

/// Outcome of a single agent loop step.
#[derive(Debug)]
pub enum AgentStepResult {
    /// The model produced valid structured output (or text in no-schema mode).
    Completed {
        /// The validated output value.
        output: Value,
        /// Accumulated token usage across all provider calls in this step.
        usage: Usage,
    },

    /// The schema enforcement budget was exhausted without valid output.
    SchemaUnreachable {
        /// Best output attempt produced, if any.
        best_attempt: Option<Value>,
        /// Validation errors from the final attempt.
        validation_errors: Vec<String>,
        /// Total schema-budget-consuming attempts made.
        attempts: u32,
        /// Accumulated token usage across all provider calls.
        usage: Usage,
    },

    /// The optional max-iterations cap was reached.
    MaxIterationsReached {
        /// Accumulated token usage across all provider calls.
        usage: Usage,
    },

    /// The configured `step_timeout` elapsed before the loop completed.
    TimedOut {
        /// Wall-clock time the loop ran before being cancelled.
        elapsed: Duration,
        /// Number of completed provider iterations at the moment of the
        /// timeout.
        iterations: usize,
        /// Last assistant text observed before the timeout, if any.
        partial_output: Option<Value>,
    },

    /// Cooperative cancellation fired via the
    /// [`CancellationToken`](tokio_util::sync::CancellationToken) passed
    /// to [`run_agent_step`](super::runner::run_agent_step). The loop
    /// returns at the next iteration boundary or when the in-flight
    /// provider call yields to the cancellation race, whichever comes
    /// first; any tool already executing finishes in full before this
    /// variant is returned. Distinct from
    /// [`MaxIterationsReached`](Self::MaxIterationsReached) (loop ran
    /// its course) and [`TimedOut`](Self::TimedOut) (wall-clock budget
    /// elapsed) — this is operator-initiated abort.
    Cancelled {
        /// Accumulated token usage across all provider calls that
        /// completed before cancellation. Empty when the token was
        /// already cancelled before the first iteration ran.
        usage: Usage,
    },
}

// ---------------------------------------------------------------------------
// Mock tool executor (test utilities)
// ---------------------------------------------------------------------------

/// Type alias for tool handler closures.
#[cfg(any(test, feature = "test-utils"))]
pub type ToolHandler = Box<dyn Fn(Value) -> Result<Value, ToolError> + Send + Sync>;

/// Mock tool executor for deterministic testing.
///
/// Wraps a map of tool name to handler function. Unknown tools return
/// [`ToolError::ToolNotFound`].
#[cfg(any(test, feature = "test-utils"))]
pub struct MockToolExecutor {
    handlers: std::collections::HashMap<String, ToolHandler>,
}

#[cfg(any(test, feature = "test-utils"))]
impl MockToolExecutor {
    /// Create a new mock executor with the given handlers.
    pub fn new(handlers: std::collections::HashMap<String, ToolHandler>) -> Self {
        Self { handlers }
    }

    /// Create an empty mock executor (all tools return `ToolNotFound`).
    pub fn empty() -> Self {
        Self {
            handlers: std::collections::HashMap::new(),
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait::async_trait]
impl ToolExecutor for MockToolExecutor {
    async fn execute(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        let _ = call_id;
        match self.handlers.get(name) {
            Some(handler) => handler(arguments),
            None => Err(ToolError::ToolNotFound {
                name: name.to_string(),
            }),
        }
    }
}
