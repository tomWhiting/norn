//! `follow_up` — execute a deferred action registered by a prior tool call.
//!
//! Tools register [`FollowUpAction`]s
//! at the end of their lifecycle (e.g. `undo`, `apply_structural`). The model
//! executes one by calling `follow_up` with the original `tool_call_id` and
//! the action's `action` name. The tool looks the reference up in the session
//! action log, checks the action's expiry against current file/turn state,
//! merges the action's pre-populated argument overrides onto the original
//! call's arguments, and dispatches the target tool through the registry's
//! full lifecycle. This eliminates re-generating large arguments (patch text,
//! file content) by reading them from the action log.
//!
//! Submodules:
//! * [`lookup`] — action-log query and action selection.
//! * [`expiry`] — expiry evaluation against current file and turn state.
//! * [`merge`] — shallow argument-override merge.
//! * [`dispatch`] — orchestration and target-tool lifecycle dispatch.

pub mod dispatch;
pub mod expiry;
pub mod lookup;
pub mod merge;

#[cfg(test)]
mod tests;

pub use self::dispatch::{CurrentTurnId, SharedToolRegistry};

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::follow_up::FollowUpAction;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Executes deferred follow-up actions by referencing a prior `tool_call_id`
/// and action name.
pub struct FollowUpTool;

impl FollowUpTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for FollowUpTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for FollowUpTool {
    fn name(&self) -> &'static str {
        "follow_up"
    }

    fn description(&self) -> &'static str {
        "Execute a deferred follow-up action that an earlier tool call \
         registered (for example `undo`, `apply_structural`, or a retry with \
         adjusted options). Pass the original call's `tool_call_id` and the \
         action's `action` name. The tool reads the original arguments from \
         the action log, applies the action's pre-set overrides, and runs the \
         target tool — so you never re-supply large arguments such as patch \
         text or file contents. Use the `action_log` tool's `follow_ups` query \
         to discover which actions are still available. Expired actions (the \
         referenced file changed, or the turn ended) return a structured error."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::General
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["tool_call_id", "action"],
            "properties": {
                "tool_call_id": {
                    "type": "string",
                    "description": "Tool-call id of the original call whose follow-up you want to execute."
                },
                "action": {
                    "type": "string",
                    "description": "Name of the registered follow-up action to execute (e.g. \"undo\", \"apply_structural\")."
                }
            },
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        // The follow-up dispatches an arbitrary target tool whose effect is
        // unknown at this layer (it may write to disk or run a process), so it
        // must be serialised for safety rather than treated as read-only.
        ToolEffect::Unknown
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        dispatch::dispatch_follow_up(envelope, ctx).await
    }

    /// The follow-up tool never registers follow-ups of its own — there is no
    /// recursive follow-up. Any follow-ups belong to the dispatched target
    /// tool and are attributed to it.
    async fn register_follow_ups(
        &self,
        _output: &ToolOutput,
        _ctx: &ToolContext,
    ) -> Vec<FollowUpAction> {
        Vec::new()
    }
}
