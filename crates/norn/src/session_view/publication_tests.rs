//! Exact producer receipts, human input retirement and late scoped event regressions.

use std::num::NonZeroUsize;
use std::sync::Arc;

use tokio::sync::broadcast;
use uuid::Uuid;

use super::contract_tests::{TestResult, model};
use super::{
    BodyOrigin, BodyRange, BodyRepresentation, CoverageGap, HistoryRecord, ItemId,
    SessionProjection, ViewError, ViewItemKind, ViewSource,
};
use crate::provider::agent_event::{
    AgentEvent, AgentEventKind, AgentEventSender, AttemptObservation, ExecutionObservation,
    ObservationScope, PublicationResolution,
};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::usage::Usage;
use crate::session::branch::{SessionBinding, SessionBrancher};
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::manager::{CreateSessionOptions, SessionManager};
use crate::session::store::{DurabilityPolicy, EventStore};

type TestError = Box<dyn std::error::Error>;

struct Fixture {
    store: EventStore,
    source: ViewSource,
    view: SessionProjection,
    root: AgentEventSender,
    receiver: broadcast::Receiver<AgentEvent>,
}

impl Fixture {
    fn new() -> Result<Self, TestError> {
        let store = EventStore::new();
        let agent = Uuid::new_v4();
        let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
        let (sender, receiver) = broadcast::channel(16);
        Ok(Self {
            view: SessionProjection::new(source.clone()),
            root: AgentEventSender::new(sender, agent, "fixture".to_owned()),
            store,
            source,
            receiver,
        })
    }

    fn begin(&mut self) -> Result<(AgentEventSender, ExecutionObservation), TestError> {
        let execution = Uuid::new_v4();
        self.view.begin_execution(execution, model()?)?;
        let (sender, observation) =
            self.root
                .observe_execution(&self.store, &self.source, execution)?;
        assert!(sender.claim_execution(&self.store, false)?.is_none());
        self.view.bind_execution_observation(&observation)?;
        Ok((sender, observation))
    }

    fn input(&self, text: &str) -> Result<HistoryRecord, TestError> {
        let id = self.store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: text.to_owned(),
        })?;
        Ok(self.store.history_record(&self.source, &id)?)
    }

    fn local_input(&mut self, text: &str) -> Result<ItemId, TestError> {
        Ok(self.view.record_local_body(
            ViewItemKind::Input,
            "You",
            text,
            BodyRepresentation::Text,
        )?)
    }

    fn event(
        &mut self,
        sender: &AgentEventSender,
        event: ProviderEvent,
    ) -> Result<AgentEvent, TestError> {
        sender.send(event);
        Ok(self.receiver.try_recv()?)
    }

    fn text_event(&mut self, sender: &AgentEventSender) -> Result<AgentEvent, TestError> {
        self.event(
            sender,
            ProviderEvent::TextDelta {
                text: "answer".to_owned(),
            },
        )
    }

    fn text_count(&self) -> usize {
        self.view
            .items()
            .filter(|row| matches!(row.kind, ViewItemKind::Text))
            .count()
    }
}

fn event_ticket(event: &AgentEvent) -> Result<AttemptObservation, TestError> {
    let AgentEventKind::Observed(observed) = &event.event else {
        return Err("fixture event is not observed".into());
    };
    let ObservationScope::Attempt(ticket) = observed.scope() else {
        return Err("fixture event has no attempt ticket".into());
    };
    Ok(ticket.clone())
}

fn answer() -> SessionEvent {
    SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: "answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    }
}

#[test]
fn human_receipt_uses_one_canonical_body_in_both_history_orders() -> TestResult {
    for history_first in [false, true] {
        let mut fixture = Fixture::new()?;
        let local = fixture.local_input("same words")?;
        let old_body = fixture
            .view
            .item(&local)
            .ok_or("missing local row")?
            .bodies
            .first()
            .ok_or("missing local body")?
            .clone();
        let record = fixture.input("same words")?;
        let canonical = record.items().first().ok_or("missing canonical row")?;
        if history_first {
            fixture.view.apply_history_record(&record)?;
            assert_eq!(fixture.view.items().len(), 2);
        }
        fixture.view.reconcile_input_record(&local, &record)?;
        fixture.view.apply_history_record(&record)?;
        fixture.view.reconcile_input_record(&local, &record)?;
        assert_eq!(fixture.view.items().len(), 1);
        assert!(fixture.view.item(&local).is_none());
        assert_eq!(fixture.view.alias(&local), Some(&canonical.id));
        let retained = fixture
            .view
            .item(&canonical.id)
            .ok_or("missing accepted row")?;
        assert_eq!(retained.label.as_str(), "You");
        assert_eq!(retained.bodies, canonical.bodies);
        assert_eq!(retained.bodies.len(), 1);
        assert!(matches!(
            retained.bodies[0].origin(),
            BodyOrigin::Committed { .. }
        ));
        assert!(matches!(
            fixture.view.read_provisional(
                &old_body,
                BodyRange {
                    offset: 0,
                    max_bytes: NonZeroUsize::new(32).ok_or("zero fixture range")?,
                }
            ),
            Err(ViewError::StaleBody { .. })
        ));
    }
    Ok(())
}

