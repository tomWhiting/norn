//! Explicit identity, coverage and display-capability failures; no payload dumps.

use crate::session::events::EventId;

/// A requested view operation cannot be applied to its named source or body.
#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    /// The caller supplied another source, agent or local store instance.
    #[error("session view source {actual:?} does not match owner {expected:?}")]
    SourceMismatch {
        /// Bound owner source.
        expected: Box<super::contract::ViewSource>,
        /// Source supplied by the operation.
        actual: Box<super::contract::ViewSource>,
    },
    /// The actual event emitter does not match this source's owning agent.
    #[error("session view agent {actual} does not match owner {expected}")]
    AgentMismatch {
        /// Owning agent.
        expected: uuid::Uuid,
        /// Supplied event agent.
        actual: uuid::Uuid,
    },
    /// The cursor did not name the supplied event.
    #[error("session view cursor does not name event {event_id}")]
    CursorMismatch {
        /// Event that must have been named.
        event_id: EventId,
    },
    /// An observed position or identifier conflicts with an earlier page.
    #[error("session history conflicts at ordinal {ordinal} for event {event_id}")]
    HistoryConflict {
        /// Conflicting append-order position.
        ordinal: usize,
        /// Supplied event identifier.
        event_id: EventId,
    },
    /// The named item is not currently present; aliases are resolved explicitly.
    #[error("session view item {item:?} is unavailable")]
    ItemUnavailable {
        /// Exact requested identity, without any body contents.
        item: Box<super::contract::ItemId>,
    },
    /// Live provider data arrived without an explicitly admitted execution.
    #[error("session view has no admitted execution for this provider event")]
    NoExecution,
    /// A retry or response association names the wrong execution window.
    #[error("session view response association does not match the named attempt")]
    AttemptMismatch,
    /// An opaque execution owner differs, or its admission has already closed.
    #[error("observed execution {actual} does not match admitted owner {expected:?}")]
    ExecutionObservationMismatch {
        /// Currently admitted execution, if any.
        expected: Option<uuid::Uuid>,
        /// Execution supplied by the producer observation.
        actual: uuid::Uuid,
    },
    /// A scoped execution received response data without its producer ticket.
    #[error("execution {execution} received response data without an attempt observation")]
    ObservationRequired {
        /// Actual admitted execution.
        execution: uuid::Uuid,
    },
    /// One attempt was associated with different accepted records.
    #[error("attempt {attempt:?} has conflicting publication for event {event_id}")]
    PublicationConflict {
        /// Exact producer attempt.
        attempt: Box<super::contract::AttemptKey>,
        /// Conflicting accepted event.
        event_id: EventId,
    },
    /// A human receipt cannot bind this local item to this accepted user record.
    #[error("local input {local:?} cannot be associated with accepted event {event_id}")]
    InputAssociation {
        /// Exact submitted local item, with its actual source.
        local: Box<super::contract::ItemId>,
        /// Exact accepted event supplied by the producer.
        event_id: EventId,
    },
    /// View counters cannot advance without losing identity.
    #[error("session view {counter} exhausted")]
    CounterExhausted {
        /// Counter whose checked advance failed.
        counter: &'static str,
    },
    /// A field is not an allowlisted field of the referenced event.
    #[error("event {event_id} does not expose the requested display field")]
    FieldUnavailable {
        /// Owning event, without its content.
        event_id: EventId,
    },
    /// An allowlisted structured body could not be decoded or serialized.
    #[error(
        "display body for event {event_id} is malformed ({category} at line {line}, column {column})"
    )]
    MalformedBody {
        /// Owning event identifier.
        event_id: EventId,
        /// Structural category; rejected payload text is deliberately absent.
        category: &'static str,
        /// Parser line containing the failure.
        line: usize,
        /// Parser column containing the failure.
        column: usize,
    },
    /// A known custom discriminator disagrees with the decoded lifecycle phase.
    #[error("display lifecycle phase does not match event {event_id}")]
    LifecycleMismatch {
        /// Actual owning event.
        event_id: EventId,
    },
    /// A typed live body could not be serialized, without a fabricated `EventId`.
    #[error("live display body {referent} could not be serialized: {source}")]
    LiveBodyMalformed {
        /// Actual call or lifecycle referent.
        referent: String,
        /// Serialization failure.
        #[source]
        source: serde_json::Error,
    },
    /// This field requires the store owner's approved spool reader.
    #[error("event {event_id} requires its owning spool reader")]
    SpoolRequired {
        /// Event containing the spool reference.
        event_id: EventId,
    },
    /// Provisional content changed or was invalidated since selection.
    #[error("provisional display body revision {revision} is stale or unavailable")]
    StaleBody {
        /// Exact requested revision.
        revision: u64,
    },
    /// A byte range is outside the body or splits an initial UTF-8 character.
    #[error("display body byte range begins at invalid offset {offset}")]
    InvalidRange {
        /// Requested original-content byte offset.
        offset: usize,
    },
    /// The explicit range cannot hold even its next complete character.
    #[error("display body demand {demand} cannot hold the character at {offset}")]
    RangeTooSmall {
        /// Requested original-content offset.
        offset: usize,
        /// Caller-selected byte demand.
        demand: usize,
    },
}
