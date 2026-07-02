//! Client-side token estimation for context-window planning (N-023 R3).
//!
//! Estimation runs immediately before each provider call so the loop can
//! emit a `loop.token_warning` custom session event before the request
//! goes over the wire. The estimator is advisory — the call still
//! proceeds — and an estimate is intentionally cheap (chars/4 for the
//! built-in implementation) so it can be evaluated every iteration
//! without measurable overhead.

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
