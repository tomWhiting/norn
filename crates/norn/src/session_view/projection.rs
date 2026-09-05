//! One deterministic semantic projection; no store reads, terminal state or execution effects.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use super::body::{BodyOrigin, BodyRef, BodyRepresentation, DisplayText};
use super::contract::{
    AcceptedModel, AttemptKey, CoverageGap, HistoryCursor, HistoryPosition, HistoryRecord,
    ItemDirection, ItemId, ItemInclusion, ProvisionalKey, SegmentKey, ViewCoverage, ViewItem,
    ViewItemKind, ViewSource,
};
use super::error::ViewError;
use super::index::ItemIndex;
use super::publication::PublicationState;
use crate::provider::agent_event::AgentEvent;
use crate::session::events::EventId;

pub(super) struct StoredBody {
    pub revision: u64,
    pub text: String,
}

pub(super) struct Execution {
    pub attempt: AttemptKey,
    pub model: AcceptedModel,
    pub completed_text: bool,
    pub completed_thinking: bool,
    pub generic_text: Vec<SegmentKey>,
    pub generic_thinking: Vec<SegmentKey>,
    pub segment: u64,
    pub last_segment: Option<SegmentKey>,
}

/// Outcome of one live reduction, retaining the completed local response identity.
#[derive(Clone, Debug)]
pub struct LiveReduction {
    /// Projection revision after the event.
    pub revision: u64,
    /// Response window ended by this event, when present.
    pub completed_attempt: Option<AttemptKey>,
    /// Exact retained notice created by this event's typed response completion.
    pub completion_item: Option<ItemId>,
    /// Event had no approved display body; only safe metadata was retained.
    pub metadata_only: bool,
}

/// A requested range of one exact provisional body revision.
#[derive(Clone, Debug)]
pub struct ProvisionalBodyChunk {
    /// Same capability identity requested by the caller.
    pub body: BodyRef,
    /// Original-content byte offset.
    pub offset: usize,
    /// Next original-content byte offset.
    pub next_offset: usize,
    /// Whether the current revision ends at this chunk.
    pub complete: bool,
    /// Exact approved original bytes in this demanded UTF-8 chunk. This lets
    /// frontends map escaped display offsets without reversing sanitization.
    pub original_text: String,
    /// Terminal-safe text; escape expansion does not change the byte cursor.
    pub text: DisplayText,
}

/// Retained semantic state for exactly one agent and local store generation.
pub struct SessionProjection {
    pub(super) source: ViewSource,
    pub(super) items: ItemIndex,
    pub(super) bodies: HashMap<ProvisionalKey, StoredBody>,
    pub(super) body_attempts: HashMap<AttemptKey, HashSet<ProvisionalKey>>,
    pub(super) local_bodies: HashMap<u64, StoredBody>,
    pub(super) aliases: HashMap<ItemId, ItemId>,
    pub(super) ordinals: BTreeMap<usize, EventId>,
    pub(super) events: HashMap<EventId, usize>,
    pub(super) complete_prefix: usize,
    pub(super) event_observations: HashMap<EventId, ItemId>,
    pub(super) execution: Option<Execution>,
    pub(super) publication: PublicationState,
    pub(super) completion_item: Option<ItemId>,
    pub(super) coverage: ViewCoverage,
    pub(super) revision: u64,
    pub(super) local_ordinal: u64,
}

impl SessionProjection {
    /// Create an empty view of the owner-supplied actual source.
    #[must_use]
    pub fn new(source: ViewSource) -> Self {
        Self {
            coverage: ViewCoverage {
                gaps: BTreeSet::new(),
                missed_live_events: 0,
                observed_cursor: HistoryCursor::empty(source.clone()),
            },
            source,
            items: ItemIndex::new(),
            bodies: HashMap::new(),
            body_attempts: HashMap::new(),
            local_bodies: HashMap::new(),
            aliases: HashMap::new(),
            ordinals: BTreeMap::new(),
            events: HashMap::new(),
            complete_prefix: 0,
            event_observations: HashMap::new(),
            execution: None,
            publication: PublicationState::new(),
            completion_item: None,
            revision: 0,
            local_ordinal: 0,
        }
    }

