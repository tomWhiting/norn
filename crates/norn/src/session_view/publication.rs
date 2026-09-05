//! Producer-owned publication reconciliation, scoped admission and exact late-event fences.

use std::collections::{HashMap, HashSet};

use crate::provider::agent_event::{
    AgentEventKind, AttemptObservation, ExecutionObservation, ObservationScope, ObservedAgentEvent,
    PublicationResolution,
};
use crate::provider::events::ProviderEvent;
use crate::session::events::EventId;

use super::contract::{AttemptKey, CoverageGap, HistoryPosition, ItemId, ViewItemKind};
use super::error::ViewError;
use super::projection::SessionProjection;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Retirement {
    Accepted(EventId),
    NotAccepted,
}

pub(super) struct PublicationState {
    pub scope: Option<ExecutionObservation>,
    latest: Option<AttemptKey>,
    retired: HashMap<AttemptKey, Retirement>,
    lagged: HashSet<AttemptKey>,
    ended: HashSet<AttemptKey>,
    pub input_owners: HashMap<EventId, ItemId>,
}

impl PublicationState {
    pub fn new() -> Self {
        Self {
            scope: None,
            latest: None,
            retired: HashMap::new(),
            lagged: HashSet::new(),
            ended: HashSet::new(),
            input_owners: HashMap::new(),
        }
    }

    pub fn reset_execution(&mut self) {
        self.scope = None;
        self.latest = None;
        self.retired.clear();
        self.lagged.clear();
        self.ended.clear();
    }

    pub fn mark_lagged(&mut self) {
        if let Some(attempt) = &self.latest {
            self.lagged.insert(attempt.clone());
        }
    }
}

impl SessionProjection {
    /// Bind the opaque producer owner before admitting any scoped live envelope.
    /// Matching UUIDs alone cannot rebind a different producer instance.
    pub fn bind_execution_observation(
        &mut self,
        observation: &ExecutionObservation,
    ) -> Result<(), ViewError> {
        self.validate_observation_source(observation)?;
        if self
            .publication
            .scope
            .as_ref()
            .is_some_and(|owner| !owner.same_execution(observation))
        {
            return Err(self.observation_mismatch(observation));
        }
        self.publication.scope = Some(observation.clone());
        Ok(())
    }

    /// Inspect one immutable ticket, returning false only while its owner is pending.
    /// A resolved ticket retires only its own provisional attempt. Entirely missed
    /// attempts apply ordinary committed history without inventing a live alias.
    pub fn reconcile_attempt(&mut self, ticket: &AttemptObservation) -> Result<bool, ViewError> {
        self.validate_observation(ticket.execution_observation())?;
        let Some(resolution) = ticket.resolution() else {
            return Ok(false);
        };
        let attempt = ticket.attempt();
        let retirement = match resolution {
            PublicationResolution::Accepted(record) => {
                let HistoryPosition::Event { event_id, .. } = record.cursor().position() else {
                    return Err(ViewError::AttemptMismatch);
                };
                if !record.assistant {
                    return Err(ViewError::PublicationConflict {
                        attempt: Box::new(attempt.clone()),
                        event_id: event_id.clone(),
                    });
                }
                Retirement::Accepted(event_id.clone())
            }
            PublicationResolution::NotAccepted(_) => Retirement::NotAccepted,
            PublicationResolution::AcceptedButUnavailable { event_id, .. } => {
                Retirement::Accepted(event_id.clone())
            }
        };
        if let Some(previous) = self.publication.retired.get(attempt) {
            if previous == &retirement {
                return Ok(true);
            }
            let event_id = match (&retirement, previous) {
                (Retirement::Accepted(id), _) | (_, Retirement::Accepted(id)) => id,
                (Retirement::NotAccepted, Retirement::NotAccepted) => {
                    return Ok(true);
                }
            };
            return Err(ViewError::PublicationConflict {
                attempt: Box::new(attempt.clone()),
                event_id: event_id.clone(),
            });
        }
        match resolution {
            PublicationResolution::Accepted(record) => {
                if self.items.attempt_ids(attempt).is_empty() {
                    self.apply_history_record(record)?;
                    self.items
                        .place_completions_after(attempt, record.cursor())?;
                } else {
                    self.reconcile_history_record(attempt, record)?;
                }
            }
            PublicationResolution::NotAccepted(reason) => {
                self.items.forget_completions(attempt);
                self.invalidate_attempt(attempt, true);
                self.coverage.gaps.insert(CoverageGap::Interrupted);
                self.record_notice(
                    ViewItemKind::Notice,
                    &format!(
                        "Response {} attempt {} was not accepted ({reason:?})",
                        attempt.response, attempt.attempt
                    ),
                )?;
            }
            PublicationResolution::AcceptedButUnavailable { event_id, error } => {
                self.items.forget_completions(attempt);
                self.invalidate_attempt(attempt, false);
                self.coverage
                    .gaps
                    .insert(CoverageGap::IncompleteAssociation);
                self.record_notice(
                    ViewItemKind::Unavailable,
                    &format!("Accepted event {event_id} is unavailable: {error}"),
                )?;
            }
        }
        self.publication.retired.insert(attempt.clone(), retirement);
        self.publication.lagged.remove(attempt);
        self.publication.ended.remove(attempt);
        Ok(true)
    }

