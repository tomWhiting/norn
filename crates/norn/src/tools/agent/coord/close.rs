//! `CloseAgentTool` — recursively shut down a target agent and its whole
//! subtree, leaves first.

use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::helpers::sender_label;
use crate::agent::registry::AgentRegistry;
use crate::error::ToolError;
use crate::r#loop::inbound::{ChannelMessage, DeliveryMode};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::agent::handle::AgentHandles;
use crate::tools::agent::infra::{infra_from, resolve_agent_id};

/// Recursively shut down a target agent and every descendant.
///
/// DFS post-order — leaves transition first, then their parents, finally
/// the target. For direct children whose
/// [`crate::tools::agent::handle::AgentHandle`] the parent holds, the
/// shutdown sends a best-effort Steer message via the child's
/// [`crate::r#loop::inbound::InboundChannel`] then aborts the child's
/// task. Indirect descendants (whose handles live on intermediate agents'
/// contexts) are marked in the registry; the aborted parent will stop
/// dispatching new work to them.
pub struct CloseAgentTool;

impl CloseAgentTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for CloseAgentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct CloseAgentArgs {
    agent_id: String,
    #[serde(default)]
    reason: Option<String>,
}

/// Collect the subtree rooted at `root` in DFS pre-order (root first).
///
/// Reverse the result to get leaves-first shutdown ordering. Iterative
/// traversal — no recursion depth bound (CO1: no hardcoded limits).
fn collect_subtree(registry: &AgentRegistry, root: Uuid) -> Vec<Uuid> {
    let mut order: Vec<Uuid> = Vec::new();
    let mut stack: Vec<Uuid> = vec![root];
    while let Some(id) = stack.pop() {
        order.push(id);
        for child in registry.children(id) {
            stack.push(child.id);
        }
    }
    order
}

/// Per-agent shutdown — best-effort steer, abort if a handle is held, and
/// terminal registry transitions. Failures during these steps are surfaced
/// in the returned status label rather than panicked on.
async fn shutdown_one(
    registry: &parking_lot::RwLock<AgentRegistry>,
    handles: Option<&AgentHandles>,
    id: Uuid,
    reason: Option<&str>,
    sender_path: &str,
) -> &'static str {
    if let Some(h) = handles
        && let Some(handle) = h.remove(id)
    {
        let body = reason.unwrap_or("close_agent").to_string();
        let msg = ChannelMessage {
            author: sender_path.to_string(),
            content: body,
            delivery: DeliveryMode::Steer,
            timestamp: Utc::now(),
        };
        // Send is best-effort: the child may already have terminated and
        // dropped its receiver. We still abort the join handle below.
        let _ = handle.inbound_tx.send(msg).await;
        handle.join_handle.abort();
    }

    let mut reg = registry.write();
    if reg.mark_completing(id).is_err() {
        // Agent already gone from the registry — nothing more to do.
        return "missing";
    }
    if reg.mark_completed(id).is_err() {
        return "failed";
    }
    "completed"
}

