//! `SignalAgentTool` — steer a child agent through the
//! [`MessageRouter`](crate::agent::message_router::MessageRouter).
//!
//! W3.1 transition note: this tool coexists with the router for exactly
//! one step of the Wave 3 rollout and is deleted when `send_message`
//! replaces it (W3.2) — never released alongside it. Delivery already
//! goes through the router (sequence minting, framed injection,
//! `agent_message.*` audit events); only route *registration* still
//! piggybacks on the [`AgentHandles`] the caller holds, because the
//! spawn/fork wrappers take over registration in W3.2.

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::helpers::sender_attribution;
use crate::agent::message_router::RouteError;
use crate::agent::registry::AgentStatus;
use crate::error::ToolError;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::provider::agent_event::{
    AgentEventSender, AgentMessageLifecycle, SharedAgentEventChannel,
};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::agent::append_message_audit;
use crate::tools::agent::handle::AgentHandles;
use crate::tools::agent::infra::{ResolvedAgent, infra_from, resolve_agent};

/// Sends a steering signal to a child agent.
///
/// Delivery travels the recipient's
/// [`InboundChannel`](crate::r#loop::inbound::InboundChannel) via the
/// shared [`MessageRouter`](crate::agent::message_router::MessageRouter)
/// and drains at the recipient's next step boundary, injected as a
/// harness-framed `<agent_message>` turn. When the recipient has no live
/// route — it is not a child this agent launched, and nothing else
/// registered it — the tool returns a structured delivery failure instead
/// of pretending the message was sent.
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
                    "description": "When true, the recipient processes this message at its next tool boundary (steer) rather than waiting for its current step to finish (update). Defaults to false."
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
        let (to_id, to_label, finished) = match resolve_agent(&infra.registry, &args.to)? {
            ResolvedAgent::Live(entry) if !entry.status.is_terminal() => {
                (entry.id, entry.path, None)
            }
            ResolvedAgent::Live(entry) => (
                entry.id,
                entry.path,
                Some((entry.status, entry.completed_at)),
            ),
            ResolvedAgent::Reclaimed(tombstone) => (
                tombstone.id,
                tombstone.path,
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
        let kind = if trigger_turn {
            MessageKind::Steer
        } else {
            MessageKind::Update
        };

        // W3.1 transition: route registration still rides on the handle
        // this agent holds for children it launched. Registration is
        // idempotent and preserves the recipient's sequence counter; the
        // spawn/fork wrappers own it from W3.2 on.
        if let Some(handles) = ctx.get_extension::<AgentHandles>()
            && let Some(inbound_tx) = handles.inbound_tx(to_id)
        {
            infra.router.register(to_id, inbound_tx);
        }

        let body = match &args.content {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string(other).map_err(|e| ToolError::ExecutionFailed {
                reason: format!("signal_agent: could not serialize content: {e}"),
            })?,
        };
        let (from_label, from_role) =
            sender_attribution(&infra.registry.read(), infra.agent_id, infra.parent_id);
        let message_id = Uuid::new_v4();
        let sent_at = Utc::now();
        let msg = ChannelMessage {
            id: message_id,
            sender_id: infra.agent_id,
            from: from_label.clone(),
            role: from_role,
            to_id,
            content: body.clone(),
            kind,
            seq: None,
            timestamp: sent_at,
        };

        match infra.router.deliver(to_id, msg).await {
            Ok(seq) => {
                // Audit: the router accepted the send. Store carrier on
                // the sender's own store, live carrier on the shared
                // broadcast channel when one is installed — mirroring the
                // SubagentLifecycle dual-carrier contract.
                let sent = AgentMessageLifecycle::Sent {
                    message_id,
                    from_id: infra.agent_id,
                    from: from_label.clone(),
                    to_id,
                    to: to_label,
                    kind,
                    seq,
                    content: body,
                    sent_at,
                };
                append_message_audit(&infra.event_store, &sent);
                if let Some(channel) = ctx.get_extension::<SharedAgentEventChannel>() {
                    AgentEventSender::new(channel.0.clone(), infra.agent_id, from_label)
                        .send_message(sent);
                }
                Ok(ToolOutput::success(serde_json::json!({
                    "delivered": true,
                    "to": to_id.to_string(),
                    "kind": kind.as_str(),
                    "seq": seq,
                    "message_id": message_id.to_string(),
                    "routed_via": "message_router",
                    "trigger_turn": trigger_turn,
                })))
            }
            // H15: a send the router did not accept is reported as exactly
            // that — no Sent audit event is emitted (nothing was accepted)
            // and nothing is queued where no loop drains.
            Err(RouteError::NotRouted { .. }) => Ok(ToolOutput::failure_with_content(
                serde_json::json!({
                    "delivered": false,
                    "to": to_id.to_string(),
                }),
                crate::tool::failure::ToolErrorPayload::new(
                    crate::tool::failure::ToolErrorKind::NotFound,
                    format!(
                        "no delivery route to recipient: '{}' is registered but has no live \
                         inbound route in the message router. signal_agent can only deliver \
                         to agents whose handle this agent holds — children it spawned or \
                         forked itself. Signal your own children directly, or route the \
                         message via the recipient's parent.",
                        args.to,
                    ),
                )
                .with_detail(serde_json::json!({ "to": args.to })),
            )),
            Err(e @ (RouteError::ChannelClosed { .. } | RouteError::ChannelFull { .. })) => {
                Ok(ToolOutput::failure_with_content(
                    serde_json::json!({
                        "delivered": false,
                        "to": to_id.to_string(),
                    }),
                    crate::tool::failure::ToolErrorPayload::new(
                        crate::tool::failure::ToolErrorKind::ExecutionFailed,
                        format!(
                            "delivery to '{}' failed: {e}. The recipient's loop ended between \
                             resolution and delivery — read its result instead of signalling it.",
                            args.to,
                        ),
                    )
                    .with_detail(serde_json::json!({ "to": args.to })),
                ))
            }
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

    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::provider::agent_event::{AGENT_MESSAGE_SENT_EVENT_TYPE, AgentEvent, AgentEventKind};
    use crate::session::events::SessionEvent;
    use crate::tools::agent::coord::test_support::{
        build_infra, envelope_for, register_agent, synthetic_handle,
    };

    /// R4: when the parent holds an `AgentHandle` for the recipient, the
    /// signal is delivered as a Steer `ChannelMessage` via the router onto
    /// the child's `InboundChannel`, with harness-resolved attribution and
    /// a router-minted sequence number.
    #[tokio::test]
    async fn signal_agent_routes_steer_via_router() {
        let sender = Uuid::new_v4();
        let (infra, registry, router) = build_infra(sender);
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
        assert_eq!(out.content["routed_via"], "message_router");
        assert_eq!(out.content["kind"], "steer");
        assert_eq!(out.content["seq"], 1);
        assert!(router.is_routed(recipient), "route registered from handle");

        let drained = inbound_rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].kind, MessageKind::Steer);
        assert_eq!(drained[0].seq, Some(1), "router-minted sequence");
        assert_eq!(drained[0].sender_id, sender, "ground-truth sender id");
        assert_eq!(drained[0].to_id, recipient);
        assert_eq!(
            drained[0].from, "root",
            "unregistered parent-less sender attributes as root",
        );
        assert!(
            drained[0].content.contains("redirect"),
            "serialized json content: {}",
            drained[0].content
        );
    }

    /// R4: `trigger_turn: false` maps to an Update message — buffered
    /// until the child would otherwise stop.
    #[tokio::test]
    async fn signal_agent_routes_update_via_router() {
        let sender = Uuid::new_v4();
        let (infra, registry, _router) = build_infra(sender);
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
        assert_eq!(out.content["kind"], "update");

        let drained = inbound_rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].kind, MessageKind::Update);
        assert_eq!(drained[0].content, "fyi");
    }

    /// Audit: every accepted send appends exactly one
    /// `agent_message.sent` Custom event to the sender's store with the
    /// verbatim content, and broadcasts a sender-tagged live event when a
    /// channel is installed. A registered sender attributes by path and
    /// role.
    #[tokio::test]
    async fn signal_agent_emits_sent_audit_on_both_carriers() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        // Re-key the infra to a *registered* sender so attribution uses
        // registry ground truth.
        let sender = register_agent(&registry, "/orchestrator", None);
        let recipient = register_agent(&registry, "/orchestrator/worker", Some(sender));
        let sender_store = Arc::clone(&infra.event_store);
        let infra = Arc::new(crate::tools::agent::infra::AgentToolInfra {
            registry: Arc::clone(&infra.registry),
            router: Arc::clone(&infra.router),
            provider: Arc::clone(&infra.provider),
            event_store: Arc::clone(&sender_store),
            agent_id: sender,
            parent_id: None,
            tool_registry: None,
        });

        let handles = Arc::new(AgentHandles::new());
        let (handle, _status_tx, mut inbound_rx) = synthetic_handle(recipient);
        handles.insert(handle);

        let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(16);
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(handles);
        ctx.insert_extension(Arc::new(SharedAgentEventChannel(tx)));

        let tool = SignalAgentTool::new();
        let envelope = envelope_for(
            "signal_agent",
            json!({"to": "/orchestrator/worker", "content": "report <now> & \"fully\""}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("send");
        assert!(!out.is_error(), "{:?}", out.content);

        // Store carrier: one Sent event, verbatim (unescaped) content.
        let events = sender_store.events();
        let sent_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::Custom {
                    event_type, data, ..
                } if event_type == AGENT_MESSAGE_SENT_EVENT_TYPE => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(sent_events.len(), 1, "exactly one Sent per accepted send");
        let data = sent_events[0];
        assert_eq!(data["phase"], "sent");
        assert_eq!(data["from"], "/orchestrator");
        assert_eq!(data["from_id"], sender.to_string());
        assert_eq!(data["to"], "/orchestrator/worker");
        assert_eq!(data["to_id"], recipient.to_string());
        assert_eq!(data["kind"], "update");
        assert_eq!(data["seq"], 1);
        assert_eq!(
            data["content"], "report <now> & \"fully\"",
            "audit stores the unescaped content verbatim",
        );

        // Live carrier: sender-tagged Message event.
        let live = rx.try_recv().expect("live Sent event broadcast");
        assert_eq!(live.agent_id, sender);
        assert!(matches!(
            live.event,
            AgentEventKind::Message(AgentMessageLifecycle::Sent { .. })
        ));

        // The delivered message attributes the registered sender's path
        // and role.
        let drained = inbound_rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].from, "/orchestrator");
        assert_eq!(drained[0].role.as_deref(), Some("worker"));
    }

    /// H15: a recipient with no live route is unreachable. The tool must
    /// report a structured delivery failure — never a fake success into a
    /// queue nothing drains — and emit no Sent audit event.
    #[tokio::test]
    async fn signal_agent_reports_delivery_failure_for_non_child() {
        let sender = Uuid::new_v4();
        let (infra, registry, router) = build_infra(sender);
        let recipient = register_agent(&registry, "/peer", None);
        let sender_store = Arc::clone(&infra.event_store);

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
                .is_some_and(|r| r.contains("inbound route")),
            "the failure explains why delivery is impossible: {:?}",
            out.content,
        );
        assert!(!router.is_routed(recipient), "no route was fabricated");
        assert!(
            sender_store.events().is_empty(),
            "a rejected send must not leave a Sent audit event",
        );
    }

    /// H15: `AgentHandles` present but the recipient is not tracked —
    /// same structured delivery failure.
    #[tokio::test]
    async fn signal_agent_reports_delivery_failure_when_handle_absent() {
        let sender = Uuid::new_v4();
        let (infra, registry, router) = build_infra(sender);
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
        assert!(!router.is_routed(recipient));
    }

    /// A recipient whose loop ended between resolution and delivery (its
    /// inbound receiver is gone) fails honestly as a closed channel, with
    /// no Sent audit event.
    #[tokio::test]
    async fn signal_agent_reports_closed_channel_honestly() {
        let sender = Uuid::new_v4();
        let (infra, registry, router) = build_infra(sender);
        let recipient = register_agent(&registry, "/parent/child", Some(sender));
        let sender_store = Arc::clone(&infra.event_store);

        // Route registered, but the receiver half is already dropped.
        let (tx, rx) = crate::r#loop::inbound::inbound_channel(4);
        router.register(recipient, tx);
        drop(rx);

        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = SignalAgentTool::new();
        let envelope = envelope_for(
            "signal_agent",
            json!({"to": "/parent/child", "content": "hi"}),
        );
        let out = tool.execute(&envelope, &ctx).await.expect("executes");
        assert!(out.is_error());
        assert_eq!(out.content["delivered"], false);
        let message = out.content["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("loop ended"),
            "closed-channel failure names the cause: {message}",
        );
        assert!(
            sender_store.events().is_empty(),
            "no Sent event for a send the channel rejected",
        );
    }

    #[tokio::test]
    async fn signal_agent_rejects_unknown_path() {
        let (infra, _registry, _router) = build_infra(Uuid::new_v4());

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
        let (infra, registry, _router) = build_infra(sender);
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
