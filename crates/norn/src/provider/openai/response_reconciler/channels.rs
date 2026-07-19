//! Authoritative reconciliation for completed preview channels.

use serde_json::Value;

use super::authoritative::{authoritative_channels, reconcile_preview};
use super::wire::required_u64;
use super::{
    DeltaReconciliationDisposition, ReconcileUpdate, ResponseDeltaChannel, ResponseItemIdentity,
    ResponseReconciler, ResponseReconciliationError,
};
use crate::provider::openai::sse::SseEvent;
use crate::provider::response_item::ResponseItem;

struct ChannelCompletionSpec {
    channel: ResponseDeltaChannel,
    value_field: &'static str,
}

impl ResponseReconciler {
    /// Return how a completed channel reconciled with its accumulated preview.
    #[must_use]
    pub fn completed_channel_reconciliation(
        &self,
        item_id: &str,
        output_index: u64,
        channel: ResponseDeltaChannel,
    ) -> Option<DeltaReconciliationDisposition> {
        let identity = ResponseItemIdentity {
            item_id: Some(item_id.to_owned()),
            output_index,
        };
        self.channel_reconciliations
            .get(&(identity, channel))
            .copied()
    }

    pub(super) fn complete_channel(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let spec =
            completion_spec(event)?.ok_or(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: "response channel completion",
                field: "type",
            })?;
        let identity = self.bind_envelope_identity(event)?;
        let added = self
            .added
            .get(&identity)
            .ok_or(ResponseReconciliationError::UnannouncedChannelCompletionIdentity)?;
        let item_type = added.raw.get("type").and_then(Value::as_str);
        if item_type.is_none_or(|kind| !channel_matches_item_type(spec.channel, kind)) {
            return Err(ResponseReconciliationError::ChannelCompletionItemKindConflict);
        }
        let authoritative = event
            .data
            .get(spec.value_field)
            .and_then(Value::as_str)
            .ok_or(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: channel_done_event_name(spec.channel),
                field: spec.value_field,
            })?;
        let key = (identity.clone(), spec.channel);
        if let Some(prior) = self.completed_channels.get(&key) {
            return if prior == authoritative {
                Ok(ReconcileUpdate::DuplicateChannelCompletion)
            } else {
                Err(ResponseReconciliationError::ConflictingChannelCompletion)
            };
        }
        if self.completed.contains_key(&identity) {
            return Err(ResponseReconciliationError::ChannelCompletionAfterItemCompletion);
        }
        let preview = self.deltas.get(&key).cloned();
        let delta_reconciliation =
            reconcile_preview(&identity, spec.channel, preview, authoritative)?;
        let disposition = delta_reconciliation.disposition;
        self.deltas.insert(key.clone(), authoritative.to_owned());
        self.completed_channels
            .insert(key.clone(), authoritative.to_owned());
        self.channel_reconciliations.insert(key, disposition);
        Ok(ReconcileUpdate::CompletedChannel {
            delta_reconciliation,
        })
    }

    pub(super) fn validate_completed_channels(
        &self,
        identity: &ResponseItemIdentity,
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        let authoritative = authoritative_channels(item)?;
        for ((completed_identity, channel), completed_value) in &self.completed_channels {
            if completed_identity != identity {
                continue;
            }
            if authoritative.get(channel) != Some(completed_value) {
                return Err(ResponseReconciliationError::ChannelItemCompletionConflict);
            }
        }
        Ok(())
    }

    pub(super) fn validate_terminal_channels(
        &self,
        terminal_identities: &std::collections::BTreeSet<ResponseItemIdentity>,
    ) -> Result<(), ResponseReconciliationError> {
        if self
            .completed_channels
            .keys()
            .any(|(identity, _)| !terminal_identities.contains(identity))
        {
            return Err(ResponseReconciliationError::ChannelCompletionAbsentFromTerminal);
        }
        Ok(())
    }

    pub(super) fn validate_terminal_core_deltas(
        &self,
        terminal_identities: &std::collections::BTreeSet<ResponseItemIdentity>,
    ) -> Result<(), ResponseReconciliationError> {
        if self.deltas.keys().any(|(identity, channel)| {
            matches!(
                channel,
                ResponseDeltaChannel::OutputText(_)
                    | ResponseDeltaChannel::Refusal(_)
                    | ResponseDeltaChannel::ReasoningSummaryText(_)
                    | ResponseDeltaChannel::ReasoningText(_)
            ) && !terminal_identities.contains(identity)
        }) {
            return Err(ResponseReconciliationError::CoreDeltaAbsentFromTerminal);
        }
        Ok(())
    }
}

pub(super) fn channel_matches_item_type(channel: ResponseDeltaChannel, item_type: &str) -> bool {
    match channel {
        ResponseDeltaChannel::OutputText(_) | ResponseDeltaChannel::Refusal(_) => {
            item_type == "message"
        }
        ResponseDeltaChannel::FunctionCallArguments => item_type == "function_call",
        ResponseDeltaChannel::CustomToolCallInput => item_type == "custom_tool_call",
        ResponseDeltaChannel::ReasoningSummaryText(_) | ResponseDeltaChannel::ReasoningText(_) => {
            item_type == "reasoning"
        }
    }
}

fn completion_spec(
    event: &SseEvent,
) -> Result<Option<ChannelCompletionSpec>, ResponseReconciliationError> {
    let spec = match event.event_type.as_str() {
        "response.output_text.done" => indexed_spec(
            event,
            "response.output_text.done",
            "content_index",
            ResponseDeltaChannel::OutputText,
            "text",
        )?,
        "response.refusal.done" => indexed_spec(
            event,
            "response.refusal.done",
            "content_index",
            ResponseDeltaChannel::Refusal,
            "refusal",
        )?,
        "response.reasoning_text.done" => indexed_spec(
            event,
            "response.reasoning_text.done",
            "content_index",
            ResponseDeltaChannel::ReasoningText,
            "text",
        )?,
        "response.reasoning_summary_text.done" => indexed_spec(
            event,
            "response.reasoning_summary_text.done",
            "summary_index",
            ResponseDeltaChannel::ReasoningSummaryText,
            "text",
        )?,
        "response.function_call_arguments.done" => ChannelCompletionSpec {
            channel: ResponseDeltaChannel::FunctionCallArguments,
            value_field: "arguments",
        },
        "response.custom_tool_call_input.done" => ChannelCompletionSpec {
            channel: ResponseDeltaChannel::CustomToolCallInput,
            value_field: "input",
        },
        _ => return Ok(None),
    };
    Ok(Some(spec))
}

fn indexed_spec(
    event: &SseEvent,
    event_type: &'static str,
    index_field: &'static str,
    channel: fn(u64) -> ResponseDeltaChannel,
    value_field: &'static str,
) -> Result<ChannelCompletionSpec, ResponseReconciliationError> {
    Ok(ChannelCompletionSpec {
        channel: channel(required_u64(event, event_type, index_field)?),
        value_field,
    })
}

fn channel_done_event_name(channel: ResponseDeltaChannel) -> &'static str {
    match channel {
        ResponseDeltaChannel::OutputText(_) => "response.output_text.done",
        ResponseDeltaChannel::Refusal(_) => "response.refusal.done",
        ResponseDeltaChannel::FunctionCallArguments => "response.function_call_arguments.done",
        ResponseDeltaChannel::CustomToolCallInput => "response.custom_tool_call_input.done",
        ResponseDeltaChannel::ReasoningSummaryText(_) => "response.reasoning_summary_text.done",
        ResponseDeltaChannel::ReasoningText(_) => "response.reasoning_text.done",
    }
}
