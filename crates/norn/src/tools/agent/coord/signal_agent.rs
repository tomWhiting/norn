//! `SignalAgentTool` — inter-agent messaging through the
//! [`MessageRouter`](crate::agent::message_router::MessageRouter) (Wave 3).
//!
//! ## Naming history
//!
//! W3.2 deleted the original `signal_agent` tool outright (no alias, no
//! shim) and replaced it with this router-backed messaging tool, which was
//! briefly named `send_message` between W3.2 and the commit that renamed
//! it back to `signal_agent`. The rename exists because meridian — the
//! downstream platform that embeds norn — registers its own workspace
//! member-messaging tool, and two "send message"-named tools in one
//! registry would confuse the model. Only the tool-surface name changed:
//! the args (`to`/`kind`/`content`), scope enforcement, and audit events
//! carried over unchanged, and nothing of the pre-W3.2 `signal_agent`
//! semantics returned with the name.
//!
//! Delivery travels the recipient's
//! [`InboundChannel`](crate::agent_loop::inbound::InboundChannel) and drains at
//! the recipient loop's step boundaries, injected as a harness-framed
//! `<agent_message>` turn. Route registration is owned by the spawn/fork
//! launch wrappers (register while a step is running, deregister while a
//! spawned child is idle or terminal); this tool only routes — it never
//! registers.
//!
//! ## Scope enforcement (Wave 3 §Permissioning)
//!
//! Who may message whom is decided here, against registry ground truth,
//! from the [`MessagingScope`] the sender's parent granted at spawn/fork
//! time ([`AgentToolInfra::grant`]):
//!
//! - [`MessagingScope::SiblingsAndParent`] — the sender may message
//!   children of its own parent (siblings, itself included) and its
//!   parent. One hop only: a grandchild never reaches the root directly.
//! - [`MessagingScope::ParentOnly`] — the sender may message only its
//!   parent.
//! - [`MessagingScope::None`] — the tool is removed from the child's
//!   surface at spawn time and refused here as defense-in-depth.
//! - A **root agent** (no parent, so no granted scope) is governed
//!   structurally: it may message its own children, nothing else.
//!
//! The scope check runs *before* the finished-recipient check, so an
//! out-of-scope sender learns nothing about an out-of-scope agent's
//! completion record. Known disclosure property (inherent in the spec's
//! resolve-then-scope order): identifier resolution itself distinguishes
//! "unknown identifier" from "out of scope", and the denial payload
//! carries the resolved UUID — a sender can probe whether a path exists
//! anywhere in the tree and learn its path→UUID mapping, but never an
//! outcome; every subsequent use of that UUID is itself scope-checked.
//!
//! ## Audit (Wave 3 §Audit trail)
//!
//! Every live-router send appends one `agent_message.sent` record to the
//! sender's own store **and** to the scope-granting parent's store
//! ([`ParentGrant::parent_store`]), and broadcasts a sender-tagged live
//! event when a channel is installed. A resolved, in-scope recipient with no
//! live route is a second accepted state: the message is recorded
//! authoritatively in the recipient's own timeline before it enters the shared
//! pending store. Sender/parent copies are explicitly non-authoritative audit
//! observations. The recipient drains its mailbox on the next resumed or
//! wake-triggered loop step. Terminal recipients still fail honestly with
//! their recorded outcome.

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::helpers::sender_attribution;
use crate::agent::message_router::RouteError;
use crate::error::ToolError;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::provider::agent_event::{
    AgentEventSender, AgentMessageLifecycle, SharedAgentEventChannel,
};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::follow_up::{
    BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction, FollowUpArgsMode,
};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::agent::infra::infra_from;
use crate::tools::agent::{WAKE_AGENT_TOOL_NAME, append_message_audit};

use super::signal_queue::queue_for_later_delivery;
use super::signal_recipient::{
    finished_failure, resolve_recipient, scope_denial, terminal_route_failure,
};

/// Public tool name for the Norn inter-agent messaging tool.
pub const SIGNAL_AGENT_TOOL_NAME: &str = "signal_agent";

/// Sends a message to another agent through the shared message router.
///
/// Recipients are addressed by hierarchical registry path, UUID, or the
/// literal `"parent"`. `kind` selects delivery semantics: `steer` drains at
/// the recipient's next step boundary and wakes a lingering recipient;
/// `update` batches at step boundaries and never wakes a lingering
/// recipient. Failures are structured and honest: out-of-scope target,
/// already-finished recipient (with its recorded completion), or no live
/// live inbound route, or queued into the shared pending-message store when
/// the recipient is valid but not currently attached to a route.
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
pub(super) struct SignalAgentArgs {
    pub(super) to: String,
    pub(super) kind: MessageKind,
    pub(super) content: String,
}

