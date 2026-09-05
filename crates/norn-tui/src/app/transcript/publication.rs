//! Frontend receipt ownership, exact human input associations and compact execution details.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use norn::model_selection::ModelRuntime;
use norn::provider::agent_event::{
    AgentEvent, AgentEventKind, AgentEventSender, AttemptObservation, ExecutionObservation,
    ObservationScope, PublicationResolution,
};
use norn::provider::events::{ProviderEvent, StopReason};
use norn::provider::usage::Usage;
use norn::session::events::EventId;
use norn::session::store::EventStore;
use norn::session_view::{
    AcceptedModel, AttemptKey, HistoryRecord, ItemId, LiveReduction, ViewItemKind,
};

use super::Transcript;
use crate::TuiError;

/// One actual operator submission and its retained local item, before acceptance.
pub(crate) struct SubmittedInput {
    pub text: String,
    pub local: ItemId,
}

struct ResponseCompletion {
    attempt: AttemptKey,
    item: Option<ItemId>,
    stop: StopReason,
    response_id: Option<String>,
    usage: Usage,
}

struct ActivePublication {
    owner: ExecutionObservation,
    input: Option<ItemId>,
    opening_resolved: bool,
    attempts: HashMap<AttemptKey, AttemptObservation>,
    completed: Vec<ResponseCompletion>,
    model: AcceptedModel,
}

/// Actual receipt-read result, consumed by the frontend event owner.
pub(crate) type InputRead = (ItemId, Result<HistoryRecord, TuiError>);

/// Receipt handles are retained only while unresolved; no duplicate history/body store.
#[derive(Default)]
pub(super) struct PublicationState {
    active: Option<ActivePublication>,
    hidden_completions: HashSet<ItemId>,
    compact_completions: HashSet<ItemId>,
}

impl Transcript {
    /// Scope the real producer before any live event can enter this admitted execution.
    pub(crate) fn observe_execution(
        &mut self,
        sender: &AgentEventSender,
        store: &EventStore,
        model: &ModelRuntime,
        input: Option<ItemId>,
    ) -> Result<AgentEventSender, TuiError> {
        let attempt = self.begin_execution(model)?;
        let (scoped, owner) = sender
            .observe_execution(store, self.projection.source(), attempt.execution)
            .map_err(super::super::render::interaction)?;
        self.projection.bind_execution_observation(&owner)?;
        self.publication.active = Some(ActivePublication {
            owner,
            input,
            opening_resolved: false,
            attempts: HashMap::new(),
            completed: Vec::new(),
            model: AcceptedModel::capture(model, self.configuration_revision),
        });
        Ok(scoped)
    }

    /// Borrow a coalescing wake handle without borrowing the view across `select!`.
    pub(crate) fn observation(&self) -> Option<ExecutionObservation> {
        self.publication
            .active
            .as_ref()
            .map(|active| active.owner.clone())
    }

    /// Retain only actual observed tickets; cells are inspected before queued deltas.
    pub(crate) fn observe_event(&mut self, event: &AgentEvent) -> Result<bool, TuiError> {
        let AgentEventKind::Observed(observed) = &event.event else {
            return Ok(true);
        };
        let Some(active) = &mut self.publication.active else {
            return Ok(false);
        };
        if !active
            .owner
            .same_execution(observed.scope().execution_observation())
        {
            return Ok(false);
        }
        if let ObservationScope::Attempt(ticket) = observed.scope()
            && ticket.resolution().is_none()
        {
            active
                .attempts
                .entry(ticket.attempt().clone())
                .or_insert_with(|| ticket.clone());
        }
        self.drain_publications()?;
        Ok(true)
    }

    /// Keep typed Done facts only until the final execution detail body owns them.
    pub(crate) fn note_completion(&mut self, event: &AgentEvent, reduction: &LiveReduction) {
        let Some(active) = &mut self.publication.active else {
            return;
        };
        let AgentEventKind::Observed(observed) = &event.event else {
            return;
        };
        let ObservationScope::Attempt(ticket) = observed.scope() else {
            return;
        };
        let AgentEventKind::Provider(ProviderEvent::Done {
            stop_reason,
            usage,
            response_id,
        }) = observed.event()
        else {
            return;
        };
        if !active
            .completed
            .iter()
            .any(|item| item.attempt == *ticket.attempt())
        {
            active.completed.push(ResponseCompletion {
                attempt: ticket.attempt().clone(),
                item: reduction.completion_item.clone(),
                stop: stop_reason.clone(),
                response_id: response_id.clone(),
                usage: usage.clone(),
            });
        }
    }

