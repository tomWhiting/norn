//! Single publication owners and scoped sender derivation; readers cannot resolve receipt cells.

use std::sync::{Arc, OnceLock};

use tokio::sync::Notify;
use uuid::Uuid;

use super::observation::{AttemptState, ExecutionState, ResponseState};
use super::{
    AgentEventSender, AttemptObservation, ExecutionObservation, ObservationError, ObservationScope,
    PublicationEnd, PublicationResolution, ResponseObservation,
};
use crate::error::SessionError;
use crate::session::events::EventId;
use crate::session::store::EventStore;
use crate::session_view::{AttemptKey, ViewSource};

impl From<ObservationError> for SessionError {
    fn from(error: ObservationError) -> Self {
        Self::Observation(Box::new(error))
    }
}

impl AgentEventSender {
    /// Derive one immutable observed execution from the actual owning store.
    ///
    /// # Errors
    /// Refuses source/agent mismatch or reuse of an already scoped sender.
    pub fn observe_execution(
        &self,
        store: &EventStore,
        source: &ViewSource,
        execution: Uuid,
    ) -> Result<(Self, ExecutionObservation), ObservationError> {
        if let Some(scope) = &self.observation {
            return Err(ObservationError::Reused {
                execution: scope.execution(),
                operation: "scope derivation",
            });
        }
        store.history_start(source)?;
        if self.agent_id != source.agent_id {
            return Err(ObservationError::AgentMismatch {
                sender: self.agent_id,
                source_agent: source.agent_id,
            });
        }
        let observation = ExecutionObservation(Arc::new(ExecutionState {
            source: source.clone(),
            execution,
            claimed: OnceLock::new(),
            opening: OnceLock::new(),
            changed: Notify::new(),
        }));
        let mut sender = self.clone();
        sender.observation = Some(ObservationScope::Execution(observation.clone()));
        Ok((sender, observation))
    }

    pub(crate) fn claim_execution(
        &self,
        store: &EventStore,
        opening: bool,
    ) -> Result<Option<PublicationOwner>, ObservationError> {
        let Some(scope) = &self.observation else {
            return Ok(None);
        };
        let ObservationScope::Execution(execution) = scope else {
            return Err(self.scope_error("execution admission"));
        };
        store.history_start(execution.source())?;
        execution
            .0
            .claimed
            .set(())
            .map_err(|()| ObservationError::Reused {
                execution: execution.execution(),
                operation: "execution admission",
            })?;
        Ok(opening.then(|| PublicationOwner::new(PublicationTarget::Opening(execution.clone()))))
    }

    pub(crate) fn observe_response(
        &self,
        response: u64,
    ) -> Result<(Self, Option<ResponsePublicationOwner>), ObservationError> {
        let Some(scope) = &self.observation else {
            return Ok((self.clone(), None));
        };
        let ObservationScope::Execution(execution) = scope else {
            return Err(self.scope_error("response admission"));
        };
        if execution.0.claimed.get().is_none() {
            return Err(self.scope_error("response before execution admission"));
        }
        let response = ResponseObservation(Arc::new(ResponseState {
            execution: execution.clone(),
            response,
            winning: OnceLock::new(),
        }));
        let mut sender = self.clone();
        sender.observation = Some(ObservationScope::Response(response.clone()));
        Ok((
            sender,
            Some(ResponsePublicationOwner {
                response,
                armed: true,
            }),
        ))
    }

    pub(crate) fn observe_attempt(
        &self,
        attempt: u32,
    ) -> Result<(Self, Option<AttemptPublicationOwner>), ObservationError> {
        let Some(scope) = &self.observation else {
            return Ok((self.clone(), None));
        };
        let ObservationScope::Response(response) = scope else {
            return Err(self.scope_error("attempt admission"));
        };
        if attempt == 0 {
            return Err(self.scope_error("zero retry identity"));
        }
        let ticket = AttemptObservation(Arc::new(AttemptState {
            execution: response.execution_observation().clone(),
            key: AttemptKey {
                execution: response.execution(),
                response: response.response(),
                attempt,
            },
            resolution: OnceLock::new(),
        }));
        let mut sender = self.clone();
        sender.observation = Some(ObservationScope::Attempt(ticket.clone()));
        Ok((
            sender,
            Some(AttemptPublicationOwner {
                response: response.clone(),
                publication: PublicationOwner::new(PublicationTarget::Attempt(ticket)),
            }),
        ))
    }

