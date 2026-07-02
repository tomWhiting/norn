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

/// Which deterministic provider stop cut a response off before the model
/// finished its turn.
///
/// Only the two abnormal [`StopReason`](crate::provider::events::StopReason)
/// variants are representable here, so a truncated outcome can never carry
/// a normal stop. Serializable so embedders can persist the stop kind
/// across process/activity boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationKind {
    /// The model hit its maximum output-token limit mid-response.
    MaxTokens,
    /// The provider's content filter cut the response off.
    ContentFilter,
}

impl TruncationKind {
    /// Stable string form used in session events and error messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::ContentFilter => "content_filter",
        }
    }
}

/// How the loop carries conversation state between provider calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

    /// Returns an owned, reference-counted handle to this executor, when
    /// one is available.
    ///
    /// Concurrent batch steps spawn each batch member onto its own tokio
    /// task so `ReadOnly`/`Network` calls genuinely run in parallel
    /// across runtime workers — which requires a `'static` executor
    /// handle to move into each task. Executors driven through an
    /// [`Arc`] return `Some` via the blanket `ToolExecutor` impl for
    /// `Arc<dyn ToolExecutor>`. The default `None` makes concurrent
    /// steps fall back to single-task `join_all` concurrency: members
    /// still overlap at await points but share one runtime worker.
    fn owned_handle(&self) -> Option<Arc<dyn ToolExecutor>> {
        None
    }
}

/// Delegating executor impl for owned handles: dispatch forwards to the
/// inner executor, and [`ToolExecutor::owned_handle`] hands out clones of
/// the `Arc` so concurrent batch steps can spawn each member on its own
/// task. Callers that hold their executor in an `Arc` pass
/// `&Arc<dyn ToolExecutor>` (instead of `&*arc`) to opt in.
#[async_trait::async_trait]
impl ToolExecutor for Arc<dyn ToolExecutor> {
    async fn execute(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        self.as_ref().execute(name, call_id, arguments).await
    }

    async fn execute_with_outcome(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<DispatchOutcome, ToolError> {
        self.as_ref()
            .execute_with_outcome(name, call_id, arguments)
            .await
    }

    fn shared_context(&self) -> Option<Arc<ToolContext>> {
        self.as_ref().shared_context()
    }

    fn owned_handle(&self) -> Option<Arc<dyn ToolExecutor>> {
        Some(Arc::clone(self))
    }
}

/// Coerce an `Arc`-held [`ToolRegistry`](crate::tool::registry::ToolRegistry)
/// into the `Arc<dyn ToolExecutor>` the interactive / print drivers hand to
/// [`run_agent_step`](crate::agent_loop::runner::run_agent_step).
///
/// The drivers pass the *returned* value by reference (`&Arc<dyn
/// ToolExecutor>`) so the blanket [`ToolExecutor`] impl for
/// `Arc<dyn ToolExecutor>` supplies [`ToolExecutor::owned_handle`], handing
/// each concurrent batch member its own spawnable `'static` handle — exactly
/// as `Agent::run` does. Passing the borrowed `&*registry` instead reaches
/// [`ToolRegistry`](crate::tool::registry::ToolRegistry)'s own impl, whose
/// default `owned_handle` is `None`, collapsing every concurrent tool batch
/// to the single-task `join_all` fallback.
///
/// This is the single coercion point every driver routes through, so that
/// owned-handle distinction cannot silently regress to the borrowed form in
/// one driver while the others stay correct.
#[must_use]
pub fn driver_executor(
    registry: &Arc<crate::tool::registry::ToolRegistry>,
) -> Arc<dyn ToolExecutor> {
    Arc::clone(registry) as Arc<dyn ToolExecutor>
}

/// Configuration for a single agent loop step.
///
/// Iteration-monitor configuration moved off this struct in N-017 and now
/// lives on [`LoopContext::iteration_monitor`](crate::agent_loop::loop_context::LoopContext::iteration_monitor), so it can be assembled by
/// the same caller that wires the rules engine and hook registry.
///
/// Serializable end to end (durations serialize as `{secs, nanos}`), so
/// embedders that cross process or activity boundaries — durable-workflow
/// activities, queued steps — can carry the *entire* loop configuration,
/// including the structured-output schema, inside their own serialized
/// inputs and reconstruct it with `serde` on the other side. Every field
/// has a default, so partial JSON deserializes with the remaining fields
/// at their [`Default`] values.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
    ///
    /// **Semantics differ from cancellation.** Cooperative cancellation
    /// (the step's `CancellationToken`) lets an in-flight tool batch
    /// finish in full before the loop returns `Cancelled`. Elapsing this
    /// budget instead **drops the step future wherever it is suspended**
    /// — including mid-tool-batch — so in-flight tools are aborted, not
    /// completed. The event store is repaired afterwards (tool calls left
    /// without results receive synthesized aborted-result records via
    /// `ensure_tool_results_complete`), but external side effects a
    /// dropped tool had already begun are not undone. Choose this field
    /// for a hard wall-clock guarantee; trigger the cancellation token
    /// for a graceful stop.
    pub step_timeout: Option<Duration>,

