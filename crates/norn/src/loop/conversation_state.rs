//! Provider conversation-state request shaping.

use crate::error::ProviderError;
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode};
use crate::provider::ProviderStateIdentity;
use crate::provider::request::{Message, ProviderContextManagement};
use crate::provider::tools::ProviderCapabilities;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;
use crate::session::{
    ActiveResponseProvenance, ResponseStateDisposition, discover_active_response_provenance,
    event_cuts_response_anchor,
};

/// Validate every reserved provider-state record without deriving a request.
pub(super) fn validate_response_state_provenance(
    events: &[SessionEvent],
) -> Result<(), ProviderError> {
    discover_active_response_provenance(events)
        .map_err(|_error| ProviderError::ProviderStateProvenanceInvalid)?;
    Ok(())
}

/// Stored Responses API anchor visible in the local prompt view.
#[derive(Debug, Clone)]
pub(super) struct ResponseThreadAnchor {
    response_id: String,
    input_start: usize,
}

impl ResponseThreadAnchor {
    pub(super) fn witness_message<'a>(&self, messages: &'a [Message]) -> Option<&'a Message> {
        self.input_start
            .checked_sub(1)
            .and_then(|index| messages.get(index))
    }
}

/// Proven current anchor plus unmarked pre-D3 candidates, newest first.
pub(super) struct ResponseThreadAnchors {
    pub(super) proven: Option<ResponseThreadAnchor>,
    pub(super) legacy_candidates: Vec<ResponseThreadAnchor>,
}

#[cfg(test)]
impl ResponseThreadAnchor {
    /// Construct an anchor directly (for tests in sibling modules that
    /// cannot name the private fields).
    pub(super) fn for_test(response_id: String, input_start: usize) -> Self {
        Self {
            response_id,
            input_start,
        }
    }
}

/// Locate the newest assistant response in the prompt view.
#[cfg(test)]
pub(super) fn latest_response_anchor(
    events: &[SessionEvent],
    prefix_len: usize,
    include_compactions: bool,
) -> Result<Option<ResponseThreadAnchor>, ProviderError> {
    let provenance = discover_active_response_provenance(events)
        .map_err(|_error| ProviderError::ProviderStateProvenanceInvalid)?;
    Ok(
        response_thread_anchors_in_epoch(events, prefix_len, include_compactions, &provenance)
            .proven,
    )
}

/// Locate the latest visible anchor while respecting omitted epoch cuts.
#[cfg(test)]
pub(super) fn latest_response_anchor_for_prompt_view(
    visible_events: &[SessionEvent],
    store: &EventStore,
    prefix_len: usize,
    include_compactions: bool,
) -> Result<Option<ResponseThreadAnchor>, ProviderError> {
    Ok(response_thread_anchors_for_prompt_view(
        visible_events,
        store,
        prefix_len,
        include_compactions,
    )?
    .proven)
}

/// Locate proven and pre-provenance anchors in the active prompt view.
pub(super) fn response_thread_anchors_for_prompt_view(
    visible_events: &[SessionEvent],
    store: &EventStore,
    prefix_len: usize,
    include_compactions: bool,
) -> Result<ResponseThreadAnchors, ProviderError> {
    let provenance = store
        .with_events(discover_active_response_provenance)
        .map_err(|_error| ProviderError::ProviderStateProvenanceInvalid)?;
    Ok(response_thread_anchors_in_epoch(
        visible_events,
        prefix_len,
        include_compactions,
        &provenance,
    ))
}

