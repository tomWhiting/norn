//! Mutable provider-thread request state and stable-seed projection.

use crate::error::ProviderError;
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode};
use crate::provider::ProviderStateIdentity;
use crate::provider::request::{Message, MessageRole, ProviderContextManagement, ToolCallCaller};
use crate::provider::tools::ProviderCapabilities;
use crate::system_prompt::{ManagedContextProjection, PromptSeedFingerprint};

use super::ResponseThreadAnchor;

/// Mutable provider-state anchor for one agent-loop step.
#[derive(Debug)]
pub(crate) struct ConversationRequestState {
    threaded: bool,
    server_compaction: bool,
    previous_response_id: Option<String>,
    anchor_prompt_seed: Option<PromptSeedFingerprint>,
    prompt_seed: PromptSeedFingerprint,
    prefix_len: usize,
    input_start: usize,
}

impl ConversationRequestState {
    /// Validate provider-state configuration and identity before request setup
    /// performs any fallible dynamic prompt work.
    pub(crate) fn validate_setup(
        config: &AgentLoopConfig,
        capabilities: ProviderCapabilities,
        state_identity: Option<ProviderStateIdentity>,
    ) -> Result<bool, ProviderError> {
        let threaded = resolve_threaded_mode(config, capabilities)?;
        if threaded && state_identity.is_none() {
            return Err(ProviderError::ProviderStateIdentityRequired);
        }
        Ok(threaded)
    }

    /// Compatibility constructor for a prompt with no typed non-System seed.
    #[cfg(test)]
    pub(crate) fn new(
        config: &AgentLoopConfig,
        capabilities: ProviderCapabilities,
        prefix_len: usize,
        thread_anchor: Option<ResponseThreadAnchor>,
    ) -> Result<Self, ProviderError> {
        Self::with_prompt_seed(
            config,
            capabilities,
            prefix_len,
            PromptSeedFingerprint::empty(),
            thread_anchor,
        )
    }

    /// Create request state bound to the current stable prompt seed.
    pub(crate) fn with_prompt_seed(
        config: &AgentLoopConfig,
        capabilities: ProviderCapabilities,
        prefix_len: usize,
        prompt_seed: PromptSeedFingerprint,
        thread_anchor: Option<ResponseThreadAnchor>,
    ) -> Result<Self, ProviderError> {
        let threaded = resolve_threaded_mode(config, capabilities)?;
        let anchor = threaded
            .then_some(thread_anchor)
            .flatten()
            .filter(|anchor| {
                anchor
                    .prompt_seed_fingerprint
                    .is_none_or(|bound| bound == prompt_seed)
            });
        let (previous_response_id, anchor_prompt_seed, input_start) = anchor.map_or_else(
            || (None, None, 0),
            |anchor| {
                (
                    Some(anchor.response_id),
                    anchor.prompt_seed_fingerprint,
                    anchor.input_start,
                )
            },
        );
        Ok(Self {
            threaded,
            server_compaction: threaded && capabilities.server_compaction,
            previous_response_id,
            anchor_prompt_seed,
            prompt_seed,
            prefix_len,
            input_start,
        })
    }

    /// Whether response storage should be enabled for this request.
    pub(crate) const fn store(&self) -> bool {
        self.threaded
    }

    /// Require an opaque credential-and-authority identity before provider
    /// threading can create or reuse response state.
    pub(crate) fn require_state_identity(
        &self,
        state_identity: Option<ProviderStateIdentity>,
    ) -> Result<(), ProviderError> {
        if self.threaded && state_identity.is_none() {
            return Err(ProviderError::ProviderStateIdentityRequired);
        }
        Ok(())
    }

    /// Number of leading stable prompt messages before persisted history.
    pub(crate) const fn prefix_len(&self) -> usize {
        self.prefix_len
    }

    /// Prompt seed captured for the request being assembled.
    pub(crate) const fn prompt_seed_fingerprint(&self) -> PromptSeedFingerprint {
        self.prompt_seed
    }

    /// Previous response ID to pass to the provider.
    pub(crate) fn previous_response_id(&self) -> Option<String> {
        self.previous_response_id.clone()
    }

