//! Provider conversation-state request shaping.

use crate::error::ProviderError;
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode};
use crate::provider::request::{Message, ProviderContextManagement};
use crate::provider::tools::ProviderCapabilities;
use crate::session::events::SessionEvent;

/// Stored Responses API anchor visible in the local prompt view.
#[derive(Debug, Clone)]
pub(super) struct ResponseThreadAnchor {
    response_id: String,
    input_start: usize,
}

/// Locate the newest assistant response in the prompt view.
pub(super) fn latest_response_anchor(
    events: &[SessionEvent],
    prefix_len: usize,
    include_compactions: bool,
) -> Option<ResponseThreadAnchor> {
    let mut message_index = prefix_len;
    let mut anchor = None;
    for event in events {
        if event_produces_prompt_message(event, include_compactions) {
            message_index = message_index.saturating_add(1);
        }
        if let SessionEvent::AssistantMessage { response_id, .. } = event
            && let Some(response_id) = response_id.as_ref().filter(|id| !id.is_empty())
        {
            anchor = Some(ResponseThreadAnchor {
                response_id: response_id.clone(),
                input_start: message_index,
            });
        }
    }
    anchor
}

fn event_produces_prompt_message(event: &SessionEvent, include_compactions: bool) -> bool {
    matches!(
        event,
        SessionEvent::UserMessage { .. }
            | SessionEvent::AssistantMessage { .. }
            | SessionEvent::ToolResult { .. }
    ) || (include_compactions && matches!(event, SessionEvent::Compaction { .. }))
}

/// Mutable provider-state anchor for one agent-loop step.
#[derive(Debug)]
pub(super) struct ConversationRequestState {
    threaded: bool,
    previous_response_id: Option<String>,
    prefix_len: usize,
    input_start: usize,
}

impl ConversationRequestState {
    /// Create request state for the current prompt.
    pub(super) fn new(
        config: &AgentLoopConfig,
        capabilities: ProviderCapabilities,
        prefix_len: usize,
        thread_anchor: Option<ResponseThreadAnchor>,
    ) -> Result<Self, ProviderError> {
        validate_provider_state_config(config, capabilities)?;
        let threaded = match config.conversation_state {
            ConversationStateMode::Auto => capabilities.response_threading,
            ConversationStateMode::ManualReplay => false,
            ConversationStateMode::ProviderThreaded => true,
        };
        let anchor = threaded.then_some(thread_anchor).flatten();
        let (previous_response_id, input_start) = if let Some(anchor) = anchor {
            (Some(anchor.response_id), anchor.input_start)
        } else {
            (None, 0)
        };
        Ok(Self {
            threaded,
            previous_response_id,
            prefix_len,
            input_start,
        })
    }

    /// Whether response storage should be enabled for this request.
    pub(super) const fn store(&self) -> bool {
        self.threaded
    }

    /// Previous response ID to pass to the provider.
    pub(super) fn previous_response_id(&self) -> Option<String> {
        self.previous_response_id.clone()
    }

    /// Provider-side context management for this request.
    pub(super) fn context_management(
        config: &AgentLoopConfig,
    ) -> Option<ProviderContextManagement> {
        if config.conversation_state == ConversationStateMode::ManualReplay {
            return None;
        }
        config
            .server_compaction_threshold_tokens
            .map(|compact_threshold_tokens| ProviderContextManagement {
                compact_threshold_tokens,
            })
    }

    /// Build the message slice for this provider request.
    pub(super) fn request_messages(&self, messages: &[Message]) -> Vec<Message> {
        if !self.threaded || self.previous_response_id.is_none() || self.input_start == 0 {
            return messages.to_vec();
        }

        let mut request_messages = Vec::new();
        request_messages.extend(messages.iter().take(self.prefix_len).cloned());
        request_messages.extend(messages.iter().skip(self.input_start).cloned());
        request_messages
    }

