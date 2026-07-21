//! Lossless reasoning-state validation for Responses replay.

use crate::error::ProviderError;
use crate::provider::request::{Message, MessageRole};

/// Reject a full request view that would replay reasoning or provider
/// compaction without its opaque provider state.
///
/// Stateful requests call this with their actual delta, so reasoning retained
/// behind a valid `previous_response_id` is not inspected or rejected. When a
/// fresh epoch or stateless backend must send an assistant reasoning or
/// compaction item, nonempty `encrypted_content` is the only lossless
/// continuation contract.
pub(in crate::provider::openai) fn validate_replayable_reasoning(
    messages: &[Message],
) -> Result<(), ProviderError> {
    for message in messages {
        if message.role != MessageRole::Assistant {
            continue;
        }

        if message.response_items.is_empty() {
            if message
                .reasoning
                .iter()
                .any(|item| item.encrypted_content.as_deref().is_none_or(str::is_empty))
            {
                return Err(ProviderError::ProviderStateReplayUnavailable);
            }
            continue;
        }

        if message.response_items.iter().any(|item| match &item.item {
            crate::provider::response_item::ResponseItem::Reasoning(reasoning) => {
                reasoning.encrypted_content().is_none_or(str::is_empty)
            }
            crate::provider::response_item::ResponseItem::Compaction(compaction) => {
                compaction.encrypted_content().is_empty()
            }
            _ => false,
        }) {
            return Err(ProviderError::ProviderStateReplayUnavailable);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::reasoning::ReasoningItem;
    use crate::provider::request::Message;
    use crate::provider::request::ProviderRequest;
    use crate::provider::response_item::{
        ResponseItem, ResponseItemError, ResponseStreamProvenance, ResponseTranscriptItem,
    };

    fn assistant_with_canonical(raw: serde_json::Value) -> Result<Message, ResponseItemError> {
        assistant_with_canonical_items(vec![raw])
    }

    fn assistant_with_canonical_items(
        raw_items: Vec<serde_json::Value>,
    ) -> Result<Message, ResponseItemError> {
        let response_items = raw_items
            .into_iter()
            .map(|raw| {
                ResponseItem::from_value(raw).map(|item| ResponseTranscriptItem {
                    item,
                    provenance: ResponseStreamProvenance::default(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Message {
            role: MessageRole::Assistant,
            content: None,
            thinking: String::new(),
            reasoning: Vec::new(),
            response_items,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        })
    }

    fn assistant_with_legacy(encrypted_content: Option<&str>) -> Message {
        assistant_with_legacy_items(&[encrypted_content])
    }

    fn assistant_with_legacy_items(encrypted_content: &[Option<&str>]) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: None,
            thinking: String::new(),
            reasoning: encrypted_content
                .iter()
                .enumerate()
                .map(|(index, value)| ReasoningItem {
                    id: format!("rs_fixture_{index}"),
                    summary: Vec::new(),
                    content: None,
                    encrypted_content: value.map(str::to_owned),
                })
                .collect(),
            response_items: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        }
    }

    fn request(messages: Vec<Message>) -> ProviderRequest {
        ProviderRequest {
            messages,
            tools: Vec::new(),
            model: "test-model".to_owned(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        }
    }

    #[test]
    fn canonical_reasoning_requires_nonempty_encrypted_content() -> Result<(), ResponseItemError> {
        for encrypted_content in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("")),
        ] {
            let mut raw = serde_json::json!({
                "type": "reasoning",
                "id": "rs_fixture",
                "summary": []
            });
            if let Some(value) = encrypted_content {
                raw["encrypted_content"] = value;
            }
            assert!(matches!(
                validate_replayable_reasoning(&[assistant_with_canonical(raw)?]),
                Err(ProviderError::ProviderStateReplayUnavailable)
            ));
        }

        let valid = assistant_with_canonical(serde_json::json!({
            "type": "reasoning",
            "id": "rs_fixture",
            "summary": [],
            "encrypted_content": "opaque"
        }))?;
        assert!(validate_replayable_reasoning(&[valid]).is_ok());
        Ok(())
    }

    #[test]
    fn legacy_reasoning_requires_nonempty_encrypted_content() {
        for value in [None, Some("")] {
            assert!(matches!(
                validate_replayable_reasoning(&[assistant_with_legacy(value)]),
                Err(ProviderError::ProviderStateReplayUnavailable)
            ));
        }
        assert!(validate_replayable_reasoning(&[assistant_with_legacy(Some("opaque"))]).is_ok());
    }

    #[test]
    fn mixed_canonical_reasoning_rejects_any_unreplayable_item() -> Result<(), ResponseItemError> {
        let valid = serde_json::json!({
            "type": "reasoning",
            "id": "rs_valid",
            "summary": [],
            "encrypted_content": "opaque"
        });
        let invalid = serde_json::json!({
            "type": "reasoning",
            "id": "rs_invalid",
            "summary": []
        });

        for items in [vec![valid.clone(), invalid.clone()], vec![invalid, valid]] {
            assert!(matches!(
                validate_replayable_reasoning(&[assistant_with_canonical_items(items)?]),
                Err(ProviderError::ProviderStateReplayUnavailable)
            ));
        }
        Ok(())
    }

    #[test]
    fn mixed_legacy_reasoning_rejects_any_unreplayable_item() {
        for items in [[Some("opaque"), None], [None, Some("opaque")]] {
            assert!(matches!(
                validate_replayable_reasoning(&[assistant_with_legacy_items(&items)]),
                Err(ProviderError::ProviderStateReplayUnavailable)
            ));
        }
    }

    #[test]
    fn provider_compaction_item_is_replayable_without_reasoning_projection()
    -> Result<(), ResponseItemError> {
        let compaction = assistant_with_canonical(serde_json::json!({
            "type": "compaction",
            "id": "cmp_fixture",
            "encrypted_content": "opaque-compaction"
        }))?;
        assert!(validate_replayable_reasoning(&[compaction]).is_ok());
        Ok(())
    }

    #[test]
    fn provider_compaction_requires_nonempty_encrypted_content() {
        let malformed = Message {
            role: MessageRole::Assistant,
            content: None,
            thinking: String::new(),
            reasoning: Vec::new(),
            response_items: vec![ResponseTranscriptItem {
                item: ResponseItem::malformed_empty_compaction_for_replay_test(),
                provenance: ResponseStreamProvenance::default(),
            }],
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        };

        assert!(matches!(
            validate_replayable_reasoning(&[malformed]),
            Err(ProviderError::ProviderStateReplayUnavailable)
        ));
    }

    #[test]
    fn payload_builder_rejects_unreplayable_reasoning_without_disclosure() {
        let result = super::super::build_payload(
            &request(vec![assistant_with_legacy(None)]),
            super::super::CATALOG_BACKEND_CODEX_SUBSCRIPTION,
        );

        assert!(matches!(
            result,
            Err(ProviderError::ProviderStateReplayUnavailable)
        ));
        let rendered = result.err().map(|error| format!("{error:?}"));
        assert!(rendered.as_deref().is_some_and(|text| {
            !text.contains("rs_fixture") && !text.contains("summary") && !text.contains("encrypted")
        }));
    }
}
