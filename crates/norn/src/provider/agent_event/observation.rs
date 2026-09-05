//! Opaque execution and attempt receipts; single assignment and coalesced wakeups, never a queue.

use std::fmt;
use std::sync::{Arc, OnceLock};

use tokio::sync::Notify;
use uuid::Uuid;

use super::AgentEventKind;
use crate::session::events::EventId;
use crate::session::store::HistoryReadError;
use crate::session_view::{AttemptKey, HistoryRecord, ViewSource};

/// A failure to bind or publish an exact local observation.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ObservationError {
    /// The actual store refused the source or indexed event.
    #[error(transparent)]
    History(Arc<HistoryReadError>),
    /// A sender cannot represent another agent's store.
    #[error("observation sender {sender} differs from source agent {source_agent}")]
    AgentMismatch {
        /// Actual sender.
        sender: Uuid,
        /// Actual source agent.
        source_agent: Uuid,
    },
    /// An execution scope was reused or rebound.
    #[error("observation execution {execution} cannot perform {operation} twice")]
    Reused {
        /// Actual local execution.
        execution: Uuid,
        /// Refused ownership operation.
        operation: &'static str,
    },
    /// An operation requires a different immutable scope.
    #[error("observation sender {agent} cannot perform {operation} in its current scope")]
    Scope {
        /// Actual sender.
        agent: Uuid,
        /// Refused operation.
        operation: &'static str,
    },
    /// Identity counters must not wrap or saturate.
    #[error("{counter} identity counter is exhausted for observed agent {agent:?}")]
    CounterExhausted {
        /// Counter being advanced.
        counter: &'static str,
        /// Actual sender when observed.
        agent: Option<Uuid>,
    },
    /// A response cannot publish without its successful attempt.
    #[error("execution {execution} response {response} has no successful observed attempt")]
    MissingAttempt {
        /// Actual local execution.
        execution: Uuid,
        /// Actual response index.
        response: u64,
    },
    /// A nested envelope would obscure its actual provenance.
    #[error("execution {execution} cannot wrap an already observed event")]
    Nested {
        /// Actual local execution.
        execution: Uuid,
    },
}

impl From<HistoryReadError> for ObservationError {
    fn from(error: HistoryReadError) -> Self {
        Self::History(Arc::new(error))
    }
}

/// Why an observed attempt never acquired accepted publication authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationEnd {
    /// The producer returned a failure before acceptance.
    Failed,
    /// Cancellation or an early return dropped the publication owner.
    Abandoned,
}

/// One immutable outcome, completed by the actual append owner.
#[derive(Debug)]
pub enum PublicationResolution {
    /// Compact record minted by the actual store after acceptance.
    Accepted(HistoryRecord),
    /// No accepted publication was reported by this owner.
    NotAccepted(PublicationEnd),
    /// Acceptance happened, but its display record could not be projected.
    AcceptedButUnavailable {
        /// The exact accepted event, never a guessed replacement.
        event_id: EventId,
        /// Typed failure without copying the original event body.
        error: Box<ObservationError>,
    },
}

pub(super) struct ExecutionState {
    pub(super) source: ViewSource,
    pub(super) execution: Uuid,
    pub(super) claimed: OnceLock<()>,
    pub(super) opening: OnceLock<PublicationResolution>,
    pub(super) changed: Notify,
}

/// Read handle for one actual store-bound local execution.
#[derive(Clone)]
pub struct ExecutionObservation(pub(super) Arc<ExecutionState>);

impl ExecutionObservation {
    /// Source validated against the store at admission.
    #[must_use]
    pub fn source(&self) -> &ViewSource {
        &self.0.source
    }
    /// Caller-owned admission UUID.
    #[must_use]
    pub fn execution(&self) -> Uuid {
        self.0.execution
    }
    /// Compare the actual opaque owner, rather than inspectable identifiers alone.
    #[must_use]
    pub fn same_execution(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// The opening user-message outcome, if this execution has resolved one.
    /// Human-input provenance belongs to the caller's explicit local input binding.
    #[must_use]
    pub fn opening_input(&self) -> Option<&PublicationResolution> {
        self.0.opening.get()
    }
    /// Wait for a coalesced change. Outcomes remain in their cells even if wakes merge.
    pub async fn changed(&self) {
        self.0.changed.notified().await;
    }
}

impl fmt::Debug for ExecutionObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionObservation")
            .field("source", self.source())
            .field("execution", &self.execution())
            .finish_non_exhaustive()
    }
}

