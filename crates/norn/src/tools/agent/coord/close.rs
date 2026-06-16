//! `CloseAgentTool` — recursively shut down a target agent and its whole
//! subtree, leaves first.

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::helpers::sender_attribution;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::error::ToolError;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::agent::handle::AgentHandles;
use crate::tools::agent::infra::{ResolvedAgent, infra_from, resolve_agent};

/// Recursively shut down a target agent and every descendant.
///
/// **Token-first (W3.5):** when the closer holds the target's
/// [`crate::tools::agent::handle::AgentHandle`], the target's cooperative
/// [`CancellationToken`](tokio_util::sync::CancellationToken) is fired
/// *before* the subtree walk. Spawn/fork create every child's run token
/// as a child of its spawner's token, so this one cancel cascades to the
/// target's entire spawned subtree — descendants the closer holds no
/// handle for included. Each descendant's run ends with the real
/// `Cancelled` outcome at its next cancellation boundary and its **own**
/// completion wrapper performs its terminal sequence (registry mark,
/// lifecycle `Completed`, result delivery, reclamation) — close never
/// touches a descendant's entry; single ownership holds at every depth.
///
/// The walk itself is DFS post-order — leaves first, the target last
/// (depth-unbounded since W3.4 recursion). For the target — whose handle
/// the closer holds — the shutdown sends a best-effort Steer message via
/// the child's [`crate::r#loop::inbound::InboundChannel`] (best-effort
/// twice over: with the token already fired the loop may end before its
/// next inbound drain), cancels the token again (idempotent), and joins
/// the wrapper task before touching the registry, so the closer and the
/// completion wrapper can never race over an entry's terminal sequence
/// and the wrapper records the run's real outcome itself. The close
/// therefore returns only after the *target's* wrapper completes;
/// cascade-cancelled descendants are reported `"cancelling"` and finish
/// through their own wrappers (their results land on their own parents'
/// channels — or are error-logged when that parent's loop already
/// ended). When no cascade was triggered (the closer holds no handle for
/// the target), live no-handle agents are reported `"unreachable"`
/// untouched, exactly as before; already-terminal entries are reclaimed
/// without rewriting their recorded outcome.
///
/// Closing an agent that already completed and was reclaimed is a **soft
/// success**: the desired post-condition (the agent is not running)
/// already holds, so the tool reports `already_completed` with the
/// recorded terminal status and timestamp instead of an error — an error
/// would only push the model into pointless retries against an agent
/// that no longer exists.
pub struct CloseAgentTool;

/// Stable `snake_case` label for a status embedded in human-readable
/// messages (the JSON fields use serde's `snake_case` serialization
/// directly).
fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Spawning => "spawning",
        AgentStatus::Active => "active",
        AgentStatus::Completing => "completing",
        AgentStatus::Idle => "idle",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::Closed => "closed",
    }
}

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