    /// Optional client-side context-window budget used by the token
    /// estimator. When set together with
    /// [`LoopContext::token_estimator`](crate::agent_loop::loop_context::LoopContext)
    /// the loop emits a `loop.token_warning` custom session event whenever
    /// the estimated prompt tokens exceed this limit. Advisory only — the
    /// provider call still runs.
    pub context_window_limit: Option<u64>,

    /// Reserve-token headroom below `context_window_limit` at which to fire
    /// auto-compaction. The trigger fires when the estimated prompt tokens
    /// exceed `context_window_limit − auto_compact_reserve_tokens` (e.g. a
    /// `272_000` window with a `30_000` reserve fires at `242_000`).
    /// Requires `context_window_limit` and a configured token estimator and
    /// context edits tracker on the loop context to take effect. `None`
    /// disables the trigger. Compaction fires at most once per
    /// `run_agent_step` call. Defaults to `Some(30_000)`.
    pub auto_compact_reserve_tokens: Option<u64>,

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
    /// This is distinct from [`Self::auto_compact_reserve_tokens`], which is
    /// local and expressed as reserve headroom below
    /// [`Self::context_window_limit`].
    pub server_compaction_threshold_tokens: Option<u64>,

    /// JSON schema enforced on the final response (structured output).
    ///
    /// `Some` puts the loop in schema mode: the model must answer through
    /// the schema tool named by [`Self::schema_tool_name`], validation
    /// failures consume [`Self::schema_attempt_budget`], and a completed
    /// run carries the validated value. `None` is plain-text mode.
    ///
    /// Living on the loop config (rather than as a separate builder-only
    /// field) gives the schema a serialized form: it rides along when an
    /// embedder persists or transmits the config, and it is introspectable
    /// post-build via
    /// [`ResolvedAgentInfo::output_schema`](crate::agent::ResolvedAgentInfo).
    pub output_schema: Option<Value>,

    /// Wall-clock budget for each profile-supplied
    /// [`PromptCommand`](crate::profile::PromptCommand) evaluated at the
    /// start of every iteration.
    ///
    /// `None` defers to the documented default,
    /// [`DEFAULT_PROMPT_COMMAND_TIMEOUT`](crate::agent_loop::loop_context::DEFAULT_PROMPT_COMMAND_TIMEOUT)
    /// (5 seconds, mirroring the shell-variable timeout in
    /// `integration::variables`). Commands in one iteration run
    /// concurrently, so the slowest command — bounded by this budget —
    /// is the iteration's total prompt-command wall-clock cost.
    /// Serde-compatible by absence: configs persisted before this field
    /// existed deserialize with `prompt_command_timeout: None`.
    pub prompt_command_timeout: Option<Duration>,

