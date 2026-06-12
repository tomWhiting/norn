//! `SignalAgentTool` — steer a child via its inbound channel.

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;

use super::helpers::sender_label;
use crate::agent::registry::AgentStatus;
use crate::error::ToolError;
use crate::r#loop::inbound::{ChannelMessage, DeliveryMode};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::agent::handle::AgentHandles;
use crate::tools::agent::infra::{ResolvedAgent, infra_from, resolve_agent};

/// Sends a steering signal to a child agent.
///
/// Delivery requires the caller to hold an
/// [`crate::tools::agent::handle::AgentHandle`] for the recipient: the
/// signal travels through the child's
/// [`crate::r#loop::inbound::InboundChannel`] (`Steer` / `FollowUp`) and
/// drains at the child's next tool boundary. When no handle is held there is
/// no channel any loop drains, so the tool returns a structured delivery
/// failure instead of pretending the message was sent.
pub struct SignalAgentTool;

impl SignalAgentTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SignalAgentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct SignalAgentArgs {
    to: String,
    content: serde_json::Value,
    #[serde(default)]
    trigger_turn: Option<bool>,
}

#[async_trait]
impl Tool for SignalAgentTool {
    fn name(&self) -> &'static str {
        "signal_agent"
    }

    fn description(&self) -> &'static str {
        include_str!("../../guidance/signal_agent.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../../guidance/signal_agent.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["to", "content"],
            "additionalProperties": false,
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Recipient agent identified by hierarchical registry path (e.g. \"/workers/analyzer\") or UUID."
                },
                "content": {
                    "description": "Message payload — any valid JSON value. Use structured objects for machine-readable coordination."
                },
                "trigger_turn": {
                    "type": "boolean",
                    "description": "When true, the recipient processes this message at its next tool boundary rather than waiting for its current step to finish. Defaults to false."
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
        let args: SignalAgentArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;
        let infra = infra_from(ctx)?;

        // Resolve including finished agents so the failure mode is honest:
        // a recipient that completed (terminal entry or reclaimed
        // tombstone) gets "already completed at <ts>", never the dishonest
        // "not registered" — that wording is reserved for identifiers that
        // never existed.
        let (to_id, finished) = match resolve_agent(&infra.registry, &args.to)? {
            ResolvedAgent::Live(entry) if !entry.status.is_terminal() => (entry.id, None),
            ResolvedAgent::Live(entry) => (entry.id, Some((entry.status, entry.completed_at))),
            ResolvedAgent::Reclaimed(tombstone) => (
                tombstone.id,
                Some((tombstone.status, Some(tombstone.completed_at))),
            ),
        };
        if let Some((status, completed_at)) = finished {
            let when =
                completed_at.map_or_else(|| "an unrecorded time".to_owned(), |ts| ts.to_rfc3339());
            let outcome = if status == AgentStatus::Failed {
                "failed"
            } else {
                "completed"
            };
            return Ok(ToolOutput::failure_with_content(
                serde_json::json!({
                    "delivered": false,
                    "to": to_id.to_string(),
                    "recipient_status": status,
                    "completed_at": completed_at.map(|ts| ts.to_rfc3339()),
                }),
                crate::tool::failure::ToolErrorPayload::new(
                    crate::tool::failure::ToolErrorKind::NotFound,
                    format!(
                        "recipient already finished: agent '{}' {outcome} at {when} and can \
                         no longer receive signals. Its run is over — read its delivered \
                         result instead of signalling it.",
                        args.to,
                    ),
                )
                .with_detail(serde_json::json!({ "to": args.to })),
            ));
        }
        let trigger_turn = args.trigger_turn.unwrap_or(false);

        // When the parent holds an AgentHandle for the recipient, route
        // through the child's InboundChannel. This is the primary path for
        // parent → child steering.
        if let Some(handles) = ctx.get_extension::<AgentHandles>()
            && let Some(inbound_tx) = handles.inbound_tx(to_id)
        {
            let delivery = if trigger_turn {
                DeliveryMode::Steer
            } else {
                DeliveryMode::FollowUp
            };
            let body = match &args.content {
                serde_json::Value::String(s) => s.clone(),
                other => serde_json::to_string(other).map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("signal_agent: could not serialize content: {e}"),
                })?,
            };
            let author = sender_label(&infra.registry.read(), infra.agent_id);
            let msg = ChannelMessage {
                author,
                content: body,
                delivery,
                timestamp: Utc::now(),
            };
            inbound_tx
                .send(msg)
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("inbound send failed: {e}"),
                })?;

            let delivery_label = match delivery {
                DeliveryMode::Steer => "steer",
                DeliveryMode::FollowUp => "follow_up",
            };
            let payload = serde_json::json!({
                "to": to_id.to_string(),
                "delivery": delivery_label,
                "routed_via": "inbound_channel",
                "trigger_turn": trigger_turn,
            });
            return Ok(ToolOutput::success(payload));
        }

        // H15: no AgentHandle means there is no inbound channel to deliver
        // through. The shared mailbox is NOT a delivery path — no agent loop
        // drains it — so queueing there would report success for a message
        // nobody will ever read. Fail honestly with the reason so the model
        // can pick a reachable recipient or restructure its coordination.
        let payload = serde_json::json!({
            "delivered": false,
            "to": to_id.to_string(),
        });
        Ok(ToolOutput::failure_with_content(
            payload,
            crate::tool::failure::ToolErrorPayload::new(
                crate::tool::failure::ToolErrorKind::NotFound,
                format!(
                    "no delivery channel to recipient: signal_agent can only deliver to \
                     agents whose handle this agent holds — children it spawned or forked \
                     itself. '{}' is registered but is not a direct child of this agent, \
                     so there is no inbound channel to deliver through. Signal your own \
                     children directly, or route the message via the recipient's parent.",
                    args.to,
                ),
            )
            .with_detail(serde_json::json!({ "to": args.to })),
        ))
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

    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::tools::agent::coord::test_support::{
        build_infra, envelope_for, register_agent, synthetic_handle,
    };

    /// R4: when the parent holds an `AgentHandle` for the recipient, the
    /// signal is delivered as a Steer `ChannelMessage` via the child's
    /// `InboundChannel` rather than the mailbox.
    #[tokio::test]
    async fn signal_agent_routes_steer_via_inbound_channel() {
        let sender = Uuid::new_v4();
        let (infra, registry, mailbox) = build_infra(sender);
        let recipient = register_agent(&registry, "/parent/child", Some(sender));

        let handles = Arc::new(AgentHandles::new());
        let (handle, _status_tx, mut inbound_rx) = synthetic_handle(recipient);
        handles.insert(handle);

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(handles);

        let tool = SignalAgentTool::new();
        let envelope = envelope_for(
            "signal_agent",
            json!({
                "to": "/parent/child",
                "content": {"redirect": "stop"},
                "trigger_turn": true,
            }),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("send");
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["routed_via"], "inbound_channel");
        assert_eq!(out.content["delivery"], "steer");

        let drained = inbound_rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].delivery, DeliveryMode::Steer);
        assert!(
            drained[0].content.contains("redirect"),
            "serialized json content: {}",
            drained[0].content
        );
        assert!(mailbox.recv(recipient).is_empty(), "no mailbox traffic");
    }

    /// R4: `trigger_turn: false` maps to a `FollowUp` message — buffered
    /// until the child would otherwise stop.
    #[tokio::test]
    async fn signal_agent_routes_follow_up_via_inbound_channel() {
        let sender = Uuid::new_v4();
        let (infra, registry, _mailbox) = build_infra(sender);
        let recipient = register_agent(&registry, "/parent/child", Some(sender));

        let handles = Arc::new(AgentHandles::new());
        let (handle, _status_tx, mut inbound_rx) = synthetic_handle(recipient);
        handles.insert(handle);

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(handles);

        let tool = SignalAgentTool::new();
        let envelope = envelope_for(
            "signal_agent",
            json!({"to": "/parent/child", "content": "fyi"}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("send");
        assert!(!out.is_error());
        assert_eq!(out.content["delivery"], "follow_up");

        let drained = inbound_rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].delivery, DeliveryMode::FollowUp);
        assert_eq!(drained[0].content, "fyi");
    }

    /// H15: a recipient the sender holds no `AgentHandle` for is
    /// unreachable. The tool must report a structured delivery failure —
    /// never a fake success into a queue nothing drains.
    #[tokio::test]
    async fn signal_agent_reports_delivery_failure_for_non_child() {
        let sender = Uuid::new_v4();
        let (infra, registry, mailbox) = build_infra(sender);
        let recipient = register_agent(&registry, "/peer", None);

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = SignalAgentTool::new();
        let envelope = envelope_for(
            "signal_agent",
            json!({
                "to": "/peer",
                "content": {"hello": "world"},
                "trigger_turn": true,
            }),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("executes");
        assert!(
            out.is_error(),
            "delivery failure must be an error the model sees"
        );
        assert_eq!(out.content["delivered"], false);
        assert_eq!(out.content["to"], recipient.to_string());
        assert!(
            out.content["error"]["message"]
                .as_str()
                .is_some_and(|r| r.contains("inbound channel")),
            "the failure explains why delivery is impossible: {:?}",
            out.content,
        );
        assert!(
            mailbox.recv(recipient).is_empty(),
            "nothing may be queued into the undrained mailbox",
        );
    }

    /// H15: `AgentHandles` present but the recipient is not tracked —
    /// same structured delivery failure, no mailbox side effects.
    #[tokio::test]
    async fn signal_agent_reports_delivery_failure_when_handle_absent() {
        let sender = Uuid::new_v4();
        let (infra, registry, mailbox) = build_infra(sender);
        let recipient = register_agent(&registry, "/peer", None);

        let handles = Arc::new(AgentHandles::new());
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(handles);

        let tool = SignalAgentTool::new();
        let envelope = envelope_for(
            "signal_agent",
            json!({"to": "/peer", "content": "hi", "trigger_turn": false}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("executes");
        assert!(out.is_error());
        assert_eq!(out.content["delivered"], false);
        assert!(
            mailbox.recv(recipient).is_empty(),
            "nothing may be queued into the undrained mailbox",
        );
    }

    #[tokio::test]
    async fn signal_agent_rejects_unknown_path() {
        let (infra, _registry, _mailbox) = build_infra(Uuid::new_v4());

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = SignalAgentTool::new();
        let envelope = envelope_for("signal_agent", json!({"to": "/missing", "content": null}));
        let err = tool.execute(&envelope, &ctx).await.expect_err("missing");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    /// Bug 2 regression: signalling an agent that already finished —
    /// whether its terminal entry is still listed or it was reclaimed
    /// down to a tombstone — fails honestly with the recorded completion,
    /// never the dishonest "not registered".
    #[tokio::test]
    async fn signal_agent_reports_finished_recipient_honestly() {
        let sender = Uuid::new_v4();
        let (infra, registry, _mailbox) = build_infra(sender);
        let recipient = register_agent(&registry, "/parent/done-child", Some(sender));
        registry
            .write()
            .mark_completed(recipient)
            .expect("complete");

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = SignalAgentTool::new();
        // Terminal-but-unreclaimed: resolvable by path despite the freed
        // path index, and reported as finished.
        let envelope = envelope_for(
            "signal_agent",
            json!({"to": "/parent/done-child", "content": "hi"}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("executes");
        assert!(out.is_error(), "delivery to a finished agent must fail");
        assert_eq!(out.content["delivered"], false);
        assert_eq!(out.content["recipient_status"], "completed");
        let message = out.content["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("already finished") && message.contains("completed at"),
            "the failure must state the recorded completion: {message}",
        );

        // Reclaimed: the tombstone keeps the truth available, by path and
        // by UUID.
        assert!(registry.write().remove_terminal(recipient), "reclaim");
        for identifier in ["/parent/done-child".to_string(), recipient.to_string()] {
            let envelope = envelope_for("signal_agent", json!({"to": identifier, "content": "hi"}));
            let out = tool.execute(&envelope, &ctx).await.expect("executes");
            assert!(out.is_error());
            assert_eq!(out.content["delivered"], false);
            assert_eq!(out.content["recipient_status"], "completed");
            assert!(
                out.content["completed_at"].as_str().is_some(),
                "the completion timestamp must surface: {:?}",
                out.content,
            );
            let message = out.content["error"]["message"].as_str().expect("message");
            assert!(
                !message.contains("not registered"),
                "'not registered' is reserved for agents that never existed: {message}",
            );
        }
    }
}