fn response_thread_anchors_in_epoch(
    events: &[SessionEvent],
    prefix_len: usize,
    include_compactions: bool,
    provenance: &ActiveResponseProvenance,
) -> ResponseThreadAnchors {
    let mut message_index = prefix_len;
    let mut proven = None;
    let mut legacy_candidates = Vec::new();
    for event in events {
        if event_produces_prompt_message(event, include_compactions) {
            message_index = message_index.saturating_add(1);
        }
        match event {
            SessionEvent::AssistantMessage { response_id, .. }
                if response_id.as_ref().is_some_and(|id| !id.is_empty()) =>
            {
                let Some(response_id) = response_id.as_ref() else {
                    continue;
                };
                let candidate = ResponseThreadAnchor {
                    response_id: response_id.clone(),
                    input_start: message_index,
                };
                match provenance.disposition(&event.base().id) {
                    Some(ResponseStateDisposition::Stored) => {
                        proven = Some(candidate);
                        legacy_candidates.clear();
                    }
                    Some(ResponseStateDisposition::Legacy) => legacy_candidates.push(candidate),
                    Some(ResponseStateDisposition::NotStored) => {
                        legacy_candidates.clear();
                    }
                    Some(ResponseStateDisposition::UnmarkedAfterProvenance) | None => {}
                }
            }
            _ if crate::session::is_interrupted_tool_result(event)
                || event_cuts_response_anchor(event) =>
            {
                proven = None;
                legacy_candidates.clear();
            }
            _ => {}
        }
    }
    legacy_candidates.reverse();
    ResponseThreadAnchors {
        proven,
        legacy_candidates,
    }
}

/// Whether `event` renders to a provider message in the prompt view.
///
/// Mirrors the projection in [`crate::session::conversion`]: user and
/// assistant messages and tool results always render; compaction events
/// render only when the prompt view includes them (`include_compactions`,
/// i.e. when a [`ContextEdits`](crate::session::context_edit::ContextEdits)
/// tracker is active); a `RuleInjection` renders exactly when its delivery
/// mode produces conversation content (context-injection and
/// message-delivery rules render a prefixed user message;
/// system-context-append rules deliver through the system prompt and render
/// nothing here); all other metadata events render nothing. The rule case
/// defers to [`DeliveryMode::format_conversation_content`] — the same
/// function `conversion.rs` uses — so the two projections cannot drift.
pub(super) fn event_produces_prompt_message(
    event: &SessionEvent,
    include_compactions: bool,
) -> bool {
    match event {
        SessionEvent::UserMessage { .. }
        | SessionEvent::AssistantMessage { .. }
        | SessionEvent::ToolResult { .. } => true,
        SessionEvent::Compaction { .. } => include_compactions,
        SessionEvent::RuleInjection {
            rule_id,
            delivery,
            content,
            ..
        } => delivery
            .format_conversation_content(rule_id, content)
            .is_some(),
        SessionEvent::ModelChange { .. }
        | SessionEvent::ProviderEpochBoundary { .. }
        | SessionEvent::ChildBranch { .. }
        | SessionEvent::ForkComplete { .. }
        | SessionEvent::Label { .. }
        | SessionEvent::Custom { .. }
        | SessionEvent::ContextMark { .. }
        | SessionEvent::SpokenResponse { .. } => false,
    }
}

