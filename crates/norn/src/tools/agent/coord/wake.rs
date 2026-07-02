//! `WakeAgentTool` — resume an idle spawned child so it drains its mailbox.

use async_trait::async_trait;
use serde::Deserialize;

use crate::agent::child_policy::MessagingScope;
use crate::agent::registry::{AgentEntry, AgentStatus};
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::agent::handle::{AgentWakeRegistry, WakeRequestOutcome};
use crate::tools::agent::infra::{ResolvedAgent, infra_from, resolve_agent};

/// Stable tool name for waking idle spawned children.
pub const WAKE_AGENT_TOOL_NAME: &str = "wake_agent";

/// Resume an idle spawned child for one mailbox-draining step.
pub struct WakeAgentTool;

impl WakeAgentTool {
    /// Constructs the tool.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for WakeAgentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct WakeAgentArgs {
    agent_id: String,
}

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

fn failure(kind: ToolErrorKind, reason: String, detail: serde_json::Value) -> ToolOutput {
    ToolOutput::failure_with_content(
        serde_json::json!({ "woken": false }),
        ToolErrorPayload::new(kind, reason).with_detail(detail),
    )
}

fn scope_denial(
    infra: &crate::tools::agent::infra::AgentToolInfra,
    target: &AgentEntry,
    requested: &str,
) -> Result<Option<ToolOutput>, ToolError> {
    let denial = |scope: &str, reason: String| {
        Some(failure(
            ToolErrorKind::PermissionDenied,
            reason,
            serde_json::json!({
                "agent_id": requested,
                "to": target.id.to_string(),
                "scope": scope,
            }),
        ))
    };

    let Some(sender_parent) = infra.parent_id else {
        if target.parent_id == Some(infra.agent_id) {
            return Ok(None);
        }
        return Ok(denial(
            "root",
            format!(
                "out of scope: a root agent may wake only its own children; \
                 '{requested}' is not a child of this agent."
            ),
        ));
    };

    let Some(policy) = infra.grant.as_ref().map(|grant| &grant.policy) else {
        return Err(ToolError::ExecutionFailed {
            reason: "wake_agent: this agent has a parent but no granted ChildPolicy on \
                     its AgentToolInfra — the spawning runtime must stamp the policy from \
                     its CoordinationEnvelope at launch. This is a harness configuration \
                     error, not a model error."
                .to_owned(),
        });
    };

    let allowed = match policy.messaging {
        MessagingScope::None => {
            return Ok(denial(
                "none",
                "wake_agent is not available under this agent's messaging scope \
                 (\"none\") — the spawning parent granted no messaging capability."
                    .to_owned(),
            ));
        }
        MessagingScope::ParentOnly => target.id == sender_parent,
        MessagingScope::SiblingsAndParent => {
            target.id == sender_parent || target.parent_id == Some(sender_parent)
        }
    };
    if allowed {
        return Ok(None);
    }

    let (scope, description) = match policy.messaging {
        MessagingScope::ParentOnly => ("parent_only", "it may wake only its parent"),
        MessagingScope::SiblingsAndParent => (
            "siblings_and_parent",
            "it may wake its siblings (children of the same parent) and its parent",
        ),
        MessagingScope::None => ("none", "messaging is not granted"),
    };
    Ok(denial(
        scope,
        format!(
            "out of scope: this agent's messaging scope is \"{scope}\" — {description}; \
             '{requested}' is neither."
        ),
    ))
}

#[async_trait]
impl Tool for WakeAgentTool {
    fn name(&self) -> &'static str {
        WAKE_AGENT_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Wake an idle spawned child so it drains queued agent messages from its mailbox."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["agent_id"],
            "additionalProperties": false,
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Target idle spawned agent identified by hierarchical registry path or UUID."
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
        let args: WakeAgentArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;
        let infra = infra_from(ctx)?;
        let target = match resolve_agent(&infra.registry, &args.agent_id)? {
            ResolvedAgent::Live(entry) => entry,
            ResolvedAgent::Reclaimed(tombstone) => {
                return Ok(failure(
                    ToolErrorKind::NotFound,
                    format!(
                        "agent '{}' already {} at {} and cannot be woken",
                        args.agent_id,
                        status_label(tombstone.status),
                        tombstone.completed_at.to_rfc3339(),
                    ),
                    serde_json::json!({ "agent_id": args.agent_id }),
                ));
            }
        };
        if let Some(denial) = scope_denial(&infra, &target, &args.agent_id)? {
            return Ok(denial);
        }
        if target.status.is_terminal() {
            return Ok(failure(
                ToolErrorKind::NotFound,
                format!(
                    "agent '{}' is terminal ({}) and cannot be woken",
                    args.agent_id,
                    status_label(target.status),
                ),
                serde_json::json!({
                    "agent_id": args.agent_id,
                    "status": target.status,
                }),
            ));
        }
        if target.status != AgentStatus::Idle {
            return Ok(ToolOutput::success(serde_json::json!({
                "woken": false,
                "already_active": true,
                "agent_id": target.id.to_string(),
                "status": target.status,
                "message": format!(
                    "agent '{}' is {}, so no wake is required",
                    args.agent_id,
                    status_label(target.status),
                ),
            })));
        }