#[test]
fn identical_human_inputs_are_distinct_and_conflicting_receipts_do_not_rebind() -> TestResult {
    let mut fixture = Fixture::new()?;
    let first = fixture.local_input("identical")?;
    let second = fixture.local_input("identical")?;
    let first_record = fixture.input("identical")?;
    let second_record = fixture.input("identical")?;
    fixture.view.reconcile_input_record(&first, &first_record)?;
    assert!(matches!(
        fixture.view.reconcile_input_record(&second, &first_record),
        Err(ViewError::InputAssociation { .. })
    ));
    assert!(matches!(
        fixture.view.reconcile_input_record(&first, &second_record),
        Err(ViewError::InputAssociation { .. })
    ));
    fixture
        .view
        .reconcile_input_record(&second, &second_record)?;
    assert_eq!(fixture.view.items().len(), 2);
    assert_ne!(fixture.view.alias(&first), fixture.view.alias(&second));
    Ok(())
}

#[test]
fn human_receipts_refuse_foreign_sources_and_non_input_records() -> TestResult {
    let mut fixture = Fixture::new()?;
    let local = fixture.local_input("input")?;
    let foreign = Fixture::new()?.input("input")?;
    assert!(matches!(
        fixture.view.reconcile_input_record(&local, &foreign),
        Err(ViewError::SourceMismatch { .. })
    ));
    let id = fixture.store.append(answer())?;
    let record = fixture.store.history_record(&fixture.source, &id)?;
    assert!(matches!(
        fixture.view.reconcile_input_record(&local, &record),
        Err(ViewError::InputAssociation { .. })
    ));
    let notice = fixture
        .view
        .record_notice(ViewItemKind::Notice, "not submitted")?;
    let input = fixture.input("input")?;
    assert!(matches!(
        fixture.view.reconcile_input_record(&notice, &input),
        Err(ViewError::InputAssociation { .. })
    ));
    assert!(fixture.view.item(&local).is_some());
    assert_eq!(fixture.view.items().len(), 2);
    Ok(())
}

#[test]
fn assistant_receipt_reconciles_both_history_orders_and_fences_buffered_events() -> TestResult {
    for history_first in [false, true] {
        let mut fixture = Fixture::new()?;
        let (execution, observation) = fixture.begin()?;
        let (response, publication) = execution.observe_response(4)?;
        let (attempt, owner) = response.observe_attempt(3)?;
        let event = fixture.text_event(&attempt)?;
        let ticket = event_ticket(&event)?;
        fixture.view.apply_live(&event)?;
        assert_eq!(fixture.view.current_attempt(), Some(ticket.attempt()));
        assert_eq!(fixture.text_count(), 1);
        let id = fixture.store.append(answer())?;
        let record = fixture.store.history_record(&fixture.source, &id)?;
        if history_first {
            fixture.view.apply_history_record(&record)?;
            assert_eq!(fixture.text_count(), 2);
        }
        owner.ok_or("missing attempt owner")?.assembled()?;
        publication
            .ok_or("missing response owner")?
            .into_publication()?
            .appended(&fixture.store, Ok(&id))?;
        assert!(fixture.view.reconcile_attempt(&ticket)?);
        fixture.view.apply_history_record(&record)?;
        assert!(fixture.view.reconcile_attempt(&ticket)?);
        assert_eq!(fixture.text_count(), 1);
        assert!(fixture.view.apply_live(&event)?.metadata_only);
        assert_eq!(fixture.text_count(), 1);
        assert_eq!(observation.execution(), ticket.execution());
    }
    Ok(())
}