    pub(super) fn validate_live_envelope(&self, event: &AgentEventKind) -> Result<(), ViewError> {
        if let Some(scope) = &self.publication.scope
            && matches!(
                event,
                AgentEventKind::Provider(_) | AgentEventKind::StreamRetry(_)
            )
        {
            return Err(ViewError::ObservationRequired {
                execution: scope.execution(),
            });
        }
        Ok(())
    }

    pub(super) fn reduce_observed(
        &mut self,
        observed: &ObservedAgentEvent,
    ) -> Result<(Option<AttemptKey>, bool), ViewError> {
        let scope = observed.scope();
        self.validate_observation(scope.execution_observation())?;
        if matches!(observed.event(), AgentEventKind::Observed(_)) {
            return Err(ViewError::ObservationRequired {
                execution: scope.execution(),
            });
        }
        match scope {
            ObservationScope::Attempt(ticket) => {
                if self.reconcile_attempt(ticket)?
                    || self.publication.lagged.contains(ticket.attempt())
                    || self.publication.ended.contains(ticket.attempt())
                    || !self.install_attempt(ticket.attempt())?
                {
                    return Ok((None, true));
                }
                let result = self.reduce_live(observed.event())?;
                if result.0.is_some() {
                    self.publication.ended.insert(ticket.attempt().clone());
                }
                Ok(result)
            }
            ObservationScope::Response(response) => {
                if self
                    .publication
                    .latest
                    .as_ref()
                    .is_some_and(|latest| response.response() < latest.response)
                {
                    return Ok((None, true));
                }
                if let AgentEventKind::StreamRetry(retry) = observed.event() {
                    self.record_notice(
                        ViewItemKind::Notice,
                        &format!(
                            "Response {} retry attempt {} in {} ms ({})",
                            response.response(),
                            retry.attempt,
                            retry.delay_ms,
                            retry.error_class
                        ),
                    )?;
                    return Ok((None, true));
                }
                self.reduce_auxiliary(observed.event(), scope.execution())
            }
            ObservationScope::Execution(execution) => {
                self.reduce_auxiliary(observed.event(), execution.execution())
            }
        }
    }

    fn reduce_auxiliary(
        &mut self,
        event: &AgentEventKind,
        execution: uuid::Uuid,
    ) -> Result<(Option<AttemptKey>, bool), ViewError> {
        if let AgentEventKind::Provider(provider) = event {
            match provider {
                ProviderEvent::ToolResult { .. }
                | ProviderEvent::Error { .. }
                | ProviderEvent::ResponseStreamEvent { .. }
                | ProviderEvent::ResponseAudioFrame { .. }
                | ProviderEvent::Compaction { .. } => {}
                ProviderEvent::TextDelta { .. }
                | ProviderEvent::RefusalDelta { .. }
                | ProviderEvent::RefusalComplete { .. }
                | ProviderEvent::ThinkingDelta { .. }
                | ProviderEvent::ToolCallDelta { .. }
                | ProviderEvent::TextComplete { .. }
                | ProviderEvent::ThinkingComplete { .. }
                | ProviderEvent::ReasoningItemDone { .. }
                | ProviderEvent::ResponseItemDone { .. }
                | ProviderEvent::ToolCallComplete { .. }
                | ProviderEvent::Done { .. } => {
                    return Err(ViewError::ObservationRequired { execution });
                }
            }
        }
        if matches!(event, AgentEventKind::StreamRetry(_)) {
            return Err(ViewError::ObservationRequired { execution });
        }
        self.reduce_live(event)
    }

    fn install_attempt(&mut self, attempt: &AttemptKey) -> Result<bool, ViewError> {
        if attempt.attempt == 0 {
            return Err(ViewError::AttemptMismatch);
        }
        if self.publication.latest.as_ref().is_some_and(|latest| {
            (attempt.response, attempt.attempt) < (latest.response, latest.attempt)
        }) {
            return Ok(false);
        }
        let execution = self.execution.as_mut().ok_or(ViewError::NoExecution)?;
        if &execution.attempt != attempt {
            execution.attempt = attempt.clone();
            execution.completed_text = false;
            execution.completed_thinking = false;
            execution.generic_text.clear();
            execution.generic_thinking.clear();
            execution.segment = 0;
            execution.last_segment = None;
        }
        self.publication.latest = Some(attempt.clone());
        Ok(true)
    }

    fn validate_observation_source(
        &self,
        observation: &ExecutionObservation,
    ) -> Result<(), ViewError> {
        if observation.source() != &self.source {
            return Err(ViewError::SourceMismatch {
                expected: Box::new(self.source.clone()),
                actual: Box::new(observation.source().clone()),
            });
        }
        if self
            .execution
            .as_ref()
            .map(|current| current.attempt.execution)
            != Some(observation.execution())
        {
            return Err(self.observation_mismatch(observation));
        }
        Ok(())
    }

    fn validate_observation(&self, observation: &ExecutionObservation) -> Result<(), ViewError> {
        self.validate_observation_source(observation)?;
        if !self
            .publication
            .scope
            .as_ref()
            .is_some_and(|owner| owner.same_execution(observation))
        {
            return Err(self.observation_mismatch(observation));
        }
        Ok(())
    }

    fn observation_mismatch(&self, observation: &ExecutionObservation) -> ViewError {
        ViewError::ExecutionObservationMismatch {
            expected: self
                .execution
                .as_ref()
                .map(|current| current.attempt.execution),
            actual: observation.execution(),
        }
    }
}