    fn scope_error(&self, operation: &'static str) -> ObservationError {
        ObservationError::Scope {
            agent: self.agent_id,
            operation,
        }
    }
}

enum PublicationTarget {
    Opening(ExecutionObservation),
    Attempt(AttemptObservation),
}

impl PublicationTarget {
    fn execution(&self) -> &ExecutionObservation {
        match self {
            Self::Opening(value) => value,
            Self::Attempt(value) => value.execution_observation(),
        }
    }
    fn cell(&self) -> &OnceLock<PublicationResolution> {
        match self {
            Self::Opening(value) => &value.0.opening,
            Self::Attempt(value) => &value.0.resolution,
        }
    }
}

/// Unique owner transferred into the actual append closure before it may commit.
pub(crate) struct PublicationOwner {
    target: PublicationTarget,
    armed: bool,
}

impl PublicationOwner {
    fn new(target: PublicationTarget) -> Self {
        Self {
            target,
            armed: true,
        }
    }

    fn resolve(mut self, resolution: PublicationResolution) -> Result<(), ObservationError> {
        self.target.cell().set(resolution).map_err(|resolution| {
            drop(resolution);
            ObservationError::Reused {
                execution: self.target.execution().execution(),
                operation: "publication resolution",
            }
        })?;
        self.armed = false;
        self.target.execution().0.changed.notify_one();
        Ok(())
    }

    /// Complete exactly once from the result seen by the append-owning closure.
    pub(crate) fn appended(
        self,
        store: &EventStore,
        result: Result<&EventId, &SessionError>,
    ) -> Result<(), ObservationError> {
        let Ok(event_id) = result else {
            return self.resolve(PublicationResolution::NotAccepted(PublicationEnd::Failed));
        };
        match store.history_record(self.target.execution().source(), event_id) {
            Ok(record) => self.resolve(PublicationResolution::Accepted(record)),
            Err(error) => {
                let error = ObservationError::from(error);
                self.resolve(PublicationResolution::AcceptedButUnavailable {
                    event_id: event_id.clone(),
                    error: Box::new(error.clone()),
                })?;
                Err(error)
            }
        }
    }

    pub(crate) fn failed(self) -> Result<(), ObservationError> {
        self.resolve(PublicationResolution::NotAccepted(PublicationEnd::Failed))
    }
}

impl Drop for PublicationOwner {
    fn drop(&mut self) {
        if self.armed {
            self.target
                .cell()
                .get_or_init(|| PublicationResolution::NotAccepted(PublicationEnd::Abandoned));
            self.target.execution().0.changed.notify_one();
        }
    }
}

/// Attempt guard resolves failed/cancelled streams and transfers only successful assembly.
pub(crate) struct AttemptPublicationOwner {
    response: ResponseObservation,
    publication: PublicationOwner,
}

impl AttemptPublicationOwner {
    pub(crate) fn assembled(mut self) -> Result<(), ObservationError> {
        let PublicationTarget::Attempt(attempt) = &self.publication.target else {
            return Err(ObservationError::MissingAttempt {
                execution: self.response.execution(),
                response: self.response.response(),
            });
        };
        self.response
            .0
            .winning
            .set(attempt.clone())
            .map_err(|attempt| ObservationError::Reused {
                execution: attempt.execution(),
                operation: "successful attempt selection",
            })?;
        self.publication.armed = false;
        Ok(())
    }
    pub(crate) fn failed(self) -> Result<(), ObservationError> {
        self.publication.failed()
    }
}

/// Holds the one successful attempt until its append owner takes over.
pub(crate) struct ResponsePublicationOwner {
    response: ResponseObservation,
    armed: bool,
}

impl ResponsePublicationOwner {
    pub(crate) fn into_publication(mut self) -> Result<PublicationOwner, ObservationError> {
        let attempt = self
            .response
            .0
            .winning
            .get()
            .ok_or(ObservationError::MissingAttempt {
                execution: self.response.execution(),
                response: self.response.response(),
            })?
            .clone();
        self.armed = false;
        Ok(PublicationOwner::new(PublicationTarget::Attempt(attempt)))
    }
}

impl Drop for ResponsePublicationOwner {
    fn drop(&mut self) {
        if self.armed
            && let Some(attempt) = self.response.0.winning.get()
        {
            attempt
                .0
                .resolution
                .get_or_init(|| PublicationResolution::NotAccepted(PublicationEnd::Abandoned));
            attempt.execution_observation().0.changed.notify_one();
        }
    }
}