/// Mutable provider-state anchor for one agent-loop step.
#[derive(Debug)]
pub(super) struct ConversationRequestState {
    threaded: bool,
    server_compaction: bool,
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
            server_compaction: threaded && capabilities.server_compaction,
            previous_response_id,
            prefix_len,
            input_start,
        })
    }

    /// Whether response storage should be enabled for this request.
    pub(super) const fn store(&self) -> bool {
        self.threaded
    }

    /// Require an opaque credential-and-authority identity before provider
    /// threading can create or reuse response state.
    pub(super) fn require_state_identity(
        &self,
        state_identity: Option<ProviderStateIdentity>,
    ) -> Result<(), ProviderError> {
        if self.threaded && state_identity.is_none() {
            return Err(ProviderError::ProviderStateIdentityRequired);
        }
        Ok(())
    }

    /// Number of leading messages always sent in full (the System prefix),
    /// even when the rest of the request is a threaded delta. Sourced by the
    /// in-flight compaction layout as the count of leading non-event
    /// messages, which — with the managed dynamic-context message now placed
    /// at the tail rather than the prefix — is exactly the System message.
    pub(super) const fn prefix_len(&self) -> usize {
        self.prefix_len
    }

    /// Previous response ID to pass to the provider.
    pub(super) fn previous_response_id(&self) -> Option<String> {
        self.previous_response_id.clone()
    }

    /// Provider-side context management for this request.
    pub(super) fn context_management(
        &self,
        config: &AgentLoopConfig,
    ) -> Option<ProviderContextManagement> {
        if !self.server_compaction {
            return None;
        }

        let compact_threshold_tokens = config.server_compaction_threshold_tokens.or_else(|| {
            config
                .context_window_limit
                .zip(config.auto_compact_reserve_tokens)
                .and_then(|(limit, reserve)| limit.checked_sub(reserve))
                .filter(|threshold| *threshold > 0)
        })?;
        Some(ProviderContextManagement {
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

    /// Build the delta a candidate pre-provenance anchor would authorize.
    pub(super) fn request_messages_for_legacy_anchor(
        &self,
        messages: &[Message],
        anchor: &ResponseThreadAnchor,
    ) -> Vec<Message> {
        if !self.threaded || anchor.input_start == 0 {
            return messages.to_vec();
        }
        let mut request_messages = Vec::new();
        request_messages.extend(messages.iter().take(self.prefix_len).cloned());
        request_messages.extend(messages.iter().skip(anchor.input_start).cloned());
        request_messages
    }

    /// Adopt a validated pre-provenance anchor for this request state.
    pub(super) fn adopt_legacy_anchor(&mut self, anchor: ResponseThreadAnchor) -> bool {
        if !self.threaded {
            return false;
        }
        self.previous_response_id = Some(anchor.response_id);
        self.input_start = anchor.input_start;
        true
    }

    /// Build a request whose current managed context uses the Responses
    /// `instructions` replacement surface.
    ///
    /// A trailing System message is extracted into top-level `instructions`
    /// by the Responses serializer. Unlike ordinary input items, prior
    /// instructions are not inherited through `previous_response_id`, so the
    /// current dynamic context replaces rather than accumulates. The live
    /// message list remains event-aligned and unmodified.
    pub(super) fn request_messages_with_managed_instructions(
        &self,
        messages: &[Message],
        managed_context: Option<String>,
    ) -> Vec<Message> {
        let mut request_messages = self.request_messages(messages);
        if self.threaded
            && let Some(content) = managed_context
        {
            request_messages.push(Message {
                response_items: Vec::new(),
                role: crate::provider::request::MessageRole::System,
                content: Some(content),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            });
        }
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

    /// Adjust the input cursor after a message is removed before it.
    ///
    /// Both callers (managed-message detach, in-flight compaction) only ever
    /// remove at or past `prefix_len` — the System-only prefix is immutable —
    /// so the prefix itself never needs adjusting.
    pub(super) fn note_removed_message(&mut self, index: usize) {
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
mod backend_security_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
    use crate::provider::openai::OpenAiProvider;
    use crate::provider::request::ProviderConfig;
    use crate::provider::traits::Provider;

    #[test]
    fn auto_state_is_stateless_for_an_explicit_canonical_codex_backend() -> Result<(), ProviderError>
    {
        let config = ProviderConfig {
            auth_source: AuthSource::OAuth { auth_root: None },
            base_url: Some("https://chatgpt.com:443/backend-api/codex/".to_owned()),
            timeout: Duration::from_secs(5),
            max_retries: 0,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: None,
            retry_backoff: None,
            retry_after_ceiling: None,
        };
        let auth_provider: Arc<dyn AuthProvider> =
            Arc::new(MockAuthProvider::single("oauth-token"));
        let provider = OpenAiProvider::with_auth_provider(config, auth_provider)?;
        let loop_config = AgentLoopConfig::default();
        let state = ConversationRequestState::new(&loop_config, provider.capabilities(), 1, None)?;

        assert!(!state.store());
        assert!(state.previous_response_id().is_none());
        assert!(state.context_management(&loop_config).is_none());
        Ok(())
    }
}

#[cfg(test)]
mod projection_tests;
#[cfg(test)]
mod request_state_tests;
#[cfg(test)]
mod test_support;