#[test]
fn publication_before_first_buffered_event_does_not_create_provisional_rows_or_gap() -> TestResult {
    let mut fixture = Fixture::new()?;
    let (execution, observation) = fixture.begin()?;
    let (response, publication) = execution.observe_response(0)?;
    let (attempt, owner) = response.observe_attempt(1)?;
    let event = fixture.text_event(&attempt)?;
    owner.ok_or("missing attempt owner")?.assembled()?;
    let id = fixture.store.append(answer())?;
    publication
        .ok_or("missing response owner")?
        .into_publication()?
        .appended(&fixture.store, Ok(&id))?;
    assert!(fixture.view.apply_live(&event)?.metadata_only);
    assert_eq!(fixture.text_count(), 1);
    assert!(
        fixture
            .view
            .items()
            .all(|row| matches!(row.id, ItemId::Committed { .. }))
    );
    assert!(
        !fixture
            .view
            .coverage()
            .gaps
            .contains(&CoverageGap::IncompleteAssociation)
    );
    assert!(observation.opening_input().is_none());
    Ok(())
}

#[test]
fn lag_fences_remaining_fragments_but_receipt_still_reconciles_and_next_attempt_is_exact()
-> TestResult {
    let mut fixture = Fixture::new()?;
    let (execution, observation) = fixture.begin()?;
    let (response, publication) = execution.observe_response(7)?;
    let (attempt, owner) = response.observe_attempt(2)?;
    let event = fixture.text_event(&attempt)?;
    let ticket = event_ticket(&event)?;
    fixture.view.apply_live(&event)?;
    let body = fixture
        .view
        .items()
        .find(|row| matches!(row.kind, ViewItemKind::Text))
        .ok_or("missing live text")?
        .bodies[0]
        .clone();
    fixture.view.mark_lagged(5)?;
    assert!(fixture.view.apply_live(&event)?.metadata_only);
    assert_eq!(
        fixture
            .view
            .items()
            .find(|row| matches!(row.kind, ViewItemKind::Text))
            .ok_or("missing live text")?
            .bodies[0],
        body
    );
    owner.ok_or("missing attempt owner")?.assembled()?;
    let id = fixture.store.append(answer())?;
    publication
        .ok_or("missing response owner")?
        .into_publication()?
        .appended(&fixture.store, Ok(&id))?;
    assert!(fixture.view.reconcile_attempt(&ticket)?);
    assert_eq!(fixture.text_count(), 1);
    let (next_response, next_publication) = execution.observe_response(8)?;
    let (next_attempt, next_owner) = next_response.observe_attempt(4)?;
    let next_event = fixture.text_event(&next_attempt)?;
    fixture.view.apply_live(&next_event)?;
    assert_eq!(
        fixture.view.current_attempt(),
        Some(event_ticket(&next_event)?.attempt())
    );
    assert_eq!(fixture.text_count(), 2);
    assert_eq!(fixture.view.coverage().missed_live_events, 5);
    assert_eq!(
        observation.execution(),
        next_attempt_execution(&next_event)?
    );
    drop(next_owner);
    drop(next_publication);
    Ok(())
}

fn next_attempt_execution(event: &AgentEvent) -> Result<Uuid, TestError> {
    Ok(event_ticket(event)?.execution())
}

#[test]
fn failed_and_abandoned_tickets_retire_only_their_attempt_and_fence_late_events() -> TestResult {
    for fail in [false, true] {
        let mut fixture = Fixture::new()?;
        let (execution, observation) = fixture.begin()?;
        let (response, publication) = execution.observe_response(0)?;
        let (attempt, owner) = response.observe_attempt(1)?;
        let event = fixture.text_event(&attempt)?;
        let ticket = event_ticket(&event)?;
        fixture.view.apply_live(&event)?;
        let owner = owner.ok_or("missing attempt owner")?;
        if fail {
            owner.failed()?;
        } else {
            drop(owner);
        }
        assert!(fixture.view.reconcile_attempt(&ticket)?);
        assert_eq!(fixture.text_count(), 0);
        assert!(fixture.view.apply_live(&event)?.metadata_only);
        let (retry, retry_owner) = response.observe_attempt(6)?;
        let retry_event = fixture.text_event(&retry)?;
        fixture.view.apply_live(&retry_event)?;
        assert_eq!(fixture.text_count(), 1);
        assert_eq!(
            fixture.view.current_attempt(),
            Some(event_ticket(&retry_event)?.attempt())
        );
        assert!(fixture.view.reconcile_attempt(&ticket)?);
        assert_eq!(fixture.text_count(), 1);
        assert_eq!(observation.execution(), ticket.execution());
        drop(retry_owner);
        drop(publication);
    }
    Ok(())
}

