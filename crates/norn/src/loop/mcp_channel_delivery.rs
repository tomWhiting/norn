//! Owned external-channel delivery with retained claims and exact conversation persistence.

use uuid::Uuid;

use crate::error::{ConfigError, SessionError};
use crate::integration::hooks::HookRegistry;
use crate::integration::{
    McpChannelDelivery, McpChannelError, McpChannelHost, McpChannelInbox, McpChannelLimits,
    frame_mcp_channel_message,
};
use crate::provider::request::{Message, MessageRole, ToolCallCaller};
use crate::provider::{AgentEventSender, McpChannelDeliveryEvent};
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

use super::append_idempotent_off_executor;
use super::loop_context::LoopContext;

/// Single receiving owner for one running agent's external MCP messages.
///
/// Admission is in memory. A message becomes consumed only after its exact
/// framed conversation event is persisted; this is not an upstream receipt.
pub struct McpChannelSession {
    recipient_id: Uuid,
    inbox: McpChannelInbox,
    prepared: Option<PreparedDelivery>,
}

struct PreparedDelivery {
    delivery: McpChannelDelivery,
    event: SessionEvent,
    content: String,
}

impl McpChannelSession {
    /// Host authority for attaching sources and inspecting/releasing the bounded inbox.
    pub fn host(&self) -> McpChannelHost {
        self.inbox.host()
    }

    /// The actual agent identity fixed at installation.
    pub const fn recipient_id(&self) -> Uuid {
        self.recipient_id
    }

    /// Wait for Wake input without consuming it or parsing it as a host command.
    pub async fn wake_ready(&self) -> Result<(), McpChannelError> {
        if self
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.delivery.wakes_session())
        {
            return Ok(());
        }
        self.inbox.wake_ready().await
    }

    /// Published held message identities, in admission order.
    pub fn held_message_ids(&self) -> Vec<Uuid> {
        self.inbox.held_message_ids()
    }

    async fn flush(
        &mut self,
        store: &EventStore,
        messages: &mut Vec<Message>,
        hooks: Option<&HookRegistry>,
        wake_only: bool,
        event_tx: Option<&AgentEventSender>,
    ) -> Result<Vec<EventId>, SessionError> {
        // A boundary owns only the work admitted when it starts. A continuously
        // producing source cannot extend this sweep forever and starve the model.
        let through = self.inbox.admitted_sequence();
        let mut delivered = Vec::new();
        loop {
            if self.prepared.is_none() {
                let claim = match self.inbox.claim_through(wake_only, through) {
                    Ok(claim) => claim,
                    Err(McpChannelError::Closed { .. }) => return Ok(delivered),
                    Err(error) => return Err(channel_session_error(&error)),
                };
                let Some(delivery) = claim else {
                    return Ok(delivered);
                };
                let content = frame_mcp_channel_message(delivery.message());
                let mut base = EventBase::new(store.last_event_id());
                base.id = EventId::from_stable_namespace(format!(
                    "mcp-channel-delivery:{}:{}",
                    self.recipient_id,
                    delivery.message().id()
                ));
                self.prepared = Some(PreparedDelivery {
                    delivery,
                    event: SessionEvent::UserMessage {
                        base,
                        content: content.clone(),
                    },
                    content,
                });
            }
            let Some(prepared) = self.prepared.as_mut() else {
                return Err(SessionError::EventAppendFailed {
                    reason: format!(
                        "channel delivery for {} lost its prepared claim",
                        self.recipient_id
                    ),
                });
            };
            let event_id = append_idempotent_off_executor(store, prepared.event.clone())?;
            prepared
                .delivery
                .consume_retained()
                .map_err(|error| channel_session_error(&error))?;
            let Some(prepared) = self.prepared.take() else {
                return Err(SessionError::EventAppendFailed {
                    reason: format!(
                        "channel delivery for {} lost its persisted event",
                        self.recipient_id
                    ),
                });
            };
            // No cancellation point separates persistence, quota release and
            // local conversation insertion. Hooks observe already-consumed data.
            messages.push(Message {
                response_items: Vec::new(),
                role: MessageRole::User,
                content: Some(prepared.content),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: ToolCallCaller::Absent,
            });
            if let Some(event_tx) = event_tx {
                let message = prepared.delivery.message();
                let observation = McpChannelDeliveryEvent {
                    event_id: event_id.clone(),
                    message_id: message.id(),
                    recipient_id: message.recipient_id(),
                    source: message.source().to_owned(),
                    generation: message.generation(),
                    sequence: message.sequence(),
                    content: message.content().to_owned(),
                };
                if let Err(error) = event_tx.send_mcp_channel(observation) {
                    tracing::warn!(%error, event_id = %event_id,
                        "channel input is persisted but live observation is unavailable");
                }
            }
            delivered.push(event_id);
            if let Some(hooks) = hooks {
                hooks.run_on_event(&prepared.event).await;
            }
        }
    }
}

impl LoopContext {
    /// Install one bounded channel owner after the root's real identity has been resolved.
    ///
    /// # Errors
    /// Refuses missing agent identity or a second receiving owner.
    pub fn install_mcp_channel_inbox(
        &mut self,
        limits: McpChannelLimits,
    ) -> Result<McpChannelHost, ConfigError> {
        let Some(recipient_id) = self.agent_id else {
            return Err(ConfigError::InvalidConfig {
                reason: "MCP channel inbox requires the running agent's resolved identity"
                    .to_owned(),
            });
        };
        if self.mcp_channel_session.is_some() {
            return Err(ConfigError::InvalidConfig {
                reason: format!("agent {recipient_id} already owns an MCP channel inbox"),
            });
        }
        let inbox = McpChannelInbox::new(recipient_id, limits);
        let host = inbox.host();
        self.mcp_channel_session = Some(McpChannelSession {
            recipient_id,
            inbox,
            prepared: None,
        });
        Ok(host)
    }
}

pub(super) async fn flush_mcp_channel_messages(
    store: &EventStore,
    messages: &mut Vec<Message>,
    context: &mut LoopContext,
    wake_only: bool,
    event_tx: Option<&AgentEventSender>,
) -> Result<Vec<EventId>, SessionError> {
    let Some(session) = context.mcp_channel_session.as_mut() else {
        return Ok(Vec::new());
    };
    if context.agent_id != Some(session.recipient_id) {
        return Err(SessionError::EventAppendFailed {
            reason: format!(
                "MCP channel recipient {} does not match running agent {:?}",
                session.recipient_id, context.agent_id
            ),
        });
    }
    session
        .flush(
            store,
            messages,
            context.hooks.as_deref(),
            wake_only,
            event_tx,
        )
        .await
}

fn channel_session_error(error: &McpChannelError) -> SessionError {
    SessionError::EventAppendFailed {
        reason: format!("MCP channel delivery: {error}"),
    }
}

#[cfg(test)]
#[path = "mcp_channel_delivery_tests.rs"]
mod tests;