    /// Update the anchor after a provider response.
    pub(super) fn observe_response(&mut self, response_id: Option<&str>, next_input_start: usize) {
        if !self.threaded {
            return;
        }
        if let Some(response_id) = response_id.filter(|id| !id.is_empty()) {
            self.previous_response_id = Some(response_id.to_owned());
            self.input_start = next_input_start;
        } else {
            self.previous_response_id = None;
            self.input_start = 0;
        }
    }

    /// Adjust the input cursor after a message is inserted before it.
    pub(super) fn note_inserted_message(&mut self, index: usize) {
        if index <= self.prefix_len {
            self.prefix_len = self.prefix_len.saturating_add(1);
        }
        if index <= self.input_start {
            self.input_start = self.input_start.saturating_add(1);
        }
    }

    /// Adjust the input cursor after a message is removed before it.
    pub(super) fn note_removed_message(&mut self, index: usize) {
        if index < self.prefix_len {
            self.prefix_len = self.prefix_len.saturating_sub(1);
        }
        if index < self.input_start {
            self.input_start = self.input_start.saturating_sub(1);
        }
    }
}

fn validate_provider_state_config(
    config: &AgentLoopConfig,
    capabilities: ProviderCapabilities,
) -> Result<(), ProviderError> {
    if config.conversation_state == ConversationStateMode::ProviderThreaded
        && !capabilities.response_threading
    {
        return Err(ProviderError::UnsupportedFeature {
            feature: "provider_threaded conversation state".to_string(),
        });
    }
    if config.server_compaction_threshold_tokens.is_some()
        && config.conversation_state == ConversationStateMode::ManualReplay
    {
        return Err(ProviderError::UnsupportedFeature {
            feature: "server-side response compaction without provider_threaded conversation state"
                .to_string(),
        });
    }
    if config.server_compaction_threshold_tokens.is_some() && !capabilities.response_threading {
        return Err(ProviderError::UnsupportedFeature {
            feature: "server-side response compaction without response threading".to_string(),
        });
    }
    if config.server_compaction_threshold_tokens.is_some() && !capabilities.server_compaction {
        return Err(ProviderError::UnsupportedFeature {
            feature: "server-side response compaction".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provider::request::MessageRole;
    use crate::session::events::{EventBase, EventUsage};
    use crate::session::store::EventStore;

    fn message(role: MessageRole, content: &str) -> Message {
        Message {
            role,
            content: Some(content.to_string()),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        }
    }

    fn config(mode: ConversationStateMode) -> AgentLoopConfig {
        AgentLoopConfig {
            conversation_state: mode,
            ..AgentLoopConfig::default()
        }
    }

    #[test]
    fn threaded_request_keeps_instructions_and_only_new_input() {
        let store = EventStore::new();
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: "old answer".to_string(),
                thinking: String::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_string(),
                response_id: Some("resp_old".to_string()),
            })
            .unwrap();

        let state = ConversationRequestState::new(
            &config(ConversationStateMode::ProviderThreaded),
            ProviderCapabilities::openai_responses(),
            2,
            latest_response_anchor(&store.events(), 2, false),
        )
        .unwrap();
        let messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::Developer, "dynamic"),
            message(MessageRole::User, "old"),
            message(MessageRole::User, "new"),
        ];

        let request_messages = state.request_messages(&messages);

        assert_eq!(state.previous_response_id().as_deref(), Some("resp_old"));
        assert_eq!(request_messages.len(), 3);
        assert_eq!(request_messages[0].role, MessageRole::System);
        assert_eq!(request_messages[1].role, MessageRole::Developer);
        assert_eq!(request_messages[2].content.as_deref(), Some("new"));
    }

    #[test]
    fn threaded_request_excludes_replay_only_developer_history() {
        let store = EventStore::new();
        store
            .append(SessionEvent::Compaction {
                base: EventBase::new(None),
                summary: "older history".to_string(),
                replaced_event_ids: Vec::new(),
            })
            .unwrap();
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "old".to_string(),
            })
            .unwrap();
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: "old answer".to_string(),
                thinking: String::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_string(),
                response_id: Some("resp_old".to_string()),
            })
            .unwrap();

        let state = ConversationRequestState::new(
            &config(ConversationStateMode::ProviderThreaded),
            ProviderCapabilities::openai_responses(),
            1,
            latest_response_anchor(&store.events(), 1, true),
        )
        .unwrap();
        let messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::Developer, "old compaction summary"),
            message(MessageRole::User, "old"),
            message(MessageRole::Assistant, "old answer"),
            message(MessageRole::User, "new"),
        ];

        let request_messages = state.request_messages(&messages);

        assert_eq!(request_messages.len(), 2);
        assert_eq!(request_messages[0].content.as_deref(), Some("system"));
        assert_eq!(request_messages[1].content.as_deref(), Some("new"));
    }

    #[test]
    fn unsupported_threading_is_rejected() {
        let err = ConversationRequestState::new(
            &config(ConversationStateMode::ProviderThreaded),
            ProviderCapabilities::default(),
            1,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ProviderError::UnsupportedFeature { .. }));
    }

    #[test]
    fn auto_mode_falls_back_when_provider_cannot_thread() {
        let state = ConversationRequestState::new(
            &config(ConversationStateMode::Auto),
            ProviderCapabilities::default(),
            1,
            Some(ResponseThreadAnchor {
                response_id: "resp_old".to_string(),
                input_start: 1,
            }),
        )
        .unwrap();

        assert_eq!(state.previous_response_id(), None);
        assert!(!state.store());
    }

    #[test]
    fn auto_mode_threads_when_provider_supports_it() {
        let state = ConversationRequestState::new(
            &config(ConversationStateMode::Auto),
            ProviderCapabilities::openai_responses(),
            1,
            Some(ResponseThreadAnchor {
                response_id: "resp_old".to_string(),
                input_start: 1,
            }),
        )
        .unwrap();

        assert_eq!(state.previous_response_id().as_deref(), Some("resp_old"));
        assert!(state.store());
    }

    #[test]
    fn threaded_anchor_starts_after_latest_visible_response() {
        let store = EventStore::new();
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: "old answer".to_string(),
                thinking: String::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_string(),
                response_id: Some("resp_old".to_string()),
            })
            .unwrap();
        store
            .append(SessionEvent::ToolResult {
                base: EventBase::new(None),
                tool_call_id: "call_old".to_string(),
                tool_name: "read".to_string(),
                output: serde_json::json!({"ok": true}),
                duration_ms: 1,
            })
            .unwrap();
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "queued user".to_string(),
            })
            .unwrap();

        let state = ConversationRequestState::new(
            &config(ConversationStateMode::ProviderThreaded),
            ProviderCapabilities::openai_responses(),
            1,
            latest_response_anchor(&store.events(), 1, false),
        )
        .unwrap();
        let messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::Assistant, "old answer"),
            message(MessageRole::ToolResult, "tool result"),
            message(MessageRole::User, "queued user"),
            message(MessageRole::User, "new user"),
        ];

        let request_messages = state.request_messages(&messages);

        assert_eq!(state.previous_response_id().as_deref(), Some("resp_old"));
        assert_eq!(request_messages.len(), 4);
        assert_eq!(request_messages[0].content.as_deref(), Some("system"));
        assert_eq!(request_messages[1].content.as_deref(), Some("tool result"));
        assert_eq!(request_messages[2].content.as_deref(), Some("queued user"));
        assert_eq!(request_messages[3].content.as_deref(), Some("new user"));
    }

    #[test]
    fn server_compaction_requires_threaded_state() {
        let err = ConversationRequestState::new(
            &AgentLoopConfig {
                conversation_state: ConversationStateMode::ManualReplay,
                server_compaction_threshold_tokens: Some(100),
                ..AgentLoopConfig::default()
            },
            ProviderCapabilities::openai_responses(),
            1,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ProviderError::UnsupportedFeature { .. }));
    }
}
