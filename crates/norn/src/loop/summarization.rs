//! LLM-written compaction summaries (fix campaign Track L, finding 1).
//!
//! When auto-compaction fires, the loop asks the step's own provider and
//! model to write a semantic summary of the events about to be elided, so
//! the model retains continuity (objectives, decisions, tool outcomes,
//! open work) instead of only the mechanical event digest. The request is
//! a plain, untooled, unthreaded completion: the elided span is rendered
//! to a labelled transcript and sent alongside fixed summarization
//! instructions.
//!
//! Failure policy lives in the caller ([`super::compaction`]): a failed or
//! unusable summarization response is logged and the mechanical digest is
//! committed instead, explicitly marked as a non-semantic fallback.

use crate::error::NornError;
use crate::r#loop::classify::call_provider;
use crate::provider::events::StopReason;
use crate::provider::request::{Message, MessageRole, ProviderRequest};
use crate::provider::traits::Provider;
use crate::provider::usage::Usage;
use crate::session::conversion::prompt_events_to_messages;
use crate::session::events::SessionEvent;

/// Instructions sent as the system message of every summarization request.
const SUMMARIZATION_SYSTEM_PROMPT: &str = "You write compaction summaries for an \
agent conversation. The transcript you receive is the OLDER portion of an ongoing \
conversation; it is about to be removed from the agent's context and replaced by \
your summary. Write a factual, specific summary that preserves everything a \
successor needs to continue seamlessly: the user's objectives and constraints, \
decisions made and their reasons, key facts and values discovered, tools that \
were run and what they returned or changed, errors encountered and how they were \
resolved, and any unfinished work or open questions. Use concrete names, paths, \
identifiers, and numbers from the transcript. Do not add commentary about the \
summarization task itself; output only the summary.";

/// Instruction appended after the transcript in the user message.
const SUMMARIZATION_USER_SUFFIX: &str = "Summarize the conversation transcript \
above. The summary will replace the transcript in the agent's context.";

/// The assembled result of a summarization completion, before the caller
/// has judged whether it is usable.
#[derive(Debug)]
pub(super) struct SummarizationResponse {
    /// Full text the model produced (may be empty).
    pub(super) text: String,
    /// Token usage of the summarization call. Accounted by the caller
    /// even when the response is rejected — the tokens were spent.
    pub(super) usage: Usage,
    /// How the model stopped; anything other than
    /// [`StopReason::EndTurn`] means the summary is incomplete.
    pub(super) stop_reason: StopReason,
}

impl SummarizationResponse {
    /// A summary is usable when the model finished its turn and produced
    /// non-whitespace text. Truncated (`MaxTokens`/`ContentFilter`) or
    /// empty responses must not silently replace conversation history.
    pub(super) fn usable_summary(&self) -> Option<&str> {
        let trimmed = self.text.trim();
        if trimmed.is_empty() || self.stop_reason != StopReason::EndTurn {
            return None;
        }
        Some(trimmed)
    }
}

