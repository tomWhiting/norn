//! Durable fallback queueing for `signal_agent`.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::agent::{PendingAgentMessage, append_pending_message_audit};
use crate::error::ToolError;
use crate::r#loop::inbound::ChannelMessage;
use crate::tool::traits::ToolOutput;
use crate::tools::agent::infra::AgentToolInfra;

use super::signal_agent::SignalAgentArgs;
use super::signal_recipient::Recipient;

pub(super) fn queue_for_later_delivery(
    infra: &AgentToolInfra,
    recipient: &Recipient,
    args: &SignalAgentArgs,
    message: ChannelMessage,
    queued_at: DateTime<Utc>,
) -> Result<ToolOutput, ToolError> {
    let message_id = message.id;
    let mut pending = PendingAgentMessage::new(message, recipient.label.clone(), queued_at);
    let queued = infra
        .pending_messages
        .persist_for_registered_recipient(&mut pending)
        .map_err(|error| ToolError::ExecutionFailed {
            reason: format!(
                "failed to durably queue message {message_id} in the recipient timeline: {error}"
            ),
        })?;

    append_queue_observation(
        "sender",
        &infra.event_store,
        &queued.mailbox_store,
        &queued.observation,
    );
    if let Some(grant) = infra.grant.as_ref()
        && !Arc::ptr_eq(&grant.parent_store, &infra.event_store)
    {
        append_queue_observation(
            "scope-granting parent",
            &grant.parent_store,
            &queued.mailbox_store,
            &queued.observation,
        );
    }

    Ok(ToolOutput::success(serde_json::json!({
        "delivered": false,
        "queued": true,
        "delivery_state": "queued",
        "to": recipient.id.to_string(),
        "recipient": recipient.label,
        "kind": args.kind.as_str(),
        "message_id": message_id.to_string(),
        "queued_at": queued_at.to_rfc3339(),
        "resume_required": true,
        "note": "recipient is not currently attached to a live inbound route; the message is queued and will be injected through the normal agent_message path when that agent next resumes or wakes",
    })))
}

fn append_queue_observation(
    owner: &str,
    observation_store: &Arc<crate::session::store::EventStore>,
    mailbox_store: &Arc<crate::session::store::EventStore>,
    observation: &crate::agent::PendingAgentMessageLifecycle,
) {
    if Arc::ptr_eq(observation_store, mailbox_store) {
        return;
    }
    if let Err(error) = append_pending_message_audit(observation_store, observation) {
        tracing::error!(
            %error,
            owner,
            "message is durably queued in its recipient timeline, but a secondary queue audit failed",
        );
    }
}