    /// Opt-in linger-await at the loop's would-stop boundaries (Wave 3,
    /// DECISION M3).
    ///
    /// `None` (the default) preserves the historical behavior exactly:
    /// the step returns the moment the model would stop, and a child
    /// result arriving afterwards is sent into a dropped channel. `Some`
    /// makes each would-stop boundary await late inbound messages and
    /// child-agent results — and the cancellation token — for up to
    /// [`LingerPolicy::deadline`](crate::agent_loop::linger::LingerPolicy::deadline)
    /// before stopping; whatever arrives is
    /// injected through the same flush/drain path as a mid-run delivery
    /// and the loop continues.
    ///
    /// There is no default duration. The linger await runs inside the
    /// step, so it counts toward a configured [`Self::step_timeout`].
    /// Serde-compatible by absence: configs persisted before this field
    /// existed deserialize with `linger: None`.
    pub linger: Option<crate::r#loop::linger::LingerPolicy>,
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
            auto_compact_reserve_tokens: Some(30_000),
            auto_compact_keep_recent_turns: 10,
            schema_tool_name: "structured_output".to_string(),
            cache_key: None,
            conversation_state: ConversationStateMode::Auto,
            server_compaction_threshold_tokens: None,
            output_schema: None,
            prompt_command_timeout: None,
            linger: None,
        }
    }
}

/// Outcome of a single agent loop step.
///
/// Every arm carries the same usage pair (W3.6 usage rollup): `usage` is
/// the step's **own** provider spend, and `children_usage` is the sum of
/// [`subtree_usage`](crate::agent::result_channel::ChildAgentResult::subtree_usage)
/// over every child result delivered into the step. The two are disjoint
/// by construction — own spend never includes children, child subtrees
/// never include this step — so the spawn/fork completion wrappers (and
/// embedders) compute the step's subtree total as `usage +
/// children_usage` without reaching into the loop.
#[derive(Debug)]
pub enum AgentStepResult {
    /// The model produced valid structured output (or text in no-schema mode).
    Completed {
        /// The validated output value.
        output: Value,
        /// Accumulated token usage across all provider calls in this step.
        usage: Usage,
        /// Summed `subtree_usage` of every child result delivered into
        /// this step. Disjoint from `usage` (own calls only).
        children_usage: Usage,
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
        /// Summed `subtree_usage` of every child result delivered into
        /// this step. Disjoint from `usage` (own calls only).
        children_usage: Usage,
    },

    /// The optional max-iterations cap was reached.
    MaxIterationsReached {
        /// Accumulated token usage across all provider calls.
        usage: Usage,
        /// Summed `subtree_usage` of every child result delivered into
        /// this step. Disjoint from `usage` (own calls only).
        children_usage: Usage,
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
        /// Accumulated token usage across all provider calls that
        /// completed before the timeout fired.
        usage: Usage,
        /// Summed `subtree_usage` of every child result delivered into
        /// this step before the timeout fired. Disjoint from `usage`
        /// (own calls only).
        children_usage: Usage,
    },

