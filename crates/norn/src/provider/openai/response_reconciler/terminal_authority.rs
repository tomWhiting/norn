//! Dialect-aware terminal item authority.

use std::collections::BTreeSet;

use serde_json::Value;

use super::wire::{authoritative_items_failure, item_type_allows_missing_id, parse_terminal_items};
use super::{
    ReconcileUpdate, ResponseItemIdentity, ResponseReconciler, ResponseReconciliationError,
    reconcile_authoritative_deltas,
};
use crate::provider::openai::sse::SseEvent;
use crate::provider::response_item::ResponseTranscriptItem;

/// Determines which stream frame supplies ordered terminal item authority.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::provider::openai) enum TerminalOutputPolicy {
    /// Public Responses requires the terminal `response.output` array.
    #[default]
    StrictPublic,
    /// Codex may omit or empty terminal output after authoritative item-done frames.
    CodexCompletedItemsFallback,
}

impl ResponseReconciler {
    pub(super) fn finish(
        &mut self,
        event: &SseEvent,
        sequence_number: u64,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let response = event.data.get("response").unwrap_or(&event.data);
        let (parsed_items, used_completed_fallback) =
            self.terminal_items(response, sequence_number)?;
        Self::validate_authoritative_item_schemas(&parsed_items)?;
        let enforce_resolution = event.event_type != "response.failed";
        if enforce_resolution && let Some(error) = authoritative_items_failure(&parsed_items, true)?
        {
            return Err(error);
        }
        let mut items = Vec::with_capacity(parsed_items.len());
        let mut terminal_identities = BTreeSet::new();
        let mut delta_reconciliations = Vec::new();
        let mut terminal_deltas = self.deltas.clone();
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
                    &mut terminal_deltas,
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
        if enforce_resolution {
            self.validate_terminal_core_deltas(&terminal_identities)?;
            self.validate_actionable_resolution(&terminal_identities, &items)?;
            if used_completed_fallback {
                self.validate_fallback_announcements(&terminal_identities)?;
            }
        }
        self.deltas = terminal_deltas;
        self.terminal = true;
        Ok(ReconcileUpdate::Terminal {
            items,
            delta_reconciliations,
        })
    }

    fn terminal_items(
        &self,
        response: &Value,
        sequence_number: u64,
    ) -> Result<(Vec<ResponseTranscriptItem>, bool), ResponseReconciliationError> {
        match response.get("output") {
            Some(Value::Array(output))
                if self.terminal_output_policy == TerminalOutputPolicy::StrictPublic
                    || !output.is_empty() =>
            {
                Ok((parse_terminal_items(output, sequence_number)?, false))
            }
            None | Some(Value::Array(_))
                if self.terminal_output_policy
                    == TerminalOutputPolicy::CodexCompletedItemsFallback =>
            {
                Ok((self.completed_item_fallback()?, true))
            }
            None | Some(_) => Err(ResponseReconciliationError::MissingTerminalOutput),
        }
    }

    fn completed_item_fallback(
        &self,
    ) -> Result<Vec<ResponseTranscriptItem>, ResponseReconciliationError> {
        for (expected_index, identity) in self.completed.keys().enumerate() {
            let expected_index = u64::try_from(expected_index).map_err(|error| {
                ResponseReconciliationError::OutputIndexOverflow {
                    reason: error.to_string(),
                }
            })?;
            if identity.output_index() != expected_index {
                return Err(ResponseReconciliationError::NonContiguousCompletedItemOutput);
            }
        }
        Ok(self.completed.values().cloned().collect())
    }

    fn validate_fallback_announcements(
        &self,
        terminal_identities: &BTreeSet<ResponseItemIdentity>,
    ) -> Result<(), ResponseReconciliationError> {
        if self
            .added
            .keys()
            .any(|identity| !terminal_identities.contains(identity))
        {
            return Err(ResponseReconciliationError::AnnouncementAbsentFromTerminal);
        }
        Ok(())
    }
}
