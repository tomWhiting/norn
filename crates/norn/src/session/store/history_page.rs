//! Explicit history demand and source-bound approved body reads outside store locks.

use std::num::NonZeroUsize;
use std::ops::Range;

use uuid::Uuid;

use super::{EventStore, StoreInner};
use crate::session::branch::{MailboxId, SessionBinding};
use crate::session::events::{EventId, SessionEvent};
use crate::session::spool::SpoolRangeError;
use crate::session_view::{
    BodyOrigin, BodyRange, BodyRef, HistoryCursor, HistoryPosition, HistoryRecord, SessionIdentity,
    ViewError, ViewSource, project_committed,
};

/// Explicit history ordering relative to an exclusive anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryDirection {
    /// Older records; returned in insertion order.
    Before,
    /// Newer records; returned in insertion order.
    After,
}

/// A caller-selected boundary; event cursors remain opaque store capabilities.
#[derive(Clone, Debug)]
pub enum HistoryAnchor {
    /// Before the first event, including an empty store.
    Start,
    /// After the last event observed by this read.
    End,
    /// Before or after this exact stored event, excluding the anchor itself.
    At(HistoryCursor),
}

/// Requested event window; no implicit retention cap or whole-history demand.
#[derive(Clone, Debug)]
pub struct HistoryRead {
    /// Owner-bound source for this operation.
    pub source: ViewSource,
    /// Exact event or explicit timeline boundary.
    pub anchor: HistoryAnchor,
    /// Direction from the exclusive anchor.
    pub direction: HistoryDirection,
    /// Maximum number of events explicitly requested.
    pub max_events: NonZeroUsize,
}

/// Compact approved records and coverage at this read's in-memory snapshot.
#[derive(Clone, Debug)]
pub struct HistoryPage {
    /// Exact owner source, including when no records were returned.
    pub source: ViewSource,
    /// Selected records in insertion order, without raw provider payloads.
    pub records: Vec<HistoryRecord>,
    /// Next exclusive anchor in the requested direction, when records exist.
    pub next: Option<HistoryCursor>,
    /// Accepted events existed before the selected range at this snapshot.
    pub has_before: bool,
    /// Accepted events existed after the selected range at this snapshot.
    pub has_after: bool,
    /// Number of accepted events observed; not a durable stream high-water mark.
    pub total_events: usize,
}

/// Explicit original-byte request for an opaque approved display capability.
#[derive(Clone, Debug)]
pub struct BodyRead {
    /// Exact owning event or projection revision.
    pub reference: BodyRef,
    /// Caller-declared byte demand.
    pub range: BodyRange,
}

/// Original approved bytes, ready for frontend escaping rather than direct painting.
#[derive(Clone, Debug)]
pub struct BodyPage {
    /// Same source, owner field, representation and revision as the request.
    pub reference: BodyRef,
    /// Actual original-byte interval returned after UTF-8 boundary adjustment.
    pub range: Range<usize>,
    /// Original text or raw serialized JSON, never a decoded spool field claim.
    pub text: String,
    /// Total original body bytes observed by the reader.
    pub total_bytes: usize,
    /// Explicit continuation when more original bytes remain.
    pub next_offset: Option<usize>,
}