    /// Actual source bound to this projection.
    #[must_use]
    pub const fn source(&self) -> &ViewSource {
        &self.source
    }

    /// Current semantic rows; body contents require explicit demand.
    #[must_use]
    pub fn items(&self) -> impl DoubleEndedIterator<Item = &ViewItem> + ExactSizeIterator {
        self.items.iter()
    }

    /// Resolve a stable item identity without scanning retained history.
    #[must_use]
    pub fn item(&self, id: &ItemId) -> Option<&ViewItem> {
        self.items.get(id)
    }

    /// Traverse from an exact current item through the existing ordered index.
    /// Earlier traversal is nearest-first. This neither resolves aliases nor
    /// clones rows or bodies; missing and foreign anchors are explicit errors.
    pub fn items_from<'a>(
        &'a self,
        anchor: &ItemId,
        direction: ItemDirection,
        inclusion: ItemInclusion,
    ) -> Result<impl DoubleEndedIterator<Item = &'a ViewItem> + use<'a>, ViewError> {
        let actual = match anchor {
            ItemId::Committed { cursor, .. } => cursor.source(),
            ItemId::Provisional(key) => &key.source,
            ItemId::Local { source, .. } => source,
        };
        if actual != &self.source {
            return Err(ViewError::SourceMismatch {
                expected: Box::new(self.source.clone()),
                actual: Box::new(actual.clone()),
            });
        }
        self.items.items_from(anchor, direction, inclusion)
    }

    /// Local view revision, independent of the committed cursor.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Explicit current coverage, without a Ready or durability claim.
    #[must_use]
    pub const fn coverage(&self) -> &ViewCoverage {
        &self.coverage
    }

    /// Proven identity aliases only; callers must still validate body revision.
    #[must_use]
    pub fn alias(&self, previous: &ItemId) -> Option<&ItemId> {
        let mut target = self.aliases.get(previous)?;
        for _ in 0..self.aliases.len() {
            match self.aliases.get(target) {
                Some(next) => target = next,
                None => return self.items.get(target).map(|_| target),
            }
        }
        None
    }

    /// Begin an actually admitted local execution with its accepted model policy.
    pub fn begin_execution(
        &mut self,
        execution: Uuid,
        model: AcceptedModel,
    ) -> Result<AttemptKey, ViewError> {
        if self.execution.is_some() {
            return Err(ViewError::AttemptMismatch);
        }
        self.publication.reset_execution();
        let attempt = AttemptKey {
            execution,
            response: 0,
            attempt: 1,
        };
        self.execution = Some(Execution {
            attempt: attempt.clone(),
            model,
            completed_text: false,
            completed_thinking: false,
            generic_text: Vec::new(),
            generic_thinking: Vec::new(),
            segment: 0,
            last_segment: None,
        });
        self.bump()?;
        Ok(attempt)
    }

    /// Current volatile response attempt, supplied to explicit commit association.
    #[must_use]
    pub fn current_attempt(&self) -> Option<&AttemptKey> {
        self.execution.as_ref().map(|execution| &execution.attempt)
    }

    /// Observe one typed event from the owning agent, without running any effects.
    pub fn apply_live(&mut self, event: &AgentEvent) -> Result<LiveReduction, ViewError> {
        if event.agent_id != self.source.agent_id {
            return Err(ViewError::AgentMismatch {
                expected: self.source.agent_id,
                actual: event.agent_id,
            });
        }
        self.validate_live_envelope(&event.event)?;
        self.completion_item = None;
        let (completed_attempt, metadata_only) = self.reduce_live(&event.event)?;
        self.bump()?;
        Ok(LiveReduction {
            revision: self.revision,
            completed_attempt,
            completion_item: self.completion_item.take(),
            metadata_only,
        })
    }

    /// Apply compact metadata minted by the actual store owner; raw persisted
    /// provider payloads do not cross this frontend boundary.
    pub fn apply_history_record(&mut self, record: &HistoryRecord) -> Result<(), ViewError> {
        let cursor = &record.cursor;
        let HistoryPosition::Event { ordinal, event_id } = cursor.position() else {
            return Err(ViewError::AttemptMismatch);
        };
        cursor.validate(&self.source, *ordinal, event_id)?;
        if let Some(existing) = self.events.get(event_id) {
            return if existing == ordinal {
                Ok(())
            } else {
                Err(ViewError::HistoryConflict {
                    ordinal: *ordinal,
                    event_id: event_id.clone(),
                })
            };
        }
        if self
            .ordinals
            .get(ordinal)
            .is_some_and(|existing| existing != event_id)
        {
            return Err(ViewError::HistoryConflict {
                ordinal: *ordinal,
                event_id: event_id.clone(),
            });
        }
        self.items.observe_committed(*ordinal)?;
        for row in &record.items {
            self.items.insert(row.clone())?;
        }
        self.events.insert(event_id.clone(), *ordinal);
        self.ordinals.insert(*ordinal, event_id.clone());
        self.merge_observation(event_id, cursor)?;
        let prior_prefix = self.complete_prefix;
        while self.ordinals.contains_key(&self.complete_prefix) {
            self.complete_prefix =
                self.complete_prefix
                    .checked_add(1)
                    .ok_or(ViewError::CounterExhausted {
                        counter: "complete history prefix",
                    })?;
        }
        for orphan in self.items.pending_result_ids(
            event_id,
            prior_prefix,
            self.complete_prefix,
            &record.items,
        ) {
            self.join_committed_result(&orphan)?;
        }
        if let Some((ordinal, event_id)) = self.ordinals.last_key_value() {
            self.coverage.observed_cursor =
                HistoryCursor::event(self.source.clone(), *ordinal, event_id.clone());
            if self.ordinals.len().checked_sub(1) == Some(*ordinal) {
                self.coverage.gaps.remove(&CoverageGap::OlderHistoryMissing);
            } else {
                self.coverage.gaps.insert(CoverageGap::OlderHistoryMissing);
            }
        }
        self.bump()
    }

    /// Replace one explicitly identified live response window with its accepted
    /// assistant event. Generic fragments are not given invented exact aliases.
    pub fn reconcile_history_record(
        &mut self,
        attempt: &AttemptKey,
        record: &HistoryRecord,
    ) -> Result<(), ViewError> {
        if !record.assistant {
            return Err(ViewError::AttemptMismatch);
        }
        let previous: Vec<_> = self.items.attempt_ids(attempt).iter().filter_map(|id| self.items.get(id)).filter(|item| !matches!(&item.id, ItemId::Provisional(key) if matches!(key.segment, SegmentKey::ToolResult(_)))).cloned().collect();
        self.apply_history_record(record)?;
        if previous.is_empty() {
            self.coverage
                .gaps
                .insert(CoverageGap::IncompleteAssociation);
        }
        self.associate_response_items(&previous, record)?;
        self.invalidate_attempt(attempt, false);
        self.items
            .place_completions_after(attempt, record.cursor())?;
        self.bump()
    }

    /// Preserve committed state and explicitly record missing live coverage.
    pub fn mark_lagged(&mut self, missed: u64) -> Result<(), ViewError> {
        self.publication.mark_lagged();
        self.coverage.missed_live_events =
            self.coverage.missed_live_events.checked_add(missed).ok_or(
                ViewError::CounterExhausted {
                    counter: "missed live events",
                },
            )?;
        self.coverage.gaps.insert(CoverageGap::BroadcastLag);
        self.record_notice(
            ViewItemKind::Notice,
            "Live coverage is incomplete; committed history will be reconciled",
        )?;
        Ok(())
    }

    /// End local ownership. Interrupted provisional tool work is marked honestly;
    /// it is never converted to a fabricated successful or null result.
    pub fn end_execution(&mut self, interrupted: bool) -> Result<(), ViewError> {
        self.publication.reset_execution();
        if let Some(execution) = self.execution.take()
            && interrupted
        {
            self.coverage.gaps.insert(CoverageGap::Interrupted);
            self.interrupt_tools(execution.attempt.execution);
            self.record_notice(
                ViewItemKind::Notice,
                "Execution interrupted; uncommitted work remains incomplete",
            )?;
        }
        self.bump()
    }

    pub(super) fn bump(&mut self) -> Result<(), ViewError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(ViewError::CounterExhausted {
                counter: "projection revision",
            })?;
        Ok(())
    }

    pub(super) fn key(&self, segment: SegmentKey) -> Result<ProvisionalKey, ViewError> {
        Ok(ProvisionalKey {
            source: self.source.clone(),
            attempt: self
                .execution
                .as_ref()
                .ok_or(ViewError::NoExecution)?
                .attempt
                .clone(),
            segment,
        })
    }

    pub(super) fn store_body(
        &mut self,
        key: ProvisionalKey,
        text: &str,
        append: bool,
        representation: BodyRepresentation,
    ) -> Result<BodyRef, ViewError> {
        self.body_attempts
            .entry(key.attempt.clone())
            .or_default()
            .insert(key.clone());
        let stored = self
            .bodies
            .entry(key.clone())
            .or_insert_with(|| StoredBody {
                revision: 0,
                text: String::new(),
            });
        stored.revision = stored
            .revision
            .checked_add(1)
            .ok_or(ViewError::CounterExhausted {
                counter: "provisional body revision",
            })?;
        if append {
            stored.text.push_str(text);
        } else {
            text.clone_into(&mut stored.text);
        }
        Ok(BodyRef {
            origin: BodyOrigin::Provisional {
                key,
                revision: stored.revision,
                representation,
            },
        })
    }

    pub(super) fn put_fragment(
        &mut self,
        key: ProvisionalKey,
        kind: ViewItemKind,
        label: &str,
        text: &str,
        append: bool,
    ) -> Result<(), ViewError> {
        let body = self.store_body(key.clone(), text, append, BodyRepresentation::Text)?;
        let id = ItemId::Provisional(key);
        if let Some(item) = self.items.get_mut(&id) {
            item.bodies = vec![body];
        } else {
            self.items.insert(ViewItem {
                id,
                kind,
                label: DisplayText::new(label),
                bodies: vec![body],
                model: self
                    .execution
                    .as_ref()
                    .map(|execution| execution.model.clone()),
            })?;
        }
        Ok(())
    }

    pub(super) fn invalidate_attempt(&mut self, attempt: &AttemptKey, include_results: bool) {
        let removes = |key: &ProvisionalKey| {
            &key.attempt == attempt
                && (include_results || !matches!(key.segment, SegmentKey::ToolResult(_)))
        };
        for id in self.items.attempt_ids(attempt) {
            if matches!(&id, ItemId::Provisional(key) if removes(key)) {
                self.items.remove(&id);
            }
        }
        if let Some(keys) = self.body_attempts.remove(attempt) {
            for key in keys {
                if removes(&key) {
                    self.bodies.remove(&key);
                } else {
                    self.body_attempts
                        .entry(attempt.clone())
                        .or_default()
                        .insert(key);
                }
            }
        }
    }

    pub(super) fn link_alias(&mut self, previous: ItemId, target: ItemId) -> Result<(), ViewError> {
        if previous == target {
            return Ok(());
        }
        let mut current = &target;
        for _ in 0..=self.aliases.len() {
            if current == &previous {
                return Err(ViewError::AttemptMismatch);
            }
            if let Some(next) = self.aliases.get(current) {
                current = next;
            } else {
                self.aliases.insert(previous, target);
                return Ok(());
            }
        }
        Err(ViewError::AttemptMismatch)
    }
}