#[async_trait]
impl Tool for SignalAgentTool {
    fn name(&self) -> &'static str {
        SIGNAL_AGENT_TOOL_NAME
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
            "required": ["to", "kind", "content"],
            "additionalProperties": false,
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Recipient: hierarchical registry path (e.g. \"/workers/analyzer\"), UUID, or the literal \"parent\"."
                },
                "kind": {
                    "type": "string",
                    "enum": ["steer", "update"],
                    "description": "steer = act on this now: drains at the recipient's next step boundary and wakes a lingering recipient. update = FYI context: batches at step boundaries and does not wake a lingering recipient."
                },
                "content": {
                    "type": "string",
                    "description": "Message body. Delivered inside a harness-built <agent_message> frame with escaped content — structured payloads go in the string."
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

        let recipient = match resolve_recipient(&infra, &args.to)? {
            Ok(recipient) => recipient,
            Err(failure) => return Ok(failure),
        };

        // Scope before completion state: an out-of-scope sender learns
        // nothing about an out-of-scope agent's recorded outcome.
        if let Some(denial) = scope_denial(&infra, &recipient, &args.to)? {
            return Ok(denial);
        }
        if let Some((status, completed_at)) = recipient.finished {
            return Ok(finished_failure(&recipient, &args.to, status, completed_at));
        }

        let (from_label, from_role) =
            sender_attribution(&infra.registry.read(), infra.agent_id, infra.parent_id);
        let message_id = Uuid::new_v4();
        let sent_at = Utc::now();
        let msg = ChannelMessage {
            id: message_id,
            sender_id: infra.agent_id,
            from: from_label.clone(),
            role: from_role,
            to_id: recipient.id,
            content: args.content.clone(),
            kind: args.kind,
            seq: None,
            timestamp: sent_at,
        };

        match infra.router.deliver(recipient.id, msg.clone()).await {
            Ok(seq) => {
                // Audit: the router accepted the send. Store carriers on
                // the sender's own store AND the scope-granting parent's
                // store (dual-store rule, Wave 3 §Audit trail); live
                // carrier on the shared broadcast channel when installed.
                let sent = AgentMessageLifecycle::Sent {
                    message_id,
                    from_id: infra.agent_id,
                    from: from_label.clone(),
                    to_id: recipient.id,
                    to: recipient.label,
                    kind: args.kind,
                    seq,
                    content: args.content,
                    sent_at,
                };
                // The Sent audit joins the primary write-through contract
                // (session-fidelity Gap 10). The message is ALREADY
                // delivered at this point — the router accepted it — so a
                // persist failure surfaces typed with wording that rules
                // out a duplicate resend, never as a silent audit hole.
                let audit_failed =
                    |which: &str, error: crate::error::SessionError| ToolError::ExecutionFailed {
                        reason: format!(
                            "message {message_id} WAS delivered (seq {seq}); do \
                             NOT resend it. Persisting the durable Sent audit \
                             to the {which} failed: {error}",
                        ),
                    };
                append_message_audit(&infra.event_store, &sent)
                    .map_err(|error| audit_failed("sender's session store", error))?;
                if let Some(grant) = infra.grant.as_ref() {
                    append_message_audit(&grant.parent_store, &sent)
                        .map_err(|error| audit_failed("parent's session store", error))?;
                }
                if let Some(channel) = ctx.get_extension::<SharedAgentEventChannel>() {
                    AgentEventSender::new(channel.0.clone(), infra.agent_id, from_label)
                        .send_message(sent);
                }
                Ok(ToolOutput::success(serde_json::json!({
                    "delivered": true,
                    "to": recipient.id.to_string(),
                    "kind": args.kind.as_str(),
                    "seq": seq,
                    "message_id": message_id.to_string(),
                })))
            }
            // A router miss has two honest outcomes after rechecking
            // lifecycle truth: terminal recipients fail with their recorded
            // finish; still-valid recipients are accepted into the durable
            // pending-message store for a future resumed loop step. No Sent
            // audit event is emitted because the live router did not accept
            // the send.
            Err(e @ RouteError::NotRouted { .. }) => {
                if let Some(output) = terminal_route_failure(&infra, &recipient, &args.to) {
                    return Ok(output);
                }
                tracing::debug!(
                    recipient_id = %recipient.id,
                    error = %e,
                    "signal_agent: recipient has no live route; queueing for later delivery",
                );
                queue_for_later_delivery(&infra, &recipient, &args, msg, sent_at)
            }
            Err(e @ RouteError::ChannelClosed { .. }) => {
                if let Some(output) = terminal_route_failure(&infra, &recipient, &args.to) {
                    return Ok(output);
                }
                tracing::debug!(
                    recipient_id = %recipient.id,
                    error = %e,
                    "signal_agent: route closed before enqueue; queueing for later delivery",
                );
                queue_for_later_delivery(&infra, &recipient, &args, msg, sent_at)
            }
            // Structurally unreachable on this path: `deliver()` awaits
            // capacity, so it never reports Full (only the sync
            // `try_deliver` can). Kept exhaustive without a wildcard, and
            // honest if the router's contract ever changes.
            Err(e @ RouteError::ChannelFull { .. }) => Ok(ToolOutput::failure_with_content(
                serde_json::json!({
                    "delivered": false,
                    "to": recipient.id.to_string(),
                }),
                ToolErrorPayload::new(
                    ToolErrorKind::ExecutionFailed,
                    format!(
                        "delivery to '{}' failed: {e}. The recipient's bounded inbound \
                             channel is at capacity; retry after it drains a step boundary.",
                        args.to,
                    ),
                )
                .with_detail(serde_json::json!({ "to": args.to })),
            )),
        }
    }

    async fn register_follow_ups(
        &self,
        output: &ToolOutput,
        _ctx: &ToolContext,
    ) -> Vec<FollowUpAction> {
        if output
            .content
            .get("queued")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Vec::new();
        }
        let Some(agent_id) = output.content.get("to").and_then(serde_json::Value::as_str) else {
            return Vec::new();
        };
        vec![FollowUpAction {
            action: "wake_agent".to_owned(),
            description: "Wake the recipient so it drains the queued mailbox message.".to_owned(),
            tool: WAKE_AGENT_TOOL_NAME.to_owned(),
            args: serde_json::json!({ "agent_id": agent_id }),
            args_mode: FollowUpArgsMode::Replace,
            expires: ExpiryCondition::Never,
            confidence: Confidence::High,
            before_content: BeforeContentSource::Unavailable,
        }]
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
#[path = "signal_agent_tests/mod.rs"]
mod tests;