/// A store-owned read or binding failed for the named source/event.
#[derive(Debug, thiserror::Error)]
pub enum HistoryReadError {
    /// The local store has not received its owner's session/agent binding.
    #[error("store instance {generation} has no session view binding")]
    Unbound {
        /// Actual local store instance.
        generation: Uuid,
    },
    /// An existing binding or managed spool names another session incarnation.
    #[error("session view binding {actual:?} does not match owner {expected:?}")]
    BindingMismatch {
        /// Owner identity, without a path capability.
        expected: SessionIdentity,
        /// Requested identity.
        actual: SessionIdentity,
    },
    /// A supplied binding names another generation of the same persisted session.
    #[error("session {session:?} binding generation {actual} differs from owner {expected}")]
    BindingGenerationMismatch {
        /// Actual named session.
        session: SessionIdentity,
        /// Owner's generation, not a view cursor.
        expected: Uuid,
        /// Supplied generation.
        actual: Uuid,
    },
    /// An empty-start boundary cannot identify a committed body event.
    #[error("empty history cursor for {view_source:?} does not name a body event")]
    EmptyCursor {
        /// Source supplied by the body operation.
        view_source: Box<ViewSource>,
    },
    /// A named cursor event is absent at its requested ordinal.
    #[error("session history event {event_id} is unavailable at ordinal {ordinal}")]
    EventUnavailable {
        /// Requested event identifier.
        event_id: EventId,
        /// Requested append ordinal.
        ordinal: usize,
    },
    /// An exact event lookup found no entry in this owner's event index.
    #[error("session view {view_source:?} has no indexed event {event_id}")]
    EventNotIndexed {
        /// Actual validated owner of the index.
        view_source: Box<ViewSource>,
        /// Exact requested identifier; no ordinal is invented for an absent event.
        event_id: EventId,
    },
    /// The current managed-session registration no longer admits this store owner.
    #[error("current session owner refused exact history event {event_id}: {source}")]
    CurrentOwner {
        /// Exact event requested through the previously bound store.
        event_id: EventId,
        /// Registered-generation or private-index failure.
        #[source]
        source: crate::session::persistence::SessionPersistError,
    },
    /// Shared display capability validation failed.
    #[error(transparent)]
    View(#[from] ViewError),
    /// The body exists only in its owning volatile projection.
    #[error("display body revision {revision} belongs to the projection, not the event store")]
    ProjectionOwned {
        /// Requested local body revision.
        revision: u64,
    },
    /// The event's spool file or registered generation could not be read.
    #[error(transparent)]
    Spool(#[from] SpoolRangeError),
}

pub(super) struct BoundViewSource {
    source: ViewSource,
    mailbox: MailboxId,
}

impl EventStore {
    /// Bind an actual owner-supplied session and agent once for this store instance.
    /// Managed stores also compare the spool's registered session generation.
    /// Sinkless stores cannot independently prove the supplied owner relationship.
    /// Repeating the same binding is idempotent; relabelling this store is refused.
    ///
    /// # Errors
    /// Returns a typed source or binding-generation mismatch when ownership differs.
    pub fn bind_view_source(
        &self,
        binding: &SessionBinding,
        agent_id: Uuid,
        parent_agent_id: Option<Uuid>,
    ) -> Result<ViewSource, HistoryReadError> {
        let session = binding
            .session_id()
            .map_or(SessionIdentity::Ephemeral(self.view_generation), |id| {
                SessionIdentity::Persisted(id.to_owned())
            });
        if let Some(spool) = &self.spool {
            spool.validate_view_binding(&session, binding.mailbox_id())?;
        }
        let requested = ViewSource {
            session,
            agent_id,
            parent_agent_id,
            store_generation: self.view_generation,
        };
        let bound = self.view_binding.get_or_init(|| BoundViewSource {
            source: requested.clone(),
            mailbox: binding.mailbox_id(),
        });
        validate_source(&bound.source, &requested)?;
        if bound.mailbox != binding.mailbox_id() {
            return Err(HistoryReadError::BindingGenerationMismatch {
                session: requested.session,
                expected: bound.mailbox.generation(),
                actual: binding.mailbox_id().generation(),
            });
        }
        Ok(requested)
    }

    /// Obtain this owner's explicit empty-start cursor without inventing an event.
    ///
    /// # Errors
    /// Refuses an unbound store or a source belonging to another owner/instance.
    pub fn history_start(&self, source: &ViewSource) -> Result<HistoryCursor, HistoryReadError> {
        self.validate_view_source(source)?;
        Ok(HistoryCursor::empty(source.clone()))
    }

    /// Clone only the selected raw records while locked, then project off-lock.
    /// This is an explicit demand operation; frontends must keep it off paint/resize.
    ///
    /// # Errors
    /// Refuses source, ordinal or event mismatches and malformed approved metadata.
    pub fn history_page(&self, read: &HistoryRead) -> Result<HistoryPage, HistoryReadError> {
        self.validate_view_source(&read.source)?;
        let (start, end, total_events, events) = {
            let inner = self.inner.read();
            let total = inner.events.len();
            let boundary = match &read.anchor {
                HistoryAnchor::Start => 0,
                HistoryAnchor::End => total,
                HistoryAnchor::At(cursor) => {
                    validate_source(&read.source, cursor.source())?;
                    match cursor.position() {
                        HistoryPosition::Empty => 0,
                        HistoryPosition::Event { ordinal, .. } => {
                            validate_cursor(&inner, &read.source, cursor)?;
                            ordinal + usize::from(read.direction == HistoryDirection::After)
                        }
                    }
                }
            };
            let (start, end) = match read.direction {
                HistoryDirection::Before => {
                    (boundary.saturating_sub(read.max_events.get()), boundary)
                }
                HistoryDirection::After => (
                    boundary,
                    boundary.saturating_add(read.max_events.get()).min(total),
                ),
            };
            for (offset, event) in inner.events[start..end].iter().enumerate() {
                let ordinal = start + offset;
                if inner.index.get(&event.base().id) != Some(&ordinal) {
                    return Err(ViewError::HistoryConflict {
                        ordinal,
                        event_id: event.base().id.clone(),
                    }
                    .into());
                }
            }
            (start, end, total, inner.events[start..end].to_vec())
        };
        let records = events
            .iter()
            .enumerate()
            .map(|(offset, event)| {
                let cursor = HistoryCursor::event(
                    read.source.clone(),
                    start + offset,
                    event.base().id.clone(),
                );
                project_committed(&cursor, event)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next = match read.direction {
            HistoryDirection::Before => records.first(),
            HistoryDirection::After => records.last(),
        }
        .map(|record| record.cursor().clone());
        Ok(HistoryPage {
            source: read.source.clone(),
            records,
            next,
            has_before: start > 0,
            has_after: end < total_events,
            total_events,
        })
    }

    /// Read one exact accepted event through its owning index, without scanning history.
    /// Only that event is cloned under the store lock; compact projection runs off-lock.
    /// This proves store acceptance and identity, not an additional durability guarantee.
    pub fn history_record(
        &self,
        source: &ViewSource,
        event_id: &EventId,
    ) -> Result<HistoryRecord, HistoryReadError> {
        self.validate_view_source(source)?;
        if let Some(spool) = &self.spool {
            spool
                .validate_record_owner()
                .map_err(|source| HistoryReadError::CurrentOwner {
                    event_id: event_id.clone(),
                    source,
                })?;
        }
        // Current managed ownership was checked immediately above. That index
        // guard is no longer held; the following snapshot is local store state,
        // not a cross-process generation lease or a new durability guarantee.
        let (cursor, event) = {
            let inner = self.inner.read();
            let ordinal = inner.index.get(event_id).copied().ok_or_else(|| {
                HistoryReadError::EventNotIndexed {
                    view_source: Box::new(source.clone()),
                    event_id: event_id.clone(),
                }
            })?;
            let cursor = HistoryCursor::event(source.clone(), ordinal, event_id.clone());
            let event = validate_cursor(&inner, source, &cursor)?.clone();
            (cursor, event)
        };
        Ok(project_committed(&cursor, &event)?)
    }

    /// Read only an approved committed body; projection-owned bodies are refused.
    /// Raw JSON inline serialization and filesystem work occur after releasing the store lock.
    ///
    /// # Errors
    /// Refuses invalid source/event/field/range capabilities, projection-owned bodies,
    /// unavailable spools, stale registered generations and filesystem failures.
    pub fn read_body(&self, read: &BodyRead) -> Result<BodyPage, HistoryReadError> {
        let (cursor, field, representation) = match read.reference.origin() {
            BodyOrigin::Committed {
                cursor,
                field,
                representation,
            } => (cursor, field, representation),
            BodyOrigin::Provisional { revision, .. } | BodyOrigin::Local { revision, .. } => {
                return Err(HistoryReadError::ProjectionOwned {
                    revision: *revision,
                });
            }
        };
        self.validate_view_source(cursor.source())?;
        let event = {
            let inner = self.inner.read();
            validate_cursor(&inner, cursor.source(), cursor)?.clone()
        };
        let actual = crate::session_view::body::validate_display_field(&event, field)?;
        if actual != *representation {
            return Err(ViewError::FieldUnavailable {
                event_id: event.base().id.clone(),
            }
            .into());
        }
        if *field == crate::session_view::DisplayField::ToolOutputSpool {
            let spool = self
                .spool
                .as_ref()
                .ok_or_else(|| ViewError::SpoolRequired {
                    event_id: event.base().id.clone(),
                })?;
            let chunk = spool.read_event_range(&event, read.range)?;
            return Ok(BodyPage {
                reference: read.reference.clone(),
                range: read.range.offset..chunk.end,
                text: chunk.text,
                total_bytes: chunk.total_bytes,
                next_offset: (chunk.end < chunk.total_bytes).then_some(chunk.end),
            });
        }
        let body = crate::session_view::body::resolve_committed_body(&event, field)?;
        let (text, end) = read.range.slice(&body)?;
        Ok(BodyPage {
            reference: read.reference.clone(),
            range: read.range.offset..end,
            text: text.to_owned(),
            total_bytes: body.len(),
            next_offset: (end < body.len()).then_some(end),
        })
    }

    fn validate_view_source(&self, source: &ViewSource) -> Result<(), HistoryReadError> {
        let owner = self.view_binding.get().ok_or(HistoryReadError::Unbound {
            generation: self.view_generation,
        })?;
        validate_source(&owner.source, source)?;
        if let Some(spool) = &self.spool {
            spool.validate_view_binding(&owner.source.session, owner.mailbox)?;
        }
        Ok(())
    }
}

fn validate_source(expected: &ViewSource, actual: &ViewSource) -> Result<(), HistoryReadError> {
    if expected != actual {
        return Err(ViewError::SourceMismatch {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual.clone()),
        }
        .into());
    }
    Ok(())
}

fn validate_cursor<'a>(
    inner: &'a StoreInner,
    source: &ViewSource,
    cursor: &HistoryCursor,
) -> Result<&'a SessionEvent, HistoryReadError> {
    let HistoryPosition::Event { ordinal, event_id } = cursor.position() else {
        return Err(HistoryReadError::EmptyCursor {
            view_source: Box::new(cursor.source().clone()),
        });
    };
    let event = inner
        .events
        .get(*ordinal)
        .ok_or_else(|| HistoryReadError::EventUnavailable {
            event_id: event_id.clone(),
            ordinal: *ordinal,
        })?;
    cursor.validate(source, *ordinal, &event.base().id)?;
    if inner.index.get(event_id) != Some(ordinal) {
        return Err(ViewError::HistoryConflict {
            ordinal: *ordinal,
            event_id: event_id.clone(),
        }
        .into());
    }
    Ok(event)
}