#[async_trait]
impl Tool for CloseAgentTool {
    fn name(&self) -> &'static str {
        "close_agent"
    }

    fn description(&self) -> &'static str {
        include_str!("../../guidance/close_agent.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../../guidance/close_agent.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["agent_id"],
            "additionalProperties": false,
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Target agent identified by hierarchical registry path or UUID. The whole subtree rooted at this agent is shut down."
                },
                "reason": {
                    "type": "string",
                    "description": "Human-readable explanation for why the agent is being closed. Recorded in the registry for observability."
                }
            }
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Process
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let started = Instant::now();
        let args: CloseAgentArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;
        let infra = infra_from(ctx)?;
        let target_id = resolve_agent_id(&infra.registry, &args.agent_id)?;
        let handles = ctx.get_extension::<AgentHandles>();
        let handles_ref = handles.as_deref();

        // Collect the subtree once under a single read lock, then drop it
        // before performing the per-agent shutdown (which takes write locks).
        let mut order = {
            let reg = infra.registry.read();
            collect_subtree(&reg, target_id)
        };
        order.reverse();

        let sender_path = sender_label(&infra.registry.read(), infra.agent_id);
        let mut shut_down = Vec::with_capacity(order.len());
        for id in order {
            let status = shutdown_one(
                &infra.registry,
                handles_ref,
                id,
                args.reason.as_deref(),
                &sender_path,
            )
            .await;
            shut_down.push(serde_json::json!({
                "agent_id": id.to_string(),
                "status": status,
            }));
        }

        let payload = serde_json::json!({
            "agent_id": target_id.to_string(),
            "reason": args.reason,
            "shut_down": shut_down,
        });
        Ok(ToolOutput {
            content: payload,
            is_error: false,
            duration: started.elapsed(),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args,
    clippy::let_underscore_future
)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::agent::registry::AgentStatus;
    use crate::tools::agent::coord::test_support::{
        build_infra, envelope_for, register_agent, synthetic_handle,
    };

    /// R3: closing the root of a subtree with two children shuts down all
    /// three agents in leaves-first (DFS post-order) ordering.
    ///
    /// Uses a depth-2 hierarchy (root + two direct children) because the
    /// registry enforces a spawn depth limit that prevents grandchildren.
    #[tokio::test]
    async fn close_agent_dfs_shuts_down_subtree() {
        let (infra, registry, _mailbox) = build_infra(Uuid::new_v4());
        let root = register_agent(&registry, "/root", None);
        let child_a = register_agent(&registry, "/root/child-a", Some(root));
        let child_b = register_agent(&registry, "/root/child-b", Some(root));

        // Parent holds AgentHandles for its direct children.
        let handles = Arc::new(AgentHandles::new());
        let (handle_a, _tx_a, _rx_a) = synthetic_handle(child_a);
        let (handle_b, _tx_b, _rx_b) = synthetic_handle(child_b);
        handles.insert(handle_a);
        handles.insert(handle_b);

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(handles);

        let tool = CloseAgentTool::new();
        let envelope = envelope_for(
            "close_agent",
            json!({"agent_id": "/root", "reason": "stop"}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("close");
        assert!(!out.is_error, "{:?}", out.content);
        let shut_down = out.content["shut_down"].as_array().expect("array");
        assert_eq!(shut_down.len(), 3);
        // Leaves-first ordering: both children appear before root.
        let root_idx = shut_down
            .iter()
            .position(|e| e["agent_id"] == root.to_string())
            .expect("root in output");
        assert_eq!(
            root_idx,
            shut_down.len() - 1,
            "root must be the last agent shut down",
        );

        let reg = registry.read();
        assert_eq!(reg.get(root).expect("root").status, AgentStatus::Completed);
        assert_eq!(
            reg.get(child_a).expect("child_a").status,
            AgentStatus::Completed
        );
        assert_eq!(
            reg.get(child_b).expect("child_b").status,
            AgentStatus::Completed
        );
    }

    /// R3 degenerate case: a leaf with no children still transitions
    /// cleanly through Completing → Completed.
    #[tokio::test]
    async fn close_agent_on_leaf_no_children_works() {
        let (infra, registry, _mailbox) = build_infra(Uuid::new_v4());
        let leaf = register_agent(&registry, "/leaf", None);

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = CloseAgentTool::new();
        let envelope = envelope_for(
            "close_agent",
            json!({"agent_id": "/leaf", "reason": "done"}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("close");
        assert!(!out.is_error);
        let shut_down = out.content["shut_down"].as_array().expect("array");
        assert_eq!(shut_down.len(), 1);
        assert_eq!(shut_down[0]["agent_id"], leaf.to_string());
        assert_eq!(shut_down[0]["status"], "completed");

        assert_eq!(
            registry.read().get(leaf).expect("entry").status,
            AgentStatus::Completed
        );
    }

    /// R3 + R4: `close_agent` sends a `Steer` shutdown message via the child's
    /// `InboundChannel` before aborting its task, so a cooperating loop
    /// has a final boundary to observe the request.
    #[tokio::test]
    async fn close_agent_sends_shutdown_steer_to_child() {
        let (infra, registry, _mailbox) = build_infra(Uuid::new_v4());
        let parent_id = infra.agent_id;
        let child = register_agent(&registry, "/parent/child", Some(parent_id));

        let handles = Arc::new(AgentHandles::new());
        let (child_handle, _tx, mut inbound_rx) = synthetic_handle(child);
        handles.insert(child_handle);

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(handles);

        let tool = CloseAgentTool::new();
        let envelope = envelope_for(
            "close_agent",
            json!({"agent_id": "/parent/child", "reason": "wrap up"}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("close");
        assert!(!out.is_error);

        let drained = inbound_rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].delivery, DeliveryMode::Steer);
        assert_eq!(drained[0].content, "wrap up");
    }
}