#[test]
fn opaque_execution_owner_cannot_be_rebound_by_uuid_or_late_events() -> TestResult {
    let mut fixture = Fixture::new()?;
    let (execution, observation) = fixture.begin()?;
    let (other_sender, other) =
        fixture
            .root
            .observe_execution(&fixture.store, &fixture.source, observation.execution())?;
    assert!(matches!(
        fixture.view.bind_execution_observation(&other),
        Err(ViewError::ExecutionObservationMismatch { .. })
    ));
    let old = fixture.event(
        &execution,
        ProviderEvent::Compaction {
            item_type: "compaction".to_owned(),
            encrypted_content: None,
        },
    )?;
    fixture.view.end_execution(false)?;
    assert!(matches!(
        fixture.view.apply_live(&old),
        Err(ViewError::ExecutionObservationMismatch { .. })
    ));
    let (next, next_observation) = fixture.begin()?;
    assert!(matches!(
        fixture.view.apply_live(&old),
        Err(ViewError::ExecutionObservationMismatch { .. })
    ));
    assert_eq!(
        fixture
            .view
            .current_attempt()
            .ok_or("missing next attempt")?
            .execution,
        next_observation.execution()
    );
    let unscoped = fixture.event(
        &fixture.root.clone(),
        ProviderEvent::TextDelta {
            text: "raw".to_owned(),
        },
    )?;
    assert!(matches!(
        fixture.view.apply_live(&unscoped),
        Err(ViewError::ObservationRequired { .. })
    ));
    let auxiliary_text = fixture.text_event(&next)?;
    assert!(matches!(
        fixture.view.apply_live(&auxiliary_text),
        Err(ViewError::ObservationRequired { .. })
    ));
    let foreign = Fixture::new()?;
    let (foreign_sender, foreign_owner) = foreign.root.observe_execution(
        &foreign.store,
        &foreign.source,
        next_observation.execution(),
    )?;
    assert!(matches!(
        fixture.view.bind_execution_observation(&foreign_owner),
        Err(ViewError::SourceMismatch { .. })
    ));
    drop(foreign_sender);
    drop(other_sender);
    Ok(())
}

#[test]
fn done_reports_exact_notice_and_preserves_actual_attempt_without_recreating_late_text()
-> TestResult {
    let mut fixture = Fixture::new()?;
    let (execution, observation) = fixture.begin()?;
    let (response, publication) = execution.observe_response(9)?;
    let (attempt, owner) = response.observe_attempt(4)?;
    let text = fixture.text_event(&attempt)?;
    let ticket = event_ticket(&text)?;
    fixture.view.apply_live(&text)?;
    let done = fixture.event(
        &attempt,
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: Some("actual-response".to_owned()),
        },
    )?;
    let reduction = fixture.view.apply_live(&done)?;
    let completion = reduction
        .completion_item
        .ok_or("missing exact Done notice")?;
    assert!(matches!(
        fixture
            .view
            .item(&completion)
            .ok_or("missing completion row")?
            .kind,
        ViewItemKind::Metadata
    ));
    assert_eq!(reduction.completed_attempt.as_ref(), Some(ticket.attempt()));
    assert_eq!(fixture.view.current_attempt(), Some(ticket.attempt()));
    let late = fixture.view.apply_live(&text)?;
    assert!(late.metadata_only);
    assert!(late.completion_item.is_none());
    assert_eq!(fixture.text_count(), 1);
    assert_eq!(observation.execution(), ticket.execution());
    drop(owner);
    drop(publication);
    Ok(())
}

