//! Client-side token estimation for context-window planning (N-023 R3).
//!
//! Estimation runs immediately before each provider call so the loop can
//! emit a `loop.token_warning` custom session event before the request
//! goes over the wire. The estimator is advisory — the call still
//! proceeds — and an estimate is intentionally cheap (chars/4 for the
//! built-in implementation) so it can be evaluated every iteration
//! without measurable overhead.

use crate::provider::reasoning::{ReasoningContentPart, ReasoningItem, ReasoningSummaryPart};
use crate::provider::request::{Message, ToolDefinition};

/// Trait implemented by anything that can produce a token-count
/// approximation for a piece of text.
///
/// Object-safe: `&self`, no generic parameters, no async. Implementations
/// must be `Send + Sync` so they can sit behind an `Arc<dyn TokenEstimator>`
/// shared across agents.
pub trait TokenEstimator: Send + Sync {
    /// Return the estimated token count for `text`.
    fn estimate(&self, text: &str) -> usize;
}

/// Cheap estimator that returns `text.chars().count() / 4`.
///
/// Matches the rough heuristic used by tiktoken-style approximations for
/// English prose without pulling in a dependency. Sufficient for context-
/// window planning where the model itself is the source of truth and the
/// estimator only needs to spot order-of-magnitude overshoots.
#[derive(Clone, Copy, Debug, Default)]
pub struct SimpleTokenEstimator;

impl TokenEstimator for SimpleTokenEstimator {
    fn estimate(&self, text: &str) -> usize {
        text.chars().count() / 4
    }
}

/// Sum the estimated tokens across every message and tool definition in a
/// prepared request.
///
/// `messages` covers content, tool-call arguments, and recorded tool
/// results. `tools` adds the descriptions and serialised parameter
/// schemas the provider includes alongside the request.
#[must_use]
pub fn estimate_prompt_tokens(
    estimator: &dyn TokenEstimator,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> usize {
    let mut total = messages.iter().map(|m| message_tokens(estimator, m)).sum();
    total += tools
        .iter()
        .map(|t| tool_tokens(estimator, t))
        .sum::<usize>();
    total
}

/// Estimate the tokens a single message contributes to a request.
///
/// Counts message content, tool-call names and arguments, the tool result
/// name — and the structured [`reasoning`](Message::reasoning) items. The
/// reasoning items are re-billed on every request on stateless-replay
/// backends (their `encrypted_content` blob is echoed ahead of the
/// assistant turn on `response_threading: false` providers), and they are
/// persisted and replayed on resume, so a reasoning-blind estimate
/// under-counts a resumed prompt by exactly the reasoning it can no longer
/// see — the shape of the original `ContextWindowExceeded` incident.
///
/// The estimate over a reasoning item is deliberately conservative: it runs
/// `chars/4` over the raw `encrypted_content` base64 (plus every summary and
/// content part), which over-counts the true reasoning-token cost of the
/// blob. Over-estimation is the safe direction for a compaction trigger — it
/// fires slightly early rather than sailing past the window — so the bias is
/// accepted.
///
/// [`Message::thinking`] is **not** counted. It is display/observability
/// state that no request serializer reads (see [`Message`] docs: replay goes
/// through the structured reasoning items, which carry the
/// `encrypted_content` the plain-text thinking string does not). Counting it
/// would over-estimate every live turn by content that never reaches the
/// wire.
fn message_tokens(estimator: &dyn TokenEstimator, message: &Message) -> usize {
    let mut total = message
        .content
        .as_deref()
        .map_or(0, |c| estimator.estimate(c));
    for call in &message.tool_calls {
        total = total.saturating_add(estimator.estimate(&call.name));
        total = total.saturating_add(estimator.estimate(&call.arguments));
    }
    if let Some(name) = message.tool_name.as_deref() {
        total = total.saturating_add(estimator.estimate(name));
    }
    for item in &message.reasoning {
        total = total.saturating_add(reasoning_tokens(estimator, item));
    }
    total
}

/// Estimate the tokens a single reasoning item re-bills on the next request.
///
/// Sums the `encrypted_content` blob (when present — the field replayed to
/// stateless backends), every summary part, and every content part. See
/// [`message_tokens`] for why the estimate over the encrypted blob is a
/// deliberate over-count.
fn reasoning_tokens(estimator: &dyn TokenEstimator, item: &ReasoningItem) -> usize {
    let mut total = item
        .encrypted_content
        .as_deref()
        .map_or(0, |blob| estimator.estimate(blob));
    for part in &item.summary {
        let ReasoningSummaryPart::SummaryText { text } = part;
        total = total.saturating_add(estimator.estimate(text));
    }
    if let Some(parts) = item.content.as_ref() {
        for part in parts {
            let text = match part {
                ReasoningContentPart::ReasoningText { text }
                | ReasoningContentPart::Text { text } => text,
            };
            total = total.saturating_add(estimator.estimate(text));
        }
    }
    total
}

fn tool_tokens(estimator: &dyn TokenEstimator, tool: &ToolDefinition) -> usize {
    let params = tool.parameters.to_string();
    estimator
        .estimate(&tool.description)
        .saturating_add(estimator.estimate(&tool.name))
        .saturating_add(estimator.estimate(&params))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn
)]
mod tests {
    use super::*;
    use crate::provider::request::{AssistantToolCall, MessageRole};