/// Ask `provider`/`model` to summarize the events about to be elided.
///
/// The request is deliberately isolated from the step's conversation
/// shaping: no tools, no response threading (`previous_response_id` unset,
/// `store` false), no cache key, and no reasoning overrides — every knob
/// defers to the provider's own defaults.
///
/// # Errors
///
/// Propagates the [`NornError`] from the provider call; the caller maps
/// any error to the mechanical-digest fallback rather than aborting the
/// agent step.
pub(super) async fn request_compaction_summary(
    provider: &dyn Provider,
    model: &str,
    elided: &[SessionEvent],
) -> Result<SummarizationResponse, NornError> {
    let transcript = render_transcript(elided);
    let request = ProviderRequest {
        messages: vec![
            text_message(MessageRole::System, SUMMARIZATION_SYSTEM_PROMPT.to_string()),
            text_message(
                MessageRole::User,
                format!("{transcript}\n\n{SUMMARIZATION_USER_SUFFIX}"),
            ),
        ],
        tools: Vec::new(),
        model: model.to_string(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    };
    // event_tx is deliberately None: streaming summarization deltas to
    // observers would be indistinguishable from assistant output.
    let response = call_provider(provider, request, None).await?;
    Ok(SummarizationResponse {
        text: response.text,
        usage: response.usage,
        stop_reason: response.stop_reason,
    })
}

/// Render the elided events to a labelled plain-text transcript.
///
/// Uses the same event-to-message projection as prompt construction
/// ([`prompt_events_to_messages`]) so tool-call arguments, tool results,
/// and prior compaction summaries appear exactly as the model originally
/// saw them, then flattens each message to a role-labelled block.
pub(super) fn render_transcript(elided: &[SessionEvent]) -> String {
    let messages = prompt_events_to_messages(elided);
    let mut transcript = String::new();
    for message in &messages {
        if !transcript.is_empty() {
            transcript.push_str("\n\n");
        }
        transcript.push_str(&render_message(message));
    }
    transcript
}

fn render_message(message: &Message) -> String {
    let label = match message.role {
        MessageRole::System => "System",
        MessageRole::Developer => "Context note",
        MessageRole::User => "User",
        MessageRole::Assistant => "Assistant",
        MessageRole::ToolResult => "Tool result",
    };
    let mut block = String::from(label);
    if let (MessageRole::ToolResult, Some(name)) = (&message.role, message.tool_name.as_deref()) {
        block.push_str(" (");
        block.push_str(name);
        block.push(')');
    }
    block.push(':');
    if let Some(content) = message.content.as_deref()
        && !content.is_empty()
    {
        block.push('\n');
        block.push_str(content);
    }
    for call in &message.tool_calls {
        block.push_str("\n[tool call] ");
        block.push_str(&call.name);
        block.push('(');
        block.push_str(&call.arguments);
        block.push(')');
    }
    block
}

fn text_message(role: MessageRole, content: String) -> Message {
    Message {
        role,
        content: Some(content),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provider::events::ProviderEvent;
    use crate::provider::mock::MockProvider;
    use crate::session::events::{EventBase, EventUsage, ToolCallEvent};

    fn user_event(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    fn assistant_with_call(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "/tmp/a"}),
                kind: crate::provider::request::ToolCallKind::Function,
            }],
            usage: EventUsage::default(),
            stop_reason: "tool_use".to_string(),
            response_id: None,
        }
    }

    fn tool_result_event() -> SessionEvent {
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "call_1".to_string(),
            tool_name: "read".to_string(),
            output: serde_json::json!({"content": "file body"}),
            spool_ref: None,
            duration_ms: 3,
        }
    }

    #[test]
    fn transcript_labels_roles_tool_calls_and_results() {
        let events = vec![
            user_event("please read /tmp/a"),
            assistant_with_call("reading now"),
            tool_result_event(),
        ];
        let transcript = render_transcript(&events);
        assert!(
            transcript.contains("User:\nplease read /tmp/a"),
            "{transcript}"
        );
        assert!(
            transcript.contains("Assistant:\nreading now"),
            "{transcript}"
        );
        assert!(transcript.contains("[tool call] read("), "{transcript}");
        assert!(transcript.contains("Tool result (read):"), "{transcript}");
        assert!(transcript.contains("file body"), "{transcript}");
    }

    #[test]
    fn transcript_renders_prior_compaction_summaries() {
        let events = vec![SessionEvent::Compaction {
            base: EventBase::new(None),
            summary: "earlier summary text".to_string(),
            replaced_event_ids: Vec::new(),
        }];
        let transcript = render_transcript(&events);
        assert!(transcript.contains("earlier summary text"), "{transcript}");
    }

    #[tokio::test]
    async fn request_is_untooled_unthreaded_and_uses_step_model() {
        let provider = MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "a fine summary".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 42,
                    output_tokens: 7,
                    ..Usage::default()
                },
                response_id: None,
            },
        ]]);
        let events = vec![user_event("hello")];

        let response = request_compaction_summary(&provider, "step-model", &events)
            .await
            .expect("summarization call succeeds");

        assert_eq!(response.usable_summary(), Some("a fine summary"));
        assert_eq!(response.usage.input_tokens, 42);
        assert_eq!(response.usage.output_tokens, 7);

        let requests = provider.requests().expect("requests recorded");
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.model, "step-model");
        assert!(request.tools.is_empty(), "summarization must be untooled");
        assert!(request.previous_response_id.is_none());
        assert!(!request.store);
        assert!(request.cache_key.is_none());
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, MessageRole::System);
        assert!(
            request.messages[1]
                .content
                .as_deref()
                .is_some_and(|c| c.contains("hello")),
            "transcript must be embedded in the user message",
        );
    }

    #[test]
    fn truncated_or_empty_summaries_are_unusable() {
        let truncated = SummarizationResponse {
            text: "cut off mid".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::MaxTokens,
        };
        assert!(truncated.usable_summary().is_none());

        let empty = SummarizationResponse {
            text: "   \n".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        };
        assert!(empty.usable_summary().is_none());
    }
}
