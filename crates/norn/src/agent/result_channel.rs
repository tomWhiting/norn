//! Child-agent result channel for delivering fork and spawn outcomes
//! to the orchestrator's outer loop.

use std::sync::Arc;

use uuid::Uuid;

use crate::agent::output::AgentStopReason;
use crate::provider::usage::Usage;

/// Formatted result from a completed child agent (fork or spawn).
///
/// Sent through the bounded mpsc channel from the child's `tokio::spawn`
/// task to the orchestrator's outer loop, which injects the formatted
/// message as the next user turn.
#[derive(Clone, Debug)]
pub struct ChildAgentResult {
    /// Registry id of the completed child.
    pub agent_id: Uuid,
    /// Display role, e.g. "fork/gpt-5.4-mini" or "spawn/reviewer".
    pub agent_role: String,
    /// Whether the child completed successfully.
    pub succeeded: bool,
    /// Markdown-formatted result ready for injection as a user message.
    pub formatted_message: String,
    /// Error message when `succeeded` is false.
    pub error: Option<String>,
    /// Typed stop reason when the child's run stopped early without
    /// completing (schema budget, max iterations, timeout, cancellation,
    /// truncation). `None` when the child completed (`succeeded: true`)
    /// or failed with a hard [`NornError`](crate::error::NornError)
    /// (in which case `error` carries the description).
    pub stop: Option<AgentStopReason>,
    /// Accumulated token usage across every provider call the child
    /// made, populated on success and every early-stop outcome alike (a
    /// stopped run still consumed tokens). [`Usage::default`] (all zeros)
    /// when the run ended in a hard
    /// [`NornError`](crate::error::NornError) or the child's wrapper task
    /// panicked — the runner's `Err` path carries no usage, so zeros mean
    /// "unknown", not "no tokens consumed".
    pub usage: Usage,
}

/// Sender half of the child-agent result channel.
///
/// Wrapped in `Arc` so it can be cloned into each child's `tokio::spawn`
/// task. Installed as a `ToolContext` extension during `build_runtime`.
#[derive(Clone, Debug)]
pub struct ChildResultSender(pub Arc<tokio::sync::mpsc::Sender<ChildAgentResult>>);

/// Channel buffer capacity. Generous enough that fork completion never
/// blocks under normal operation; a full channel signals runaway spawning.
pub const CHILD_RESULT_CHANNEL_CAPACITY: usize = 256;