pub(super) struct ResponseState {
    pub(super) execution: ExecutionObservation,
    pub(super) response: u64,
    pub(super) winning: OnceLock<AttemptObservation>,
}

/// Immutable producer-owned response scope, including retry notices.
#[derive(Clone)]
pub struct ResponseObservation(pub(super) Arc<ResponseState>);

impl ResponseObservation {
    /// Actual admitted execution owner.
    #[must_use]
    pub fn execution_observation(&self) -> &ExecutionObservation {
        &self.0.execution
    }
    /// Store source for this response.
    #[must_use]
    pub fn source(&self) -> &ViewSource {
        self.0.execution.source()
    }
    /// Local execution UUID.
    #[must_use]
    pub fn execution(&self) -> Uuid {
        self.0.execution.execution()
    }
    /// Actual zero-based request index supplied by the producer.
    #[must_use]
    pub fn response(&self) -> u64 {
        self.0.response
    }
}

impl fmt::Debug for ResponseObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseObservation")
            .field("execution", self.execution_observation())
            .field("response", &self.response())
            .finish_non_exhaustive()
    }
}

pub(super) struct AttemptState {
    pub(super) execution: ExecutionObservation,
    pub(super) key: AttemptKey,
    pub(super) resolution: OnceLock<PublicationResolution>,
}

/// Read-only ticket carried by every streamed event of one actual attempt.
#[derive(Clone)]
pub struct AttemptObservation(pub(super) Arc<AttemptState>);

impl AttemptObservation {
    /// Actual admitted execution owner.
    #[must_use]
    pub fn execution_observation(&self) -> &ExecutionObservation {
        &self.0.execution
    }
    /// Store source for this attempt.
    #[must_use]
    pub fn source(&self) -> &ViewSource {
        self.0.execution.source()
    }
    /// Local execution UUID.
    #[must_use]
    pub fn execution(&self) -> Uuid {
        self.0.execution.execution()
    }
    /// Actual response and retry identity, independent of received event order.
    #[must_use]
    pub fn attempt(&self) -> &AttemptKey {
        &self.0.key
    }
    /// Immutable acceptance or abandonment outcome.
    #[must_use]
    pub fn resolution(&self) -> Option<&PublicationResolution> {
        self.0.resolution.get()
    }
}

impl fmt::Debug for AttemptObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttemptObservation")
            .field("source", self.source())
            .field("attempt", self.attempt())
            .field("resolved", &self.resolution().is_some())
            .finish()
    }
}

/// Exact provenance for an event emitted by an observed execution.
#[derive(Clone, Debug)]
pub enum ObservationScope {
    /// Auxiliary events emitted outside a provider stream.
    Execution(ExecutionObservation),
    /// Request-scoped retry/progress information.
    Response(ResponseObservation),
    /// Stream data belonging to one actual retry.
    Attempt(AttemptObservation),
}

impl ObservationScope {
    /// Shared opaque owner of every scope in this execution.
    #[must_use]
    pub fn execution_observation(&self) -> &ExecutionObservation {
        match self {
            Self::Execution(value) => value,
            Self::Response(value) => value.execution_observation(),
            Self::Attempt(value) => value.execution_observation(),
        }
    }
    /// Actual validated source.
    #[must_use]
    pub fn source(&self) -> &ViewSource {
        self.execution_observation().source()
    }
    /// Local execution UUID.
    #[must_use]
    pub fn execution(&self) -> Uuid {
        self.execution_observation().execution()
    }
}

/// A native event plus opaque producer provenance; its payload is moved, not cloned.
#[derive(Clone)]
pub struct ObservedAgentEvent {
    scope: ObservationScope,
    event: Box<AgentEventKind>,
}

impl ObservedAgentEvent {
    pub(super) fn new(
        scope: ObservationScope,
        event: AgentEventKind,
    ) -> Result<Self, ObservationError> {
        if matches!(event, AgentEventKind::Observed(_)) {
            return Err(ObservationError::Nested {
                execution: scope.execution(),
            });
        }
        Ok(Self {
            scope,
            event: Box::new(event),
        })
    }
    /// Exact immutable event owner.
    #[must_use]
    pub const fn scope(&self) -> &ObservationScope {
        &self.scope
    }
    /// Original native payload, without a clone.
    #[must_use]
    pub fn event(&self) -> &AgentEventKind {
        &self.event
    }
    /// Consume the envelope while preserving its exact scope and original payload.
    #[must_use]
    pub fn into_parts(self) -> (ObservationScope, AgentEventKind) {
        (self.scope, *self.event)
    }
}

impl fmt::Debug for ObservedAgentEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObservedAgentEvent")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}
