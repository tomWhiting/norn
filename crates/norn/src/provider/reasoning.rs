//! Typed reasoning output items from the `OpenAI` Responses API.
//!
//! A `response.output_item.done` event with `item.type == "reasoning"`
//! carries the model's reasoning for the turn: human-readable summary
//! parts, optional raw reasoning-text content parts, and — when the
//! request included `reasoning.encrypted_content` — an opaque
//! `encrypted_content` blob. On the stateless-replay backend
//! (`response_threading: false`, the `ChatGPT`/Codex subscription shape)
//! that blob is the **only** way to thread the model's reasoning across
//! tool-call iterations: each captured item is echoed back as a
//! `"reasoning"` input item ahead of the assistant output it preceded
//! (see `serialize_assistant_into` in
//! [`crate::provider::openai::request`]). The Codex CLI reference
//! implements the same capture-and-replay contract
//! (`reference/codex-rs/protocol-models.rs:757-767`).

use serde::{Deserialize, Serialize};

/// One part of a reasoning item's `summary` array.
///
/// The wire shape is a tagged object (`{"type": "summary_text",
/// "text": …}`), mirrored by the Codex reference's
/// `ReasoningItemReasoningSummary` (`protocol-models.rs:1192-1196`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummaryPart {
    /// A natural-language summary segment.
    SummaryText {
        /// The summary text.
        text: String,
    },
}

/// One part of a reasoning item's optional `content` array.
///
/// Mirrors the Codex reference's `ReasoningItemContent`
/// (`protocol-models.rs:1198-1204`): raw reasoning text on models that
/// expose it, plus a plain-text part shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningContentPart {
    /// Raw reasoning text.
    ReasoningText {
        /// The reasoning text.
        text: String,
    },
    /// Plain text content.
    Text {
        /// The text.
        text: String,
    },
}

/// A complete reasoning output item captured from a
/// `response.output_item.done` SSE event.
///
/// The item is stored on the assistant [`Message`] that its turn
/// produced and replayed on the Responses API when `encrypted_content`
/// is present — without the blob the server cannot reconstruct the
/// reasoning state, so encrypted-content-less items are captured for
/// observability but never echoed.
///
/// [`Message`]: crate::provider::request::Message
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningItem {
    /// Server-assigned item identifier (`rs_*` on the wire). Retained
    /// for observability; replay deliberately omits it (the Codex
    /// reference applies `skip_serializing` to the same field at
    /// `protocol-models.rs:757-761` — the id is server-internal and the
    /// stateless backend rejects unknown item ids).
    #[serde(default)]
    pub id: String,
    /// Human-readable summary parts.
    #[serde(default)]
    pub summary: Vec<ReasoningSummaryPart>,
    /// Raw reasoning content parts, on models that expose them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ReasoningContentPart>>,
    /// Opaque encrypted reasoning state, present when the request asked
    /// for `include: ["reasoning.encrypted_content"]`. Required for
    /// replay on the stateless backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_full_wire_item() {
        let item: ReasoningItem = serde_json::from_value(serde_json::json!({
            "type": "reasoning",
            "id": "rs_123",
            "summary": [{"type": "summary_text", "text": "thought about it"}],
            "content": [{"type": "reasoning_text", "text": "raw chain"}],
            "encrypted_content": "opaque-blob",
        }))
        .expect("wire item deserializes");
        assert_eq!(item.id, "rs_123");
        assert_eq!(
            item.summary,
            vec![ReasoningSummaryPart::SummaryText {
                text: "thought about it".to_owned(),
            }],
        );
        assert_eq!(
            item.content,
            Some(vec![ReasoningContentPart::ReasoningText {
                text: "raw chain".to_owned(),
            }]),
        );
        assert_eq!(item.encrypted_content.as_deref(), Some("opaque-blob"));
    }

    #[test]
    fn deserializes_minimal_wire_item() {
        // Every field except the object itself is optional on the wire:
        // reasoning items without encrypted content (store: true
        // requests) and with an empty summary are legitimate.
        let item: ReasoningItem = serde_json::from_value(serde_json::json!({"type": "reasoning"}))
            .expect("minimal item deserializes");
        assert!(item.id.is_empty());
        assert!(item.summary.is_empty());
        assert!(item.content.is_none());
        assert!(item.encrypted_content.is_none());
    }

    #[test]
    fn serialization_omits_absent_optional_fields() {
        let item = ReasoningItem {
            id: "rs_1".to_owned(),
            summary: Vec::new(),
            content: None,
            encrypted_content: None,
        };
        let json = serde_json::to_value(&item).expect("serialize");
        assert!(json.get("content").is_none());
        assert!(json.get("encrypted_content").is_none());
    }
}
