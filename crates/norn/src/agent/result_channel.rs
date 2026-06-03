//! Child-agent result channel for delivering fork and spawn outcomes
//! to the orchestrator's outer loop.

use std::sync::Arc;

use uuid::Uuid;

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
