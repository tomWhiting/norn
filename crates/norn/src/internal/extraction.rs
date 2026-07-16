//! Focused answer extraction agent for web page content.

use std::any::Any;
use std::sync::Arc;

use futures_util::StreamExt;

use crate::error::{ProviderError, ToolError};
use crate::provider::events::ProviderEvent;
use crate::provider::request::{Message, MessageRole, ProviderRequest};
use crate::provider::traits::Provider;

const EXTRACTION_MODEL: &str = "gpt-5.4";
const MAX_CONTENT_CHARS: usize = 400_000;
const TRUNCATION_NOTE: &str =
    "\n\n[Note: web page content was truncated to 400,000 characters before extraction.]";

/// Shared provider handle published into [`ToolContext`](crate::tool::context::ToolContext)
/// extensions for internal agents.
#[derive(Clone)]
pub struct SharedProvider(pub Arc<dyn Provider>);

impl SharedProvider {
    /// Returns the wrapped provider as an `Any` reference.
    ///
    /// This type is `Send + Sync + 'static`, so it satisfies the extension-map
    /// bounds (`Any` is implemented automatically for all `'static` types).
    #[must_use]
    pub fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Desired amount of detail in extraction answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailLevel {
    /// Short direct answers.
    Brief,
    /// Clear answers with relevant detail.
    Normal,
    /// Comprehensive answers with context and explanation.
    Detailed,
}

/// Extract focused answers from page `content` for `questions` using the shared provider.
///
/// Returns a JSON array of answer objects, each with `question`, `answer`,
/// and `lines` fields.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] when the provider call fails, the
/// stream reports an error, or no text content is produced.
pub async fn extract(
    provider: &dyn Provider,
    content: &str,
    questions: &[String],
    detail: DetailLevel,
) -> Result<serde_json::Value, ToolError> {
    let request = ProviderRequest {
        messages: vec![
            Message {
                response_items: Vec::new(),
                role: MessageRole::System,
                content: Some(system_prompt(detail).to_owned()),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            },
            Message {
                response_items: Vec::new(),
                role: MessageRole::User,
                content: Some(build_user_message(content, questions)),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            },
        ],
        tools: Vec::new(),
        model: EXTRACTION_MODEL.to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    };

    let mut stream = provider
        .stream(request)
        .map_err(|error| provider_error(&error))?;
    let mut text = String::new();

    while let Some(result) = stream.next().await {
        match result.map_err(|error| provider_error(&error))? {
            ProviderEvent::TextDelta { text: chunk } => text.push_str(&chunk),
            ProviderEvent::Error { error } => return Err(provider_error(&error)),
            _ => {}
        }
    }

    if text.is_empty() {
        return Err(ToolError::ExecutionFailed {
            reason: "extraction model returned no content".to_owned(),
        });
    }

    let trimmed = text.trim();
    let json_str = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map_or(trimmed, |s| s.strip_suffix("```").unwrap_or(s));

    serde_json::from_str(json_str.trim()).map_err(|e| ToolError::ExecutionFailed {
        reason: format!("extraction model returned invalid JSON: {e}\nRaw: {text}"),
    })
}

fn system_prompt(detail: DetailLevel) -> &'static str {
    match detail {
        DetailLevel::Brief => {
            "You are an extraction agent. Return a JSON array of answer objects. The content has line numbers prepended to each line. For each question, return {\"question\": <number>, \"answer\": \"...\", \"lines\": \"...\"}. Your answers will be read by someone who has not seen the source page. Extract actual facts, names, definitions, values, signatures, and explanations — not links to where information might be found. A link is not an answer; the information the link points to is the answer. Provide enough context that the reader understands what something is and why it matters. Cite the line numbers where you found the information in the lines field (e.g. \"42-58, 103-107\"). If the content does not contain enough information to answer a question, say so in the answer field. Use only information present in the provided content. Do not fabricate. Keep answers concise but complete enough to be useful without visiting the source page. Return ONLY the JSON array, no other text."
        }
        DetailLevel::Normal => {
            "You are an extraction agent. Return a JSON array of answer objects. The content has line numbers prepended to each line. For each question, return {\"question\": <number>, \"answer\": \"...\", \"lines\": \"...\"}. Your answers will be read by someone who has not seen the source page. Extract actual facts, names, definitions, values, signatures, and explanations — not links to where information might be found. A link is not an answer; the information the link points to is the answer. Provide enough context that the reader understands what something is, how it relates to other concepts, and why it matters. Include relevant details, examples, and context that help the reader build a mental model. Cite the line numbers where you found the information in the lines field (e.g. \"42-58, 103-107\"). If the content does not contain enough information to answer a question, say so in the answer field. Use only information present in the provided content. Do not fabricate. Quote function signatures, type names, configuration values, and version numbers when they appear. Return ONLY the JSON array, no other text."
        }
        DetailLevel::Detailed => {
            "You are an extraction agent. Return a JSON array of answer objects. The content has line numbers prepended to each line. For each question, return {\"question\": <number>, \"answer\": \"...\", \"lines\": \"...\"}. Your answers will be read by someone who has not seen the source page. Extract actual facts, names, definitions, values, signatures, and explanations — not links to where information might be found. A link is not an answer; the information the link points to is the answer. Provide enough context that the reader understands what something is, how it relates to other concepts, why it matters, and what the implications are. Be thorough: include all relevant details, relationships, examples, edge cases, and caveats. Explain design choices when the content describes them. Cite the line numbers where you found the information in the lines field (e.g. \"42-58, 103-107\"). If the content does not contain enough information to answer a question, say so in the answer field. Use only information present in the provided content. Do not fabricate. Quote function signatures, type names, configuration values, version numbers, and code examples. Return ONLY the JSON array, no other text."
        }
    }
}