    /// Replace the stable prefix and reconcile its anchor binding.
    ///
    /// A changed V2 seed cuts the anchor and forces full replay. A readable
    /// V1/pre-D8 anchor remains usable for one bootstrap request, which sends
    /// the full stable Developer/User seed and upgrades the next provenance
    /// record to V2. System-only changes preserve the anchor because they do
    /// not affect `prompt_seed`.
    pub(crate) fn sync_stable_prefix(
        &mut self,
        messages: &mut Vec<Message>,
        stable_prefix: Vec<Message>,
        prompt_seed: PromptSeedFingerprint,
    ) {
        let new_prefix_len = stable_prefix.len();
        messages.splice(..self.prefix_len, stable_prefix);
        if self.input_start != 0 {
            self.input_start = if new_prefix_len >= self.prefix_len {
                self.input_start
                    .saturating_add(new_prefix_len.saturating_sub(self.prefix_len))
            } else {
                self.input_start
                    .saturating_sub(self.prefix_len.saturating_sub(new_prefix_len))
            };
        }
        self.prefix_len = new_prefix_len;
        self.prompt_seed = prompt_seed;
        if self
            .anchor_prompt_seed
            .is_some_and(|bound| bound != prompt_seed)
        {
            self.previous_response_id = None;
            self.anchor_prompt_seed = None;
            self.input_start = 0;
        }
    }

    /// Provider-side context management for this request.
    pub(crate) fn context_management(
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
    pub(crate) fn request_messages(&self, messages: &[Message]) -> Vec<Message> {
        if !self.threaded || self.previous_response_id.is_none() || self.input_start == 0 {
            return messages.to_vec();
        }

        let mut request_messages = Vec::new();
        let stable_prefix = messages.iter().take(self.prefix_len);
        if self.anchor_prompt_seed.is_some() {
            request_messages.extend(
                stable_prefix
                    .filter(|message| message.role == MessageRole::System)
                    .cloned(),
            );
        } else {
            request_messages.extend(stable_prefix.cloned());
        }
        request_messages.extend(messages.iter().skip(self.input_start).cloned());
        request_messages
    }

    /// Build the bootstrap delta a pre-provenance anchor would authorize.
    pub(crate) fn request_messages_for_legacy_anchor(
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

    /// Adopt a validated pre-provenance anchor for one bootstrap request.
    pub(crate) fn adopt_legacy_anchor(&mut self, anchor: ResponseThreadAnchor) -> bool {
        if !self.threaded || anchor.prompt_seed_fingerprint.is_some() {
            return false;
        }
        self.previous_response_id = Some(anchor.response_id);
        self.anchor_prompt_seed = None;
        self.input_start = anchor.input_start;
        true
    }

    /// Build a request whose Norn-owned managed policy uses Responses
    /// `instructions`.
    pub(crate) fn request_messages_with_managed_instructions(
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
                role: ManagedContextProjection::ThreadedResponsesInstructions.role(),
                content: Some(content),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: ToolCallCaller::Absent,
            });
        }
        request_messages
    }

    /// Update the anchor after a provider response.
    pub(crate) fn observe_response(&mut self, response_id: Option<&str>, next_input_start: usize) {
        if !self.threaded {
            return;
        }
        if let Some(response_id) = response_id.filter(|id| !id.is_empty()) {
            self.previous_response_id = Some(response_id.to_owned());
            self.anchor_prompt_seed = Some(self.prompt_seed);
            self.input_start = next_input_start;
        } else {
            self.previous_response_id = None;
            self.anchor_prompt_seed = None;
            self.input_start = 0;
        }
    }

    /// Adjust the input cursor after a message is removed before it.
    pub(crate) fn note_removed_message(&mut self, index: usize) {
        if index < self.input_start {
            self.input_start = self.input_start.saturating_sub(1);
        }
    }
}

fn resolve_threaded_mode(
    config: &AgentLoopConfig,
    capabilities: ProviderCapabilities,
) -> Result<bool, ProviderError> {
    validate_provider_state_config(config, capabilities)?;
    Ok(match config.conversation_state {
        ConversationStateMode::Auto => capabilities.response_threading,
        ConversationStateMode::ManualReplay => false,
        ConversationStateMode::ProviderThreaded => true,
    })
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