    /// The model stopped deterministically before completing its output —
    /// it hit its maximum output-token limit or the provider's content
    /// filter cut the response off — with no tool calls and no output
    /// schema in play. The response is an incomplete fragment, never a
    /// completion: the partial text and accumulated usage ride on this
    /// variant, and the full fragment plus stop reason are persisted on
    /// the session's `AssistantMessage` and `loop.truncated` events.
    ///
    /// Truncation is a *stopped run*, not a transport error — it is never
    /// retried (re-sending the identical request reproduces the same
    /// deterministic stop) and never surfaces as a
    /// [`ProviderError`](crate::error::ProviderError).
    Truncated {
        /// Which deterministic stop cut the response off.
        kind: TruncationKind,
        /// Partial assistant text produced before the cut, if any.
        partial_text: Option<String>,
        /// Completed provider iterations, including the truncated one.
        iterations: u32,
        /// Accumulated token usage across all provider calls, including
        /// the truncated call.
        usage: Usage,
        /// Summed `subtree_usage` of every child result delivered into
        /// this step. Disjoint from `usage` (own calls only).
        children_usage: Usage,
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
        /// Summed `subtree_usage` of every child result delivered into
        /// this step before cancellation. Disjoint from `usage` (own
        /// calls only).
        children_usage: Usage,
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::r#loop::linger::LingerPolicy;

    /// Serde-compatibility pin: configs persisted before the `linger`
    /// field existed (Phase 2 serialized configs) deserialize with
    /// `linger: None` — absent = None = current behavior.
    #[test]
    fn config_without_linger_field_deserializes_to_none() {
        let empty: AgentLoopConfig = serde_json::from_str("{}").unwrap();
        assert!(empty.linger.is_none());

        let legacy = serde_json::json!({
            "schema_attempt_budget": 3,
            "max_iterations": null,
            "step_timeout": null,
            "context_window_limit": null,
            "auto_compact_reserve_tokens": 30000,
            "auto_compact_keep_recent_turns": 10,
            "schema_tool_name": "structured_output",
            "cache_key": null,
            "conversation_state": "auto",
            "server_compaction_threshold_tokens": null,
            "output_schema": null
        });
        let cfg: AgentLoopConfig = serde_json::from_value(legacy).unwrap();
        assert!(cfg.linger.is_none());
        assert_eq!(cfg.schema_attempt_budget, 3);
    }

    /// Shape pin for the serialized policy: durations serialize as
    /// `{secs, nanos}`, matching every other duration on this config.
    #[test]
    fn linger_policy_serde_shape_is_pinned() {
        let policy = LingerPolicy {
            deadline: Duration::from_millis(1500),
        };
        let value = serde_json::to_value(policy).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "deadline": { "secs": 1, "nanos": 500_000_000 } }),
        );
        let back: LingerPolicy = serde_json::from_value(value).unwrap();
        assert_eq!(back, policy);
    }

    /// Serde-compatibility pin: configs persisted before the
    /// `prompt_command_timeout` field existed deserialize with `None` —
    /// absent = None = the documented 5-second default at evaluation time.
    #[test]
    fn config_without_prompt_command_timeout_deserializes_to_none() {
        let empty: AgentLoopConfig = serde_json::from_str("{}").unwrap();
        assert!(empty.prompt_command_timeout.is_none());

        let cfg = AgentLoopConfig {
            prompt_command_timeout: Some(Duration::from_millis(750)),
            ..AgentLoopConfig::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AgentLoopConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.prompt_command_timeout,
            Some(Duration::from_millis(750))
        );
    }

    /// Owned-handle contract: borrows expose no handle (concurrent batch
    /// steps fall back to single-task concurrency), while an
    /// `Arc<dyn ToolExecutor>` hands out clones of itself and delegates
    /// dispatch to the inner executor.
    #[tokio::test]
    async fn arc_executor_exposes_owned_handle_and_delegates() {
        let mut handlers: std::collections::HashMap<String, ToolHandler> =
            std::collections::HashMap::new();
        handlers.insert(
            "echo".to_owned(),
            Box::new(|args| Ok(serde_json::json!({ "echoed": args }))),
        );
        let mock = MockToolExecutor::new(handlers);
        assert!(
            mock.owned_handle().is_none(),
            "a plain executor exposes no owned handle",
        );

        let shared: Arc<dyn ToolExecutor> = Arc::new(mock);
        let handle = shared
            .owned_handle()
            .expect("an Arc'd executor hands out an owned handle");
        let result = handle
            .execute("echo", "call-1", serde_json::json!({ "x": 1 }))
            .await
            .expect("delegated dispatch succeeds");
        assert_eq!(result["echoed"]["x"], 1);
        assert!(
            handle.owned_handle().is_some(),
            "the handle itself stays spawnable",
        );
    }

    #[test]
    fn config_with_linger_round_trips() {
        let cfg = AgentLoopConfig {
            linger: Some(LingerPolicy {
                deadline: Duration::from_secs(30),
            }),
            ..AgentLoopConfig::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AgentLoopConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.linger,
            Some(LingerPolicy {
                deadline: Duration::from_secs(30),
            }),
        );
    }
}
