//! Local display body ownership and exact committed-event observation reconciliation.

use super::body::{BodyOrigin, BodyRange, BodyRef, BodyRepresentation, DisplayField, DisplayText};
use super::contract::{
    HistoryCursor, HistoryPosition, HistoryRecord, ItemId, ViewItem, ViewItemKind,
};
use super::error::ViewError;
use super::projection::{ProvisionalBodyChunk, SessionProjection, StoredBody};
use crate::session::events::EventId;

impl SessionProjection {
    /// Replace one proven submitted human row with its exact accepted user record.
    /// The original local identity remains an alias, but only the canonical
    /// committed body survives. Local body revisions are retired rather than rebound.
    pub fn reconcile_input_record(
        &mut self,
        local: &ItemId,
        record: &HistoryRecord,
    ) -> Result<(), ViewError> {
        let HistoryPosition::Event { ordinal, event_id } = record.cursor.position() else {
            return Err(ViewError::AttemptMismatch);
        };
        record.cursor.validate(&self.source, *ordinal, event_id)?;
        let ItemId::Local { source, ordinal } = local else {
            return Err(ViewError::InputAssociation {
                local: Box::new(local.clone()),
                event_id: event_id.clone(),
            });
        };
        if source != &self.source {
            return Err(ViewError::SourceMismatch {
                expected: Box::new(self.source.clone()),
                actual: Box::new(source.clone()),
            });
        }
        let invalid = || ViewError::InputAssociation {
            local: Box::new(local.clone()),
            event_id: event_id.clone(),
        };
        let [canonical] = record.items.as_slice() else {
            return Err(invalid());
        };
        let [body] = canonical.bodies.as_slice() else {
            return Err(invalid());
        };
        if !matches!(canonical.kind, ViewItemKind::Input)
            || !matches!(body.origin(), BodyOrigin::Committed { cursor, field: DisplayField::UserContent, .. } if cursor == &record.cursor)
        {
            return Err(invalid());
        }
        if self.publication.input_owners.get(event_id) == Some(local)
            && self.alias(local) == Some(&canonical.id)
        {
            return Ok(());
        }
        if self.publication.input_owners.contains_key(event_id)
            || self.aliases.contains_key(local)
            || self.event_observations.contains_key(event_id)
        {
            return Err(invalid());
        }
        let submitted = self.items.get(local).ok_or_else(invalid)?;
        if !matches!(submitted.kind, ViewItemKind::Input) {
            return Err(invalid());
        }
        let label = submitted.label.clone();
        self.apply_history_record(record)?;
        let accepted = self.items.get_mut(&canonical.id).ok_or_else(invalid)?;
        accepted.label = label;
        accepted.bodies.clone_from(&canonical.bodies);
        if self.items.remove(local).is_none() {
            return Err(invalid());
        }
        self.local_bodies.remove(ordinal);
        self.link_alias(local.clone(), canonical.id.clone())?;
        self.publication
            .input_owners
            .insert(event_id.clone(), local.clone());
        self.bump()
    }