fn build_user_message(content: &str, questions: &[String]) -> String {
    let (capped_content, was_truncated) = cap_content(content);
    let truncation_note = if was_truncated { TRUNCATION_NOTE } else { "" };

    format!(
        "<page_content>\n{capped_content}{truncation_note}\n</page_content>\n\n<questions>\n{}\n</questions>",
        numbered_questions(questions)
    )
}

fn cap_content(content: &str) -> (&str, bool) {
    let mut char_indices = content.char_indices();
    let Some((byte_idx, _)) = char_indices.nth(MAX_CONTENT_CHARS) else {
        return (content, false);
    };
    (&content[..byte_idx], true)
}

fn numbered_questions(questions: &[String]) -> String {
    if questions.is_empty() {
        return "1. Summarize the key information from the provided content.".to_owned();
    }

    questions
        .iter()
        .enumerate()
        .map(|(index, question)| format!("{}. {}", index + 1, question))
        .collect::<Vec<_>>()
        .join("\n")
}

fn provider_error(error: &ProviderError) -> ToolError {
    ToolError::ExecutionFailed {
        reason: format!("extraction provider call failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_content_at_char_boundary_with_note() {
        let content = "é".repeat(MAX_CONTENT_CHARS + 1);
        let message = build_user_message(&content, &["What is here?".to_owned()]);

        assert!(message.contains(TRUNCATION_NOTE.trim()));
        assert!(message.contains("1. What is here?"));
    }

    #[test]
    fn prompts_are_distinct_and_constrained() {
        let prompts = [
            system_prompt(DetailLevel::Brief),
            system_prompt(DetailLevel::Normal),
            system_prompt(DetailLevel::Detailed),
        ];

        assert_ne!(prompts[0], prompts[1]);
        assert_ne!(prompts[1], prompts[2]);
        for prompt in prompts {
            assert!(prompt.contains("present in the provided content"));
            assert!(prompt.contains("Do not fabricate"));
            assert!(prompt.contains("A link is not an answer"));
        }
    }
}