        let pending = infra.pending_messages.messages_for_delivery(target.id);
        if pending.is_empty() {
            return Ok(failure(
                ToolErrorKind::Blocked,
                format!(
                    "agent '{}' is idle but has no queued mailbox messages to drain",
                    args.agent_id,
                ),
                serde_json::json!({ "agent_id": args.agent_id }),
            ));
        }

        let wake_registry = ctx.require_extension::<AgentWakeRegistry>()?;
        let outcome = wake_registry.request_wake(target.id);
        match outcome {
            WakeRequestOutcome::Queued => Ok(ToolOutput::success(serde_json::json!({
                "woken": true,
                "agent_id": target.id.to_string(),
                "status": target.status,
                "queued_messages": pending.len(),
                "message": "wake queued; the agent will drain its mailbox on its next step",
            }))),
            WakeRequestOutcome::AlreadyQueued => Ok(ToolOutput::success(serde_json::json!({
                "woken": false,
                "already_queued": true,
                "agent_id": target.id.to_string(),
                "queued_messages": pending.len(),
            }))),
            WakeRequestOutcome::AlreadyActive(status) => {
                Ok(ToolOutput::success(serde_json::json!({
                    "woken": false,
                    "already_active": true,
                    "agent_id": target.id.to_string(),
                    "status": status,
                })))
            }
            WakeRequestOutcome::Terminal(status) => Ok(failure(
                ToolErrorKind::NotFound,
                format!(
                    "agent '{}' is terminal ({}) and cannot be woken",
                    args.agent_id,
                    status_label(status),
                ),
                serde_json::json!({ "agent_id": args.agent_id, "status": status }),
            )),
            WakeRequestOutcome::NotRegistered => Ok(failure(
                ToolErrorKind::NotFound,
                format!(
                    "agent '{}' has no wake controller registered; only spawned idle agents are wakeable",
                    args.agent_id,
                ),
                serde_json::json!({ "agent_id": args.agent_id }),
            )),
            WakeRequestOutcome::ChannelClosed => Ok(failure(
                ToolErrorKind::NotFound,
                format!(
                    "agent '{}' wake controller is closed and cannot accept a wake",
                    args.agent_id,
                ),
                serde_json::json!({ "agent_id": args.agent_id }),
            )),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args
)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use chrono::Utc;
    use serde_json::json;
    use tokio::sync::{mpsc, watch};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::agent::PendingAgentMessage;
    use crate::r#loop::inbound::{ChannelMessage, MessageKind, inbound_channel};
    use crate::tools::agent::coord::test_support::{build_infra, envelope_for, register_agent};
    use crate::tools::agent::handle::{AgentHandle, ChildBranchMetadata};

    fn ctx_with(
        infra: Arc<crate::tools::agent::infra::AgentToolInfra>,
        wake_registry: Arc<AgentWakeRegistry>,
    ) -> ToolContext {
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(wake_registry);
        ctx
    }

    fn register_wake_handle(
        wake_registry: &AgentWakeRegistry,
        agent_id: Uuid,
        status: AgentStatus,
    ) -> mpsc::Receiver<()> {
        let (_status_tx, status_rx) = watch::channel(status);
        let (inbound_tx, _inbound_rx) = inbound_channel(8);
        let (wake_tx, wake_rx) = mpsc::channel(1);
        let handle = AgentHandle {
            agent_id,
            status_rx,
            inbound_tx,
            wake_tx,
            wake_pending: Arc::new(AtomicBool::new(false)),
            cancel: CancellationToken::new(),
            join_handle: tokio::spawn(async {}),
            event_store: Arc::new(crate::session::store::EventStore::new()),
            branch_metadata: ChildBranchMetadata {
                child_agent_id: agent_id,
                parent_agent_id: Uuid::nil(),
                profile_name: None,
                spawned_at: Utc::now(),
            },
        };
        wake_registry.insert(handle.wake_handle());
        wake_rx
    }