    /// Retain a frontend/runtime notice as safe data without provider authority.
    pub fn record_notice(&mut self, kind: ViewItemKind, label: &str) -> Result<ItemId, ViewError> {
        let id = ItemId::Local {
            source: self.source.clone(),
            ordinal: self.local_ordinal,
        };
        self.local_ordinal =
            self.local_ordinal
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted {
                    counter: "local item identity",
                })?;
        self.items.insert(ViewItem {
            id: id.clone(),
            kind,
            label: DisplayText::new(label),
            bodies: Vec::new(),
            model: None,
        })?;
        self.bump()?;
        Ok(id)
    }

    /// Read a demanded range only if the exact provisional body still exists.
    pub fn read_provisional(
        &self,
        body: &BodyRef,
        range: BodyRange,
    ) -> Result<ProvisionalBodyChunk, ViewError> {
        let (source, revision, stored) = match body.origin() {
            BodyOrigin::Provisional { key, revision, .. } => {
                (&key.source, *revision, self.bodies.get(key))
            }
            BodyOrigin::Local {
                source,
                ordinal,
                revision,
                ..
            } => (source, *revision, self.local_bodies.get(ordinal)),
            BodyOrigin::Committed { .. } => return Err(ViewError::AttemptMismatch),
        };
        if source != &self.source {
            return Err(ViewError::SourceMismatch {
                expected: Box::new(self.source.clone()),
                actual: Box::new(source.clone()),
            });
        }
        let stored = stored
            .filter(|stored| stored.revision == revision)
            .ok_or(ViewError::StaleBody { revision })?;
        let (text, next_offset) = range.slice(&stored.text)?;
        Ok(ProvisionalBodyChunk {
            body: body.clone(),
            offset: range.offset,
            next_offset,
            complete: next_offset == stored.text.len(),
            original_text: text.to_owned(),
            text: DisplayText::new(text),
        })
    }

    pub(super) fn local_body(
        &mut self,
        kind: ViewItemKind,
        label: &str,
        text: &str,
        representation: BodyRepresentation,
    ) -> Result<(), ViewError> {
        let ordinal = self.local_ordinal;
        let id = self.record_notice(kind, label)?;
        let body = BodyRef {
            origin: BodyOrigin::Local {
                source: self.source.clone(),
                ordinal,
                revision: 1,
                representation,
            },
        };
        self.local_bodies.insert(
            ordinal,
            StoredBody {
                revision: 1,
                text: text.to_owned(),
            },
        );
        if let Some(row) = self.items.get_mut(&id) {
            row.bodies.push(body);
        }
        Ok(())
    }

    /// Retain large frontend content once behind a revision-bound local body.
    /// This records display data only; it grants no operator or provider authority.
    pub fn record_local_body(
        &mut self,
        kind: ViewItemKind,
        label: &str,
        text: &str,
        representation: BodyRepresentation,
    ) -> Result<ItemId, ViewError> {
        let ordinal = self.local_ordinal;
        self.local_body(kind, label, text, representation)?;
        Ok(ItemId::Local {
            source: self.source.clone(),
            ordinal,
        })
    }

    pub(super) fn observe_event(
        &mut self,
        event_id: &EventId,
        kind: ViewItemKind,
        label: &str,
        text: &str,
        representation: BodyRepresentation,
    ) -> Result<(), ViewError> {
        if self.event_observations.contains_key(event_id) {
            return Ok(());
        }
        let ordinal = self.local_ordinal;
        self.local_body(kind, label, text, representation)?;
        let id = ItemId::Local {
            source: self.source.clone(),
            ordinal,
        };
        self.event_observations.insert(event_id.clone(), id);
        if let Some(ordinal) = self.events.get(event_id).copied() {
            self.merge_observation(
                event_id,
                &HistoryCursor::event(self.source.clone(), ordinal, event_id.clone()),
            )?;
        }
        Ok(())
    }

    pub(super) fn merge_observation(
        &mut self,
        event_id: &EventId,
        cursor: &HistoryCursor,
    ) -> Result<(), ViewError> {
        let Some(previous) = self.event_observations.get(event_id).cloned() else {
            return Ok(());
        };
        let target = ItemId::Committed {
            cursor: cursor.clone(),
            part: 0,
        };
        if previous == target {
            return Ok(());
        }
        let Some(observed) = self.items.remove(&previous) else {
            return Err(ViewError::AttemptMismatch);
        };
        let row = self
            .items
            .get_mut(&target)
            .ok_or(ViewError::AttemptMismatch)?;
        if matches!(observed.kind, ViewItemKind::ExternalInput) {
            row.bodies = observed.bodies;
        } else {
            row.bodies.extend(observed.bodies);
        }
        row.kind = observed.kind;
        row.label = observed.label;
        self.link_alias(previous, target.clone())?;
        self.event_observations.insert(event_id.clone(), target);
        Ok(())
    }
}