#[test]
fn accepted_event_with_replaced_managed_owner_is_unavailable_not_unaccepted() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let options = || CreateSessionOptions {
        model: "fixture".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    };
    let original_session =
        manager.create_with_id("publication-owner", options(), DurabilityPolicy::Flush)?;
    let binding = SessionBinding::persistent_root(
        Arc::new(SessionBrancher::new(
            manager.clone(),
            "publication-owner".to_owned(),
            DurabilityPolicy::Flush,
        )),
        &original_session.entry,
        &[],
    );
    let agent = Uuid::new_v4();
    let source = original_session
        .store
        .bind_view_source(&binding, agent, None)?;
    let mut view = SessionProjection::new(source.clone());
    let execution = Uuid::new_v4();
    view.begin_execution(execution, model()?)?;
    let (sender, mut receiver) = broadcast::channel(1);
    let root = AgentEventSender::new(sender, agent, "fixture".to_owned());
    let (sender, observation) =
        root.observe_execution(&original_session.store, &source, execution)?;
    assert!(
        sender
            .claim_execution(&original_session.store, false)?
            .is_none()
    );
    view.bind_execution_observation(&observation)?;
    let (response, publication) = sender.observe_response(0)?;
    let (attempt, owner) = response.observe_attempt(1)?;
    attempt.send(ProviderEvent::TextDelta {
        text: "answer".to_owned(),
    });
    let event = receiver.try_recv()?;
    let ticket = event_ticket(&event)?;
    view.apply_live(&event)?;
    owner.ok_or("missing attempt owner")?.assembled()?;
    let id = original_session.store.append(answer())?;
    manager.delete("publication-owner")?;
    let replacement =
        manager.create_with_id("publication-owner", options(), DurabilityPolicy::Flush)?;
    assert_ne!(
        original_session.entry.generation,
        replacement.entry.generation
    );
    assert!(
        publication
            .ok_or("missing response owner")?
            .into_publication()?
            .appended(&original_session.store, Ok(&id))
            .is_err()
    );
    assert!(
        matches!(ticket.resolution(), Some(PublicationResolution::AcceptedButUnavailable { event_id, .. }) if event_id == &id)
    );
    assert!(view.reconcile_attempt(&ticket)?);
    assert!(
        !view
            .items()
            .any(|row| matches!(row.kind, ViewItemKind::Text))
    );
    assert!(
        view.items()
            .any(|row| matches!(row.kind, ViewItemKind::Unavailable)
                && row.label.as_str().contains(&id.to_string()))
    );
    assert!(
        view.coverage()
            .gaps
            .contains(&CoverageGap::IncompleteAssociation)
    );
    assert!(!view.coverage().gaps.contains(&CoverageGap::Interrupted));
    assert!(view.apply_live(&event)?.metadata_only);
    Ok(())
}

#[test]
fn exact_done_relocates_after_its_accepted_answer_in_both_history_and_fragment_orders() -> TestResult
{
    for history_first in [false, true] {
        for saw_fragment in [false, true] {
            let mut fixture = Fixture::new()?;
            let prior = fixture
                .view
                .record_notice(ViewItemKind::Error, "prior error")?;
            let (execution, observation) = fixture.begin()?;
            let (response, publication) = execution.observe_response(0)?;
            let (attempt, owner) = response.observe_attempt(1)?;
            if saw_fragment {
                let fragment = fixture.text_event(&attempt)?;
                fixture.view.apply_live(&fragment)?;
            }
            let done = fixture.event(
                &attempt,
                ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                },
            )?;
            let ticket = event_ticket(&done)?;
            let completed = fixture
                .view
                .apply_live(&done)?
                .completion_item
                .ok_or("missing Done item")?;
            let id = fixture.store.append(answer())?;
            let record = fixture.store.history_record(&fixture.source, &id)?;
            if history_first {
                fixture.view.apply_history_record(&record)?;
            }
            owner.ok_or("missing attempt owner")?.assembled()?;
            publication
                .ok_or("missing response owner")?
                .into_publication()?
                .appended(&fixture.store, Ok(&id))?;
            fixture.view.reconcile_attempt(&ticket)?;
            fixture.view.apply_history_record(&record)?;
            let final_item = fixture
                .view
                .record_notice(ViewItemKind::Notice, "final summary")?;
            let ids: Vec<_> = fixture
                .view
                .items_from(
                    &prior,
                    super::ItemDirection::Later,
                    super::ItemInclusion::Inclusive,
                )?
                .map(|row| row.id.clone())
                .collect();
            assert_eq!(
                ids,
                vec![
                    prior,
                    record.items()[0].id.clone(),
                    completed.clone(),
                    final_item
                ]
            );
            assert!(fixture.view.item(&completed).is_some());
            assert_eq!(fixture.view.items.completion_relocations.get(), 1);
            assert!(fixture.view.apply_live(&done)?.completion_item.is_none());
            assert_eq!(fixture.view.items.completion_relocations.get(), 1);
            assert_eq!(observation.execution(), ticket.execution());
        }
    }
    Ok(())
}
