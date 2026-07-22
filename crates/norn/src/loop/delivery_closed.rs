//! Terminal persistence for messages accepted immediately before route close.

use crate::agent::{ClosedPendingMailbox, PendingAgentMessage, PendingAgentMessages};
use crate::error::SessionError;
use crate::r#loop::UndeliveredWindow;
use crate::r#loop::inbound::ChannelMessage;

/// Persist a controller-owned buffer after mailbox acceptance has closed.
///
/// These messages were already accepted by the live route. They receive a
/// canonical Q on the closed session timeline for a future direct resume, but
/// are not republished into a mailbox the terminating controller cannot drain.
pub(crate) fn persist_undelivered_after_close(
    pending: &PendingAgentMessages,
    closed: &ClosedPendingMailbox,
    messages: &mut Vec<ChannelMessage>,
    window: UndeliveredWindow,
) -> Result<(), SessionError> {
    let agent_id = closed.recipient_id();
    let mut hard_error = None;
    for mut message in std::mem::take(messages) {
        message.to_id = agent_id;
        let message_id = message.id;
        let kind = message.kind;
        let mut record =
            PendingAgentMessage::new(message, agent_id.to_string(), chrono::Utc::now());
        match pending.stage_after_close(closed, &mut record) {
            Ok(()) => tracing::debug!(
                %message_id,
                recipient = %agent_id,
                kind = kind.as_str(),
                window = window.as_str(),
                "accepted inbound message reached a closing controller; staged for terminal persistence",
            ),
            Err(error) => {
                tracing::error!(
                    %message_id,
                    %error,
                    window = window.as_str(),
                    "failed to stage a message accepted before terminal mailbox closure",
                );
                if hard_error.is_none() {
                    hard_error = Some(error);
                }
            }
        }
    }
    let finalization = pending.finalize_closed_pending(closed);
    if let Some(error) = hard_error {
        return Err(error);
    }
    finalization
}
