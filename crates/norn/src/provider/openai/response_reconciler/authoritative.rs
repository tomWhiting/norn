//! Reconciliation of preview deltas with authoritative completed content.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    DeltaReconciliation, DeltaReconciliationDisposition, ResponseDeltaChannel,
    ResponseItemIdentity, ResponseReconciliationError,
};
use crate::provider::response_item::{ResponseContentPart, ResponseItem};

type DeltaMap = BTreeMap<(ResponseItemIdentity, ResponseDeltaChannel), String>;

pub(super) fn reconcile_authoritative_deltas(
    deltas: &mut DeltaMap,
    identity: &ResponseItemIdentity,
    item: &ResponseItem,
) -> Result<Vec<DeltaReconciliation>, ResponseReconciliationError> {
    let authoritative = authoritative_channels(item)?;
    if deltas
        .keys()
        .filter(|(delta_identity, _)| delta_identity == identity)
        .any(|(_, channel)| !authoritative.contains_key(channel))
    {
        return Err(ResponseReconciliationError::DeltaItemKindConflict);
    }

    let mut reconciliations = Vec::with_capacity(authoritative.len());
    for (channel, content) in authoritative {
        let key = (identity.clone(), channel);
        let disposition = match deltas.insert(key, content.clone()) {
            None => DeltaReconciliationDisposition::Synthesized,
            Some(preview) if preview == content => DeltaReconciliationDisposition::Matched,
            Some(_) => DeltaReconciliationDisposition::Repaired,
        };
        reconciliations.push(DeltaReconciliation {
            channel,
            disposition,
        });
    }
    Ok(reconciliations)
}

pub(super) fn authoritative_channels(
    item: &ResponseItem,
) -> Result<BTreeMap<ResponseDeltaChannel, String>, ResponseReconciliationError> {
    let mut channels = BTreeMap::new();
    match item {
        ResponseItem::Message(message) => {
            for (index, part) in message.content().iter().enumerate() {
                let index = content_index(index)?;
                match part {
                    ResponseContentPart::OutputText { text, .. } => {
                        channels.insert(ResponseDeltaChannel::OutputText(index), text.clone());
                    }
                    ResponseContentPart::Refusal { refusal, .. } => {
                        channels.insert(ResponseDeltaChannel::Refusal(index), refusal.clone());
                    }
                    ResponseContentPart::Opaque { .. } => {}
                }
            }
        }
        ResponseItem::Reasoning(reasoning) => {
            append_reasoning_parts(
                &mut channels,
                reasoning.summary(),
                ResponseDeltaChannel::ReasoningSummaryText,
                "summary_text",
            )?;
            if let Some(content) = reasoning.content() {
                append_reasoning_parts(
                    &mut channels,
                    content,
                    ResponseDeltaChannel::ReasoningText,
                    "reasoning_text",
                )?;
            }
        }
        ResponseItem::FunctionCall(call) => {
            channels.insert(
                ResponseDeltaChannel::FunctionCallArguments,
                call.arguments().to_owned(),
            );
        }
        ResponseItem::CustomToolCall(call) => {
            channels.insert(
                ResponseDeltaChannel::CustomToolCallInput,
                call.input().to_owned(),
            );
        }
        ResponseItem::WebSearchCall(_)
        | ResponseItem::Compaction(_)
        | ResponseItem::Known(_)
        | ResponseItem::Opaque(_) => {}
    }
    Ok(channels)
}

fn append_reasoning_parts(
    channels: &mut BTreeMap<ResponseDeltaChannel, String>,
    parts: &[Value],
    channel: fn(u64) -> ResponseDeltaChannel,
    expected_type: &str,
) -> Result<(), ResponseReconciliationError> {
    for (index, part) in parts.iter().enumerate() {
        if part.get("type").and_then(Value::as_str) != Some(expected_type) {
            continue;
        }
        let text = part.get("text").and_then(Value::as_str).ok_or(
            ResponseReconciliationError::MalformedAuthoritativeContent {
                reason: "reasoning text part missing text",
            },
        )?;
        channels.insert(channel(content_index(index)?), text.to_owned());
    }
    Ok(())
}

fn content_index(index: usize) -> Result<u64, ResponseReconciliationError> {
    u64::try_from(index).map_err(|error| ResponseReconciliationError::ContentIndexOverflow {
        reason: error.to_string(),
    })
}