/// Harness-resolved identity of the closing agent, stamped onto the
/// shutdown steer's attribution fields (registry ground truth, resolved
/// once per close call).
struct CloserAttribution {
    id: Uuid,
    label: String,
    role: Option<String>,
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

/// Per-agent shutdown. Failures during these steps are surfaced in the
/// returned status label rather than panicked on.
///
/// **With a held handle** — best-effort Steer, then trigger the handle's
/// cooperative cancellation token and **join the wrapper before touching
/// the registry**. The token is the same one the launching tool threaded
/// into the child's [`run_agent_step`](crate::r#loop::runner::run_agent_step)
/// request, so cancelling it terminates the *run itself*: the loop
/// observes the token at the top of every iteration and races it
/// (cancel-priority, `biased` select) against the in-flight provider
/// call, so even a hung provider request yields immediately. The run
/// returns `AgentStepResult::Cancelled` and the wrapper — the sole owner
/// of the terminal sequence — records the run's **real** outcome:
/// registry `Failed`, typed `AgentStopReason::Cancelled` on the lifecycle
/// event and result channel, status broadcast, reclamation if anchored.
/// After the join the entry is therefore normally terminal and the
/// closer's job is reclaim-only (`"reclaimed"`).
///
/// The wrapper is **never aborted**: aborting it would drop the wrapper
/// future at `inner.await`, detaching the inner run task to keep
/// executing (provider calls, tool executions, file mutations) with no
/// observer — the exact defect cancellation exists to prevent. The join
/// is deliberately unbounded-but-cancellation-backed; no timeout is
/// applied because none is configured anywhere (inventing one is
/// forbidden) and every await on the wrapper's post-cancellation path is
/// one the runtime already depends on terminating: the provider race
/// yields to the token instantly, a tool already executing finishes in
/// full by the loop's documented cancellation contract, hooks are
/// operator code the loop awaits inline on every run anyway, and the
/// result-channel send is bounded-buffer backpressure into the closing
/// parent's own channel — under the concurrent-children cap and the
/// loop's per-iteration drains a full buffer is unreachable short of
/// runaway spawning, and even then the close stalls observably rather
/// than detaching anything (a `try_send` that dropped the result on a
/// full buffer would be a silent failure, so blocking is correct).
///
/// **Without a handle, cascade triggered** (`"cancelling"`) — the
/// caller already fired the target's token before this walk, and W3.5
/// guarantees every spawn/fork descendant's token is a child of its
/// spawner's, so this agent's run is already ending with the real
/// `Cancelled` outcome. Its own completion wrapper owns the terminal
/// sequence (mark, lifecycle, delivery, reclamation) — the closer must
/// not race it for the entry, so it reports the truth and moves on.
/// (`cascade_cancelled` is only ever set for registry *descendants* of
/// a handle-held target; every such descendant was launched by
/// `spawn_agent`/`fork`, whose token lineage is unbroken by
/// construction. The one launch path with no token at all — rhai
/// script children, `cancel: None` — registers children only under
/// script hosts, which hold no `AgentHandles` and therefore can never
/// trigger a cascade in the first place.)
///
/// **Without a handle, no cascade** — the closer cannot stop the
/// target's task, so it must not touch a live entry either: marking a
/// still-running agent terminal would falsify the record, and removing
/// the entry would steal the terminal transition its completion wrapper
/// still owes. Live no-handle targets are reported `"unreachable"`.
///
/// An entry that is *already* terminal is treated as reclaim-only: its
/// recorded outcome is never rewritten (terminal statuses are immutable;
/// a Failed child must not be resurrected as Completed). The status
/// check and the transitions run under one write lock so a concurrent
/// reclaimer cannot slip between them. Reclamation leaves a tombstone,
/// so an entry that vanished between resolution and shutdown reports
/// `"already_completed"` from its completion record.
///
/// **Entry still live after the join** (`"force_failed"`) — the wrapper
/// ended without performing its terminal mark: it panicked / was killed
/// externally (join error), or it exited with the transition suppressed
/// by a stop-hook Block. The closer now owns this lifecycle end, but it
/// cannot know the outcome the wrapper never recorded — so it records
/// the most truthful status available: **`Failed`**, never `Completed`
/// (a forced shutdown with an unknown outcome must not masquerade as a
/// success). A dedicated "forced shutdown, outcome unknown" variant on
/// [`AgentStatus`] was considered and rejected for this fix: the enum is
/// matched exhaustively by external observers (norn-tui's status panel
/// icons/colours/hold-window logic) and serialized into tombstones and
/// tool outputs, so widening it ripples far beyond the close path.
/// `Failed` is the conservative truth — the run did not verifiably
/// complete — and the close `reason` is carried on the tool output and
/// the log line below.
async fn shutdown_one(
    registry: &parking_lot::RwLock<AgentRegistry>,
    handles: Option<&AgentHandles>,
    id: Uuid,
    reason: Option<&str>,
    closer: &CloserAttribution,
    cascade_cancelled: bool,
) -> &'static str {
    let held = handles.and_then(|h| h.remove(id));
    let had_handle = held.is_some();
    if let Some(handle) = held {
        let body = reason.unwrap_or("close_agent").to_string();
        // Direct handle send: the shutdown steer deliberately bypasses
        // the MessageRouter (the handle is the closer's own capability
        // and the recipient is being torn down), so it carries no
        // router sequence. It still renders through the one framed
        // injection path if the recipient drains it.
        let msg = ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: closer.id,
            from: closer.label.clone(),
            role: closer.role.clone(),
            to_id: id,
            content: body,
            kind: MessageKind::Steer,
            seq: None,
            timestamp: Utc::now(),
        };
        // Send is best-effort: the child may already have terminated and
        // dropped its receiver. The cancellation below terminates the run
        // either way, so the failure is logged, not surfaced.
        if let Err(e) = handle.inbound_tx.send(msg).await {
            tracing::debug!(
                agent_id = %id,
                error = %e,
                "shutdown steer not delivered; child already dropped its receiver",
            );
        }
        // Terminate the run cooperatively, then join the wrapper so it
        // completes its own terminal sequence with the run's real
        // outcome. See the function docs for why this join is unbounded
        // and why the wrapper is never aborted.
        handle.cancel.cancel();
        if let Err(join_error) = handle.join_handle.await {
            // The wrapper never aborts itself and workspace code denies
            // panics, so a join error means a dependency panicked inside
            // the wrapper or something external killed the task — either
            // way the entry below is still live and the closer must own
            // the forced-failure mark.
            tracing::error!(
                agent_id = %id,
                error = %join_error,
                "close_agent: child wrapper task died without completing its \
                 terminal sequence",
            );
        }
    }

    let mut reg = registry.write();
    let Some(entry) = reg.get(id) else {
        // Gone from the registry: reclaimed by its own completion
        // wrapper (or another closer) between resolution and here. The
        // tombstone proves it finished; its absence would mean an entry
        // vanished without a terminal record — an invariant violation.
        if reg.tombstone(id).is_some() {
            return "already_completed";
        }
        tracing::error!(
            agent_id = %id,
            "close_agent: invariant violation: target vanished from the registry \
             without a completion record",
        );
        return "missing";
    };
    if entry.status.is_terminal() {
        // Terminal — either it finished on its own earlier, or the
        // cancellation above let the wrapper record the run's real
        // outcome. Reclaim without rewriting it.
        reg.remove_terminal(id);
        return "reclaimed";
    }
    if !had_handle {
        if cascade_cancelled {
            // Live, no handle — but the target's token-first cancel
            // already reached this descendant through its token lineage
            // (every spawn/fork child token is a child of its spawner's
            // token, W3.5). Its run is ending with the real `Cancelled`
            // outcome and its own wrapper owns the terminal sequence;
            // the closer reports the truth and leaves the entry to it.
            return "cancelling";
        }
        // Live, and the closer cannot stop its task: leave the entry to
        // its lifecycle owner and say so.
        return "unreachable";
    }
    // The wrapper was joined above and ended without its terminal mark
    // (see the function docs). The closer owns this lifecycle end but
    // does not know the run's outcome, so it records the honest
    // conservative status — Failed — and reclaims immediately.
    tracing::warn!(
        agent_id = %id,
        reason = reason.unwrap_or("close_agent"),
        "close_agent: wrapper ended without a terminal mark; recording forced \
         shutdown as Failed (outcome unknown)",
    );
    if let Err(e) = reg.mark_failed(id) {
        tracing::warn!(agent_id = %id, error = %e, "close_agent: mark_failed failed");
        return "failed";
    }
    reg.remove_terminal(id);
    "force_failed"
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
        let args: CloseAgentArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;
        let infra = infra_from(ctx)?;
        let target_id = match resolve_agent(&infra.registry, &args.agent_id)? {
            ResolvedAgent::Live(entry) => entry.id,
            // Soft success: the desired post-condition — the agent is not
            // running — already holds, and the registry retains the real
            // outcome. Reporting this as an error would only push the
            // model into pointless retries against an agent that no
            // longer exists; reporting it as a plain success would hide
            // that nothing was actually shut down. So: success, with the
            // recorded completion spelled out.
            ResolvedAgent::Reclaimed(tombstone) => {
                let completed_at = tombstone.completed_at.to_rfc3339();
                return Ok(ToolOutput::success(serde_json::json!({
                    "agent_id": tombstone.id.to_string(),
                    "path": tombstone.path,
                    "already_completed": true,
                    "status": tombstone.status,
                    "completed_at": completed_at,
                    "reason": args.reason,
                    "message": format!(
                        "agent '{}' already {} at {} and was reclaimed; nothing to close",
                        args.agent_id,
                        status_label(tombstone.status),
                        completed_at,
                    ),
                })));
            }
        };
        let handles = ctx.get_extension::<AgentHandles>();
        let handles_ref = handles.as_deref();

        // Collect the subtree once under a single read lock, then drop it
        // before performing the per-agent shutdown (which takes write locks).
        let mut order = {
            let reg = infra.registry.read();
            collect_subtree(&reg, target_id)
        };
        order.reverse();

        // Token-first cascade (W3.5): fire the target's cancellation
        // token *before* the leaves-first walk. With hierarchical child
        // tokens the cancel reaches every spawned descendant immediately
        // — including ones the closer holds no handle for — so each
        // descendant's run is already ending (real `Cancelled` outcome,
        // own wrapper's terminal sequence) by the time the walk reports
        // on it. The token is peeked without removing the handle; the
        // target's own shutdown below still takes ownership, cancels
        // again (idempotent), and joins the wrapper.
        let cascade_triggered = handles_ref
            .and_then(|h| h.cancel_token(target_id))
            .map(|token| token.cancel())
            .is_some();

        let (label, role) =
            sender_attribution(&infra.registry.read(), infra.agent_id, infra.parent_id);
        let closer = CloserAttribution {
            id: infra.agent_id,
            label,
            role,
        };
        let mut shut_down = Vec::with_capacity(order.len());
        for id in order {
            let status = shutdown_one(
                &infra.registry,
                handles_ref,
                id,
                args.reason.as_deref(),
                &closer,
                // Strict descendants only: the target itself is handled
                // through the held-handle join path, never the
                // cascade-report path.
                cascade_triggered && id != target_id,
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
        Ok(ToolOutput::success(payload))
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
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let root = register_agent(&registry, "/root", None);
        let child_a = register_agent(&registry, "/root/child-a", Some(root));
        let child_b = register_agent(&registry, "/root/child-b", Some(root));

        // The closer holds AgentHandles for every agent in the subtree —
        // close only force-stops agents whose handles it holds.
        let handles = Arc::new(AgentHandles::new());
        let (handle_root, _tx_root, _rx_root) = synthetic_handle(root);
        let (handle_a, _tx_a, _rx_a) = synthetic_handle(child_a);
        let (handle_b, _tx_b, _rx_b) = synthetic_handle(child_b);
        handles.insert(handle_root);
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
        assert!(!out.is_error(), "{:?}", out.content);
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

        // Terminal cleanup removes closed agents from the registry and frees
        // their paths.
        let reg = registry.read();
        assert!(reg.get(root).is_none(), "closed root removed");
        assert!(reg.get(child_a).is_none(), "closed child_a removed");
        assert!(reg.get(child_b).is_none(), "closed child_b removed");
        assert!(reg.get_by_path("/root").is_none(), "root path freed");
    }

    /// Forced-close record, wrapper-died-pre-mark window: the synthetic
    /// wrapper exits on cancellation without ever marking the registry, so
    /// after the join the entry is still live and the closer owns the
    /// lifecycle end. It cannot know the outcome the wrapper never
    /// recorded, so it must record `Failed` — never `Completed` — report
    /// the agent as `"force_failed"`, and leave a `Failed` tombstone.
    #[tokio::test]
    async fn close_agent_on_leaf_no_children_works() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let leaf = register_agent(&registry, "/leaf", None);

        let handles = Arc::new(AgentHandles::new());
        let (handle, _tx, _rx) = synthetic_handle(leaf);
        handles.insert(handle);

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(handles);

        let tool = CloseAgentTool::new();
        let envelope = envelope_for(
            "close_agent",
            json!({"agent_id": "/leaf", "reason": "done"}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("close");
        assert!(!out.is_error());
        let shut_down = out.content["shut_down"].as_array().expect("array");
        assert_eq!(shut_down.len(), 1);
        assert_eq!(shut_down[0]["agent_id"], leaf.to_string());
        assert_eq!(
            shut_down[0]["status"], "force_failed",
            "a wrapper that died before its terminal mark has an unknown \
             outcome — close must not report it completed",
        );

        // Terminal cleanup removes the closed leaf from the registry and
        // retains an honest completion record: Failed, never Completed.
        let reg = registry.read();
        assert!(reg.get(leaf).is_none(), "closed leaf removed");
        let tombstone = reg.tombstone(leaf).expect("closed leaf tombstoned");
        assert_eq!(tombstone.path, "/leaf");
        assert_eq!(
            tombstone.status,
            AgentStatus::Failed,
            "forced shutdown with unknown outcome is recorded Failed",
        );
    }

    /// A live agent whose handle the closer does not hold cannot be
    /// force-stopped: close must report it `"unreachable"` and leave its
    /// registry entry untouched — never mark a still-running agent
    /// completed or steal its wrapper's terminal transition.
    #[tokio::test]
    async fn close_agent_without_handle_reports_unreachable() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let peer = register_agent(&registry, "/peer", None);

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = CloseAgentTool::new();
        let envelope = envelope_for("close_agent", json!({"agent_id": "/peer"}));
        let out = tool.execute(&envelope, &ctx).await.expect("close");
        assert!(!out.is_error());
        let shut_down = out.content["shut_down"].as_array().expect("array");
        assert_eq!(shut_down.len(), 1);
        assert_eq!(
            shut_down[0]["status"], "unreachable",
            "close must say it cannot force-stop an agent it holds no handle for",
        );
        assert_eq!(
            registry.read().get(peer).expect("entry untouched").status,
            AgentStatus::Active,
            "the live entry must survive a no-handle close attempt",
        );
    }

    /// Bug 2 regression: closing an agent that already completed and was
    /// reclaimed is a soft success carrying the recorded outcome — not
    /// "could not resolve agent" / "not registered". Covers resolution by
    /// path and by UUID.
    #[tokio::test]
    async fn close_agent_on_reclaimed_agent_reports_soft_success() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let child = register_agent(&registry, "/smoke/child", None);
        registry.write().mark_completed(child).expect("complete");
        assert!(registry.write().remove_terminal(child), "reclaim");

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = CloseAgentTool::new();
        for identifier in ["/smoke/child".to_string(), child.to_string()] {
            let envelope = envelope_for("close_agent", json!({"agent_id": identifier}));
            let out = tool.execute(&envelope, &ctx).await.expect("close");
            assert!(
                !out.is_error(),
                "soft success, not an error: {:?}",
                out.content
            );
            assert_eq!(out.content["already_completed"], true);
            assert_eq!(out.content["agent_id"], child.to_string());
            assert_eq!(out.content["path"], "/smoke/child");
            assert_eq!(out.content["status"], "completed");
            assert!(
                out.content["completed_at"].as_str().is_some(),
                "the completion timestamp must surface: {:?}",
                out.content,
            );
            let message = out.content["message"].as_str().expect("message");
            assert!(
                message.contains("already completed") && message.contains("nothing to close"),
                "the message must state the truth: {message}",
            );
        }
    }

    /// "Not registered" is reserved for agents that never existed: an
    /// unknown UUID (no entry, no tombstone) still errors.
    #[tokio::test]
    async fn close_agent_rejects_never_existed_id() {
        let (infra, _registry, _router) = build_infra(Uuid::new_v4());
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = CloseAgentTool::new();
        let envelope = envelope_for(
            "close_agent",
            json!({"agent_id": Uuid::new_v4().to_string()}),
        );
        let err = tool.execute(&envelope, &ctx).await.expect_err("unknown id");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(
                    reason.contains("not registered") && reason.contains("no completion"),
                    "never-existed ids must be reported as such: {reason}",
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    /// Terminal-resurrection regression: closing a child that already
    /// failed on its own must NOT rewrite its outcome to Completed — the
    /// close path treats an already-terminal child as reclaim-only.
    #[tokio::test]
    async fn close_agent_reclaims_already_failed_child_without_rewriting() {
        use crate::agent::registry::AgentStatus;

        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let child = register_agent(&registry, "/parent/failed-child", None);
        registry
            .write()
            .mark_failed(child)
            .expect("mark child failed");
        assert_eq!(
            registry.read().get(child).expect("entry").status,
            AgentStatus::Failed,
        );

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = CloseAgentTool::new();
        let envelope = envelope_for(
            "close_agent",
            json!({"agent_id": child.to_string(), "reason": "sweep"}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("close");
        assert!(!out.is_error(), "{:?}", out.content);
        let shut_down = out.content["shut_down"].as_array().expect("array");
        assert_eq!(shut_down.len(), 1);
        assert_eq!(
            shut_down[0]["status"], "reclaimed",
            "an already-terminal child is reclaimed, never re-marked completed",
        );
        assert!(
            registry.read().get(child).is_none(),
            "the terminal entry is reclaimed from the registry",
        );
    }

    /// R3 + R4: `close_agent` sends a `Steer` shutdown message via the child's
    /// `InboundChannel` before cancelling its run and joining the wrapper,
    /// so a cooperating loop has a final boundary to observe the request.
    #[tokio::test]
    async fn close_agent_sends_shutdown_steer_to_child() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
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
        assert!(!out.is_error());

        let drained = inbound_rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].kind, MessageKind::Steer);
        assert_eq!(drained[0].content, "wrap up");
        assert_eq!(drained[0].sender_id, parent_id, "closer id is ground truth");
        assert_eq!(
            drained[0].from, "root",
            "unregistered parent-less closer attributes as root",
        );
    }
}