    #[test]
    fn simple_estimator_chars_div_four() {
        let est = SimpleTokenEstimator;
        assert_eq!(est.estimate(""), 0);
        assert_eq!(est.estimate("abcd"), 1);
        assert_eq!(est.estimate("abcdefgh"), 2);
        assert_eq!(est.estimate("hello world"), 11 / 4);
    }

    #[test]
    fn simple_estimator_handles_unicode() {
        let est = SimpleTokenEstimator;
        // 4 grapheme-ish chars but each is multibyte; chars().count() == 4.
        assert_eq!(est.estimate("café"), 1);
    }

    #[test]
    fn estimate_prompt_tokens_sums_messages_and_tools() {
        let est = SimpleTokenEstimator;
        let messages = vec![
            Message {
                reasoning: Vec::new(),
                role: MessageRole::System,
                content: Some("a".repeat(40)),
                thinking: String::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            },
            Message {
                reasoning: Vec::new(),
                role: MessageRole::User,
                content: Some("b".repeat(20)),
                thinking: String::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            },
        ];
        let tools = vec![ToolDefinition {
            name: "read".to_string(),
            description: "x".repeat(40),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let total = estimate_prompt_tokens(&est, &messages, &tools);
        // 40/4 + 20/4 + 4/4 + 1 (name "read") + chars/4 of params json.
        assert!(total >= 10 + 5 + 10);
    }

    /// A resumed assistant message replays its persisted reasoning items;
    /// the estimate must count them, so it is strictly higher than the same
    /// message with the reasoning stripped. A reasoning-blind estimate
    /// under-counts a resumed prompt by exactly the reasoning it cannot see —
    /// the `ContextWindowExceeded` incident shape.
    #[test]
    fn estimate_counts_reasoning_items() {
        use crate::provider::reasoning::{
            ReasoningContentPart, ReasoningItem, ReasoningSummaryPart,
        };

        let est = SimpleTokenEstimator;
        let reasoning = vec![ReasoningItem {
            id: "rs_1".to_string(),
            summary: vec![ReasoningSummaryPart::SummaryText {
                text: "s".repeat(40),
            }],
            content: Some(vec![ReasoningContentPart::ReasoningText {
                text: "c".repeat(40),
            }]),
            encrypted_content: Some("e".repeat(400)),
        }];
        let with_reasoning = Message {
            reasoning,
            role: MessageRole::Assistant,
            content: Some("answer".to_string()),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        };
        let without_reasoning = Message {
            reasoning: Vec::new(),
            ..with_reasoning.clone()
        };

        let with = estimate_prompt_tokens(&est, std::slice::from_ref(&with_reasoning), &[]);
        let without = estimate_prompt_tokens(&est, std::slice::from_ref(&without_reasoning), &[]);
        assert!(
            with > without,
            "reasoning items must raise the estimate ({with} !> {without})",
        );
        // encrypted (400/4=100) + summary (40/4=10) + content (40/4=10) = 120.
        assert_eq!(with - without, 120, "the reasoning delta must be counted");
    }

    /// `thinking` is display-only — no request serializer reads it — so the
    /// estimate must NOT count it. Counting it would over-estimate every
    /// live turn by content that never reaches the wire.
    #[test]
    fn estimate_ignores_thinking() {
        let est = SimpleTokenEstimator;
        let base = Message {
            reasoning: Vec::new(),
            role: MessageRole::Assistant,
            content: Some("answer".to_string()),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        };
        let with_thinking = Message {
            thinking: "t".repeat(400),
            ..base.clone()
        };
        assert_eq!(
            estimate_prompt_tokens(&est, std::slice::from_ref(&with_thinking), &[]),
            estimate_prompt_tokens(&est, std::slice::from_ref(&base), &[]),
            "thinking is display-only and must not enter the estimate",
        );
    }

    #[test]
    fn estimate_includes_tool_call_arguments() {
        let est = SimpleTokenEstimator;
        let msg = Message {
            reasoning: Vec::new(),
            role: MessageRole::Assistant,
            content: Some(String::new()),
            thinking: String::new(),
            tool_calls: vec![AssistantToolCall {
                call_id: "call_tc1".to_string(),
                name: "search".to_string(),
                arguments: "x".repeat(40),
                kind: crate::provider::request::ToolCallKind::Function,
            }],
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        };
        let n = estimate_prompt_tokens(&est, std::slice::from_ref(&msg), &[]);
        assert!(n >= 10);
    }
}
