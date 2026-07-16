//! Identity-keyed reconciliation for Responses API stream frames.
//!
//! Deltas are preview data only. Canonical transcript items come exclusively
//! from authoritative `response.output_item.done` frames or the terminal
//! response's ordered `output` array. This module never executes tool calls.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::sse::SseEvent;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};

mod authoritative;
mod call_identity;
mod channels;
mod item_channels;
mod model;
mod roles;
mod wire;

use authoritative::reconcile_authoritative_deltas;
use channels::channel_matches_item_type;
use item_channels::ItemChannelState;
pub use model::{
    DeltaReconciliation, DeltaReconciliationDisposition, ReconcileUpdate, ResponseDeltaChannel,
    ResponseItemIdentity, ResponseReconciliationError,
};
use wire::{
    OutputItemSupport, announced_item_id, authoritative_items_failure, delta_event_name,
    delta_event_type, embedded_item_id, envelope_identity, item_type_allows_missing_id,
    output_item_support, parse_item, parse_terminal_items, required_object,
    required_sequence_number, required_u64, validate_output_text_delta_logprobs,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct FrameSignature {
    event_type: String,
    data: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AddedItem {
    raw: Value,
    support: OutputItemSupport,
}

/// Stateful, pure reconciler for one Responses stream.
#[derive(Debug, Default)]
pub struct ResponseReconciler {
    frames: BTreeMap<u64, FrameSignature>,
    highest_sequence: Option<u64>,
    ids_to_indices: BTreeMap<String, u64>,
    indices_to_ids: BTreeMap<u64, String>,
    call_ids_to_items: BTreeMap<String, ResponseItemIdentity>,
    added: BTreeMap<ResponseItemIdentity, AddedItem>,
    deltas: BTreeMap<(ResponseItemIdentity, ResponseDeltaChannel), String>,
    completed_channels: BTreeMap<(ResponseItemIdentity, ResponseDeltaChannel), String>,
    channel_reconciliations:
        BTreeMap<(ResponseItemIdentity, ResponseDeltaChannel), DeltaReconciliationDisposition>,
    item_channels: ItemChannelState,
    completed: BTreeMap<ResponseItemIdentity, ResponseTranscriptItem>,
    terminal: bool,
    failed: bool,
}

impl ResponseReconciler {
    /// Create an empty per-response reconciler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept one parsed SSE frame.
    ///
    /// # Errors
    ///
    /// Returns a typed protocol error for missing/conflicting sequence or item
    /// identity, malformed authoritative items, or unresolved actionable calls.
    pub fn ingest(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        if self.failed {
            return Err(ResponseReconciliationError::AlreadyFailed);
        }
        let sequence_number = match self.accept_sequence(event) {
            Ok(Some(sequence_number)) => sequence_number,
            Ok(None) => {
                return Ok(ReconcileUpdate::DuplicateSequence {
                    sequence_number: required_sequence_number(event)?,
                });
            }
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        if self.terminal {
            self.failed = true;
            return Err(ResponseReconciliationError::PostTerminalFrame);
        }
        let result = self.apply(event, sequence_number);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Return accumulated preview text for one identity and channel.
    #[must_use]
    pub fn accumulated_delta(
        &self,
        item_id: &str,
        output_index: u64,
        channel: ResponseDeltaChannel,
    ) -> Option<&str> {
        let identity = ResponseItemIdentity {
            item_id: Some(item_id.to_owned()),
            output_index,
        };
        self.deltas.get(&(identity, channel)).map(String::as_str)
    }

    fn accept_sequence(
        &mut self,
        event: &SseEvent,
    ) -> Result<Option<u64>, ResponseReconciliationError> {
        let sequence_number = required_sequence_number(event)?;
        let signature = FrameSignature {
            event_type: event.event_type.clone(),
            data: event.data.clone(),
        };
        if let Some(prior) = self.frames.get(&sequence_number) {
            return if *prior == signature {
                Ok(None)
            } else {
                Err(ResponseReconciliationError::ConflictingDuplicateSequence { sequence_number })
            };
        }
        if let Some(highest) = self.highest_sequence
            && sequence_number < highest
        {
            return Err(ResponseReconciliationError::NonMonotonicSequence {
                sequence_number,
                highest_sequence_number: highest,
            });
        }
        self.frames.insert(sequence_number, signature);
        self.highest_sequence = Some(sequence_number);
        Ok(Some(sequence_number))
    }

    fn add_item(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let raw = required_object(event, "response.output_item.added", "item")?;
        let output_index = required_u64(event, "response.output_item.added", "output_index")?;
        let item_type = raw.get("type").and_then(Value::as_str).ok_or(
            ResponseReconciliationError::InvalidEnvelopeField {
                event_type: "response.output_item.added",
                field: "item.type",
            },
        )?;
        let item_id = announced_item_id(raw, item_type, "response.output_item.added")?;
        let identity = self.bind_identity(item_id, output_index)?;
        self.bind_announced_call(&identity, raw, item_type, "response.output_item.added")?;
        let added = AddedItem {
            raw: raw.clone(),
            support: output_item_support(item_type),
        };
        if let Some(prior) = self.added.get(&identity)
            && prior != &added
        {
            return Err(ResponseReconciliationError::ConflictingAddedItem);
        }
        self.added.insert(identity, added);
        Ok(ReconcileUpdate::Accepted)
    }

    fn append_indexed_delta(
        &mut self,
        event: &SseEvent,
        channel: fn(u64) -> ResponseDeltaChannel,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let content_index = required_u64(event, delta_event_type(channel), "content_index")?;
        let identity = self.bind_envelope_identity(event)?;
        if self.content_part_is_done(&identity, content_index) {
            return Err(ResponseReconciliationError::ItemScopedEventAfterCompletion);
        }
        self.append_delta(event, channel(content_index))
    }

    fn append_summary_delta(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let index = required_u64(
            event,
            "response.reasoning_summary_text.delta",
            "summary_index",
        )?;
        let identity = self.bind_envelope_identity(event)?;
        if self.reasoning_summary_part_is_done(&identity, index) {
            return Err(ResponseReconciliationError::ItemScopedEventAfterCompletion);
        }
        self.append_delta(event, ResponseDeltaChannel::ReasoningSummaryText(index))
    }

    fn append_call_delta(
        &mut self,
        event: &SseEvent,
        channel: ResponseDeltaChannel,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        self.append_delta(event, channel)
    }

    fn append_delta(
        &mut self,
        event: &SseEvent,
        channel: ResponseDeltaChannel,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let identity = self.bind_envelope_identity(event)?;
        if self
            .completed_channels
            .contains_key(&(identity.clone(), channel))
        {
            return Err(ResponseReconciliationError::DeltaAfterChannelCompletion);
        }
        if self.completed.contains_key(&identity) {
            return Err(ResponseReconciliationError::DeltaAfterCompletion);
        }
        let added = self
            .added
            .get(&identity)
            .ok_or(ResponseReconciliationError::UnannouncedDeltaIdentity)?;
        let item_type = added.raw.get("type").and_then(Value::as_str);
        if item_type.is_none_or(|item_type| !channel_matches_item_type(channel, item_type)) {
            return Err(ResponseReconciliationError::DeltaItemKindConflict);
        }
        if matches!(channel, ResponseDeltaChannel::OutputText(_)) {
            validate_output_text_delta_logprobs(event)?;
        }
        let delta = event.data.get("delta").and_then(Value::as_str).ok_or(
            ResponseReconciliationError::InvalidEnvelopeField {
                event_type: delta_event_name(channel),
                field: "delta",
            },
        )?;
        self.deltas
            .entry((identity, channel))
            .or_default()
            .push_str(delta);
        Ok(ReconcileUpdate::Accepted)
    }

    fn complete_item(
        &mut self,
        event: &SseEvent,
        sequence_number: u64,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let raw = required_object(event, "response.output_item.done", "item")?;
        let output_index = required_u64(event, "response.output_item.done", "output_index")?;
        let item = parse_item(raw.clone(), "response.output_item.done")?;
        let item_id = embedded_item_id(event, &item)?;
        let mut transcript = ResponseTranscriptItem {
            item,
            provenance: ResponseStreamProvenance {
                item_id: item_id.clone(),
                output_index: Some(output_index),
                content_index: None,
                sequence_number: Some(sequence_number),
            },
        };
        if let Some(error) = authoritative_items_failure(std::slice::from_ref(&transcript), false) {
            return Err(error);
        }
        if item_id.is_none() && !item_type_allows_missing_id(transcript.item.item_type()) {
            return Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: "response.output_item.done",
                field: "item.id",
            });
        }
        let identity = self.bind_identity(item_id.as_deref(), output_index)?;
        transcript.provenance.item_id = identity.item_id().map(str::to_owned);
        self.validate_added_family(&identity, &transcript.item)?;
        self.validate_authoritative_call(
            &identity,
            transcript.item.raw(),
            transcript.item.item_type(),
            "response.output_item.done",
        )?;
        self.validate_completed_channels(&identity, &transcript.item)?;
        self.reconcile_item_channel_authority(&identity, &transcript.item)?;
        if let Some(prior) = self.completed.get(&identity) {
            return if prior.item.raw() == transcript.item.raw() {
                Ok(ReconcileUpdate::DuplicateCompletion { identity })
            } else {
                Err(ResponseReconciliationError::ConflictingCompletion)
            };
        }
        let delta_reconciliations =
            reconcile_authoritative_deltas(&mut self.deltas, &identity, &transcript.item)?;
        self.completed.insert(identity, transcript.clone());
        Ok(ReconcileUpdate::CompletedItem {
            item: Box::new(transcript),
            delta_reconciliations,
        })
    }

    fn finish(
        &mut self,
        event: &SseEvent,
        sequence_number: u64,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let response = event.data.get("response").unwrap_or(&event.data);
        let output = response
            .get("output")
            .and_then(Value::as_array)
            .ok_or(ResponseReconciliationError::MissingTerminalOutput)?;
        let parsed_items = parse_terminal_items(output, sequence_number)?;
        let enforce_actionable_resolution = event.event_type != "response.failed";
        if enforce_actionable_resolution
            && let Some(error) = authoritative_items_failure(&parsed_items, true)
        {
            return Err(error);
        }
        let mut items = Vec::with_capacity(parsed_items.len());
        let mut terminal_identities = BTreeSet::new();
        let mut delta_reconciliations = Vec::new();
        for mut transcript in parsed_items {
            let output_index = transcript.provenance.output_index.ok_or(
                ResponseReconciliationError::InvalidEnvelopeField {
                    event_type: "terminal response",
                    field: "output index",
                },
            )?;
            let item_id = transcript.item.id();
            if item_id.is_none() && !item_type_allows_missing_id(transcript.item.item_type()) {
                return Err(ResponseReconciliationError::InvalidEnvelopeField {
                    event_type: "terminal response",
                    field: "output item id",
                });
            }
            let identity = self.bind_identity(item_id, output_index)?;
            transcript.provenance.item_id = identity.item_id().map(str::to_owned);
            self.validate_added_family(&identity, &transcript.item)?;
            self.validate_authoritative_call(
                &identity,
                transcript.item.raw(),
                transcript.item.item_type(),
                "terminal response",
            )?;
            self.validate_completed_channels(&identity, &transcript.item)?;
            self.reconcile_item_channel_authority(&identity, &transcript.item)?;
            let transcript = if let Some(completed) = self.completed.get(&identity) {
                if completed.item.raw() != transcript.item.raw() {
                    return Err(ResponseReconciliationError::TerminalCompletionConflict);
                }
                completed.clone()
            } else {
                delta_reconciliations.extend(reconcile_authoritative_deltas(
                    &mut self.deltas,
                    &identity,
                    &transcript.item,
                )?);
                transcript
            };
            terminal_identities.insert(identity);
            items.push(transcript);
        }
        if self
            .completed
            .keys()
            .any(|identity| !terminal_identities.contains(identity))
        {
            return Err(ResponseReconciliationError::CompletionAbsentFromTerminal);
        }
        self.validate_terminal_channels(&terminal_identities)?;
        self.validate_terminal_item_channels(&terminal_identities)?;
        if enforce_actionable_resolution {
            self.validate_actionable_resolution(&terminal_identities, &items)?;
        }
        self.terminal = true;
        Ok(ReconcileUpdate::Terminal {
            items,
            delta_reconciliations,
        })
    }

    pub(super) fn bind_envelope_identity(
        &mut self,
        event: &SseEvent,
    ) -> Result<ResponseItemIdentity, ResponseReconciliationError> {
        let envelope = envelope_identity(event)?;
        self.bind_identity(envelope.item_id(), envelope.output_index())
    }

    fn bind_identity(
        &mut self,
        item_id: Option<&str>,
        output_index: u64,
    ) -> Result<ResponseItemIdentity, ResponseReconciliationError> {
        if let Some(item_id) = item_id {
            if let Some(prior_index) = self.ids_to_indices.get(item_id)
                && *prior_index != output_index
            {
                return Err(ResponseReconciliationError::ItemIdRebound {
                    item_id: item_id.to_owned(),
                    prior_index: *prior_index,
                    new_index: output_index,
                });
            }
            if let Some(prior_id) = self.indices_to_ids.get(&output_index)
                && prior_id != item_id
            {
                return Err(ResponseReconciliationError::OutputIndexRebound { output_index });
            }
            self.ids_to_indices.insert(item_id.to_owned(), output_index);
            self.indices_to_ids.insert(output_index, item_id.to_owned());
        }
        let item_id = self.indices_to_ids.get(&output_index).cloned();
        Ok(ResponseItemIdentity {
            item_id,
            output_index,
        })
    }

    fn validate_added_family(
        &self,
        identity: &ResponseItemIdentity,
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        let announced_type = self
            .added
            .get(identity)
            .and_then(|added| added.raw.get("type"))
            .and_then(Value::as_str);
        if announced_type.is_some_and(|item_type| item_type != item.item_type()) {
            return Err(ResponseReconciliationError::AddedItemKindConflict);
        }
        Ok(())
    }

    fn validate_actionable_resolution(
        &self,
        terminal: &BTreeSet<ResponseItemIdentity>,
        retained_items: &[ResponseTranscriptItem],
    ) -> Result<(), ResponseReconciliationError> {
        if self.deltas.keys().any(|(identity, channel)| {
            matches!(
                channel,
                ResponseDeltaChannel::FunctionCallArguments
                    | ResponseDeltaChannel::CustomToolCallInput
            ) && !terminal.contains(identity)
        }) {
            return Err(ResponseReconciliationError::DeltaOnlyActionableCall);
        }
        for (identity, added) in &self.added {
            if terminal.contains(identity) {
                continue;
            }
            match added.support {
                OutputItemSupport::SupportedExecutable => {
                    return Err(ResponseReconciliationError::UnresolvedActionableItem {
                        retained_items: retained_items.to_vec(),
                    });
                }
                OutputItemSupport::UnsupportedExecutable => {
                    return Err(ResponseReconciliationError::UnsupportedExecutableItem {
                        retained_items: retained_items.to_vec(),
                    });
                }
                OutputItemSupport::Unknown => {
                    return Err(ResponseReconciliationError::UnknownOutputItemType {
                        retained_items: retained_items.to_vec(),
                    });
                }
                OutputItemSupport::Inert => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
