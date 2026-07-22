//! Provider conversation-state request shaping.

use crate::error::ProviderError;
use crate::provider::request::Message;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;
use crate::session::{
    ActiveResponseProvenance, ResponseStateDisposition, discover_active_response_provenance,
    event_cuts_response_anchor,
};

mod request_state;
pub(super) use request_state::ConversationRequestState;

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
    prompt_seed_fingerprint: Option<crate::system_prompt::PromptSeedFingerprint>,
}

impl ResponseThreadAnchor {
    pub(super) fn note_stable_prefix_insertion(&mut self) {
        self.input_start = self.input_start.saturating_add(1);
    }

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
            prompt_seed_fingerprint: None,
        }
    }

    pub(super) fn for_test_with_prompt_seed(
        response_id: String,
        input_start: usize,
        prompt_seed_fingerprint: crate::system_prompt::PromptSeedFingerprint,
    ) -> Self {
        Self {
            response_id,
            input_start,
            prompt_seed_fingerprint: Some(prompt_seed_fingerprint),
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
    let mut legacy_system_append_requires_replay = false;
    for event in events {
        if matches!(
            event,
            SessionEvent::RuleInjection {
                origin: None,
                delivery: crate::rules::types::DeliveryMode::SystemContextAppend,
                ..
            }
        ) {
            // Before D8 this row was resent as request-local System context and
            // was not a provider input message. Its conservative User
            // projection therefore cannot sit behind an unbound old anchor:
            // doing so would skip the local message while Responses also does
            // not inherit the old top-level instructions. One full replay
            // publishes a seed-bound V2 anchor under the current projection.
            legacy_system_append_requires_replay = true;
        }
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
                    prompt_seed_fingerprint: provenance.prompt_seed_fingerprint(&event.base().id),
                };
                let current_projection_is_bound = candidate.prompt_seed_fingerprint.is_some()
                    || !legacy_system_append_requires_replay;
                match provenance.disposition(&event.base().id) {
                    Some(ResponseStateDisposition::Stored) if current_projection_is_bound => {
                        proven = Some(candidate);
                        legacy_candidates.clear();
                    }
                    Some(ResponseStateDisposition::Legacy) if current_projection_is_bound => {
                        legacy_candidates.push(candidate);
                    }
                    Some(ResponseStateDisposition::Stored | ResponseStateDisposition::Legacy) => {
                        proven = None;
                        legacy_candidates.clear();
                    }
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
/// tracker is active); every `RuleInjection` renders once, with source-derived
/// authority for new rows and conservative User authority for origin-less
/// pre-D8 rows. All other metadata events render nothing.
pub(super) fn event_produces_prompt_message(
    event: &SessionEvent,
    include_compactions: bool,
) -> bool {
    match event {
        SessionEvent::UserMessage { .. }
        | SessionEvent::AssistantMessage { .. }
        | SessionEvent::ToolResult { .. }
        | SessionEvent::RuleInjection { .. } => true,
        SessionEvent::Compaction { .. } => include_compactions,
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

#[cfg(test)]
mod backend_security_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::r#loop::config::AgentLoopConfig;
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
mod seed_binding_tests;
#[cfg(test)]
mod test_support;