    fn queue_pending_message(infra: &crate::tools::agent::infra::AgentToolInfra, recipient: Uuid) {
        let message = ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: infra.agent_id,
            from: "root".to_owned(),
            role: None,
            to_id: recipient,
            content: "queued work".to_owned(),
            kind: MessageKind::Steer,
            seq: None,
            timestamp: Utc::now(),
        };
        let pending = PendingAgentMessage::new(message, "/root/child".to_owned(), Utc::now());
        assert!(
            infra.pending_messages.queue(pending).is_some(),
            "test message id must be fresh",
        );
    }

    #[tokio::test]
    async fn wake_agent_refuses_idle_agent_with_empty_mailbox() {
        let sender = Uuid::new_v4();
        let (infra, registry, _router) = build_infra(sender);
        let child = register_agent(&registry, "/root/child", Some(sender));
        registry.write().mark_idle(child).expect("mark idle");
        let wake_registry = Arc::new(AgentWakeRegistry::new());
        let _wake_rx = register_wake_handle(&wake_registry, child, AgentStatus::Idle);
        let ctx = ctx_with(Arc::clone(&infra), wake_registry);

        let out = WakeAgentTool::new()
            .execute(
                &envelope_for(WAKE_AGENT_TOOL_NAME, json!({ "agent_id": "/root/child" })),
                &ctx,
            )
            .await
            .expect("wake executes");

        assert!(out.is_error());
        assert_eq!(out.content["woken"], false);
    }

    /// Seam I2-3 (deregistration drain): a message the router accepted
    /// but the child's controller swept out of the channel at
    /// deregistration lands in the durable pending store — and the
    /// pending-store-only wake gate, now authoritative, accepts the wake.
    #[tokio::test]
    async fn wake_agent_succeeds_for_message_stranded_at_deregistration() {
        let sender = Uuid::new_v4();
        let (infra, registry, _router) = build_infra(sender);
        let child = register_agent(&registry, "/root/child", Some(sender));
        registry.write().mark_idle(child).expect("mark idle");

        // A message sits undrained in the child's inbound channel — the
        // deregistration window. The controller's sweep queues it durably.
        let (inbound_tx, mut inbound_rx) = inbound_channel(8);
        inbound_tx
            .send(ChannelMessage {
                id: Uuid::new_v4(),
                sender_id: sender,
                from: "root".to_owned(),
                role: None,
                to_id: child,
                content: "stranded at deregistration".to_owned(),
                kind: MessageKind::Steer,
                seq: Some(1),
                timestamp: Utc::now(),
            })
            .await
            .expect("send");
        let store = crate::session::store::EventStore::new();
        crate::tools::agent::spawn_launch::requeue_stranded_inbound(
            &store,
            child,
            Some(&infra.pending_messages),
            &mut inbound_rx,
            crate::r#loop::UndeliveredWindow::Deregistration,
        );
        assert_eq!(infra.pending_messages.pending_for(child), 1);
        assert_eq!(
            store.len(),
            1,
            "the sweep must persist one agent_message.queued audit",
        );

        let wake_registry = Arc::new(AgentWakeRegistry::new());
        let mut wake_rx = register_wake_handle(&wake_registry, child, AgentStatus::Idle);
        let ctx = ctx_with(Arc::clone(&infra), wake_registry);

        let out = WakeAgentTool::new()
            .execute(
                &envelope_for(WAKE_AGENT_TOOL_NAME, json!({ "agent_id": "/root/child" })),
                &ctx,
            )
            .await
            .expect("wake executes");

        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["woken"], true);
        assert_eq!(out.content["queued_messages"], 1);
        assert!(wake_rx.recv().await.is_some(), "wake signal delivered");
    }

    #[tokio::test]
    async fn wake_agent_sends_single_wake_for_queued_mailbox() {
        let sender = Uuid::new_v4();
        let (infra, registry, _router) = build_infra(sender);
        let child = register_agent(&registry, "/root/child", Some(sender));
        registry.write().mark_idle(child).expect("mark idle");
        queue_pending_message(&infra, child);
        let wake_registry = Arc::new(AgentWakeRegistry::new());
        let mut wake_rx = register_wake_handle(&wake_registry, child, AgentStatus::Idle);
        let ctx = ctx_with(Arc::clone(&infra), wake_registry);

        let out = WakeAgentTool::new()
            .execute(
                &envelope_for(WAKE_AGENT_TOOL_NAME, json!({ "agent_id": "/root/child" })),
                &ctx,
            )
            .await
            .expect("wake executes");

        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["woken"], true);
        assert_eq!(out.content["queued_messages"], 1);
        assert!(wake_rx.recv().await.is_some(), "wake signal delivered");
    }
}