    /// Resolve available cells in either history/live order, without polling or FIFO inference.
    pub(crate) fn drain_publications(&mut self) -> Result<(), TuiError> {
        let Some(active) = &mut self.publication.active else {
            return Ok(());
        };
        if !active.opening_resolved
            && let Some(resolution) = active.owner.opening_input()
        {
            match resolution {
                PublicationResolution::Accepted(record) => {
                    if let Some(input) = &active.input {
                        self.projection.reconcile_input_record(input, record)?;
                    } else {
                        self.projection.apply_history_record(record)?;
                    }
                }
                PublicationResolution::NotAccepted(reason) => {
                    if active.input.is_some() {
                        self.projection.record_notice(
                            ViewItemKind::Unavailable,
                            &format!("Opening input was not accepted: {reason:?}"),
                        )?;
                    }
                }
                PublicationResolution::AcceptedButUnavailable { event_id, error } => {
                    self.projection.record_notice(
                        ViewItemKind::Unavailable,
                        &format!("Accepted opening input {event_id:?} is unavailable: {error}"),
                    )?;
                }
            }
            active.opening_resolved = true;
        }
        let mut resolved = Vec::new();
        for (key, ticket) in &active.attempts {
            if self.projection.reconcile_attempt(ticket)? {
                resolved.push(key.clone());
            }
        }
        for key in resolved {
            active.attempts.remove(&key);
        }
        Ok(())
    }

    /// Dispatch the acknowledged exact input lookup off the terminal event owner.
    pub(crate) fn read_delivered_input(
        &mut self,
        store: &Arc<EventStore>,
        item: ItemId,
        event_id: EventId,
    ) {
        let store = Arc::clone(store);
        let source = self.projection.source().clone();
        self.input_tasks.spawn_blocking(move || {
            let result = store
                .history_record(&source, &event_id)
                .map_err(TuiError::from);
            (item, result)
        });
    }

    /// Consume a completed exact input read, preserving failures and source fences.
    pub(crate) fn finish_input(
        &mut self,
        result: Result<InputRead, tokio::task::JoinError>,
    ) -> Result<(), TuiError> {
        let (item, record) = result.map_err(|source| TuiError::ViewTask {
            operation: "accepted input",
            source,
        })?;
        match record {
            Ok(record) => self.projection.reconcile_input_record(&item, &record)?,
            Err(error) => {
                self.notice(
                    ViewItemKind::Unavailable,
                    "Accepted steer display unavailable",
                    Some(&error.to_string()),
                )?;
            }
        }
        Ok(())
    }

    /// Move completed execution metadata into one lazy details body; exact routine IDs fold beneath it.
    pub(crate) fn complete_publication(
        &mut self,
        label: &str,
        elapsed: Option<Duration>,
        usage: Option<&Usage>,
        normal: bool,
    ) -> Result<ItemId, TuiError> {
        self.drain_publications()?;
        let Some(active) = self.publication.active.take() else {
            return self.notice(ViewItemKind::Notice, label, None);
        };
        let mut details = format!(
            "Outcome: {label}\nSource: {:?}\nExecution: {}\nAccepted model: {:?}\nElapsed: {elapsed:?} (whole turn)\nUsage: {usage:?}\n",
            active.owner.source(),
            active.owner.execution(),
            active.model
        );
        for completed in &active.completed {
            writeln!(details, "Response attempt: {:?}\nStop reason: {:?}\nProvider response ID: {:?}\nUsage: {:?}",
                completed.attempt, completed.stop, completed.response_id, completed.usage)
                .map_err(super::super::render::interaction)?;
        }
        if !active.attempts.is_empty() || (active.input.is_some() && !active.opening_resolved) {
            writeln!(details, "Publication coverage incomplete: {} observed attempts unresolved; opening resolved: {}",
                active.attempts.len(), active.opening_resolved).map_err(super::super::render::interaction)?;
        }
        let item = self.notice(ViewItemKind::Notice, label, Some(&details))?;
        if normal {
            self.publication
                .hidden_completions
                .extend(active.completed.into_iter().filter_map(|done| done.item));
            self.publication.compact_completions.insert(item.clone());
        }
        Ok(item)
    }

    /// Exact routine completion notices folded only after normal final outcome.
    pub(crate) fn completion_hidden(&self, item: &ItemId) -> bool {
        self.publication.hidden_completions.contains(item)
    }

    /// Final completion details start compact and remain explicitly expandable/selectable.
    pub(crate) fn completion_compact(&self, item: &ItemId) -> bool {
        self.publication.compact_completions.contains(item)
    }
}

#[cfg(test)]
#[path = "publication_tests.rs"]
mod tests;
