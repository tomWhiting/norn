//! Opaque receipt ownership, scope isolation and coalesced notification regressions.

use futures_util::FutureExt;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::*;
use crate::session::branch::SessionBinding;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn accepted_opening_is_available_before_wait_and_cannot_rebind() -> TestResult {
    let store = EventStore::new();
    let agent = Uuid::new_v4();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
    let (tx, receiver) = broadcast::channel(1);
    let root = AgentEventSender::new(tx, agent, "fixture".to_owned());
    let (sender, observation) = root.observe_execution(&store, &source, Uuid::new_v4())?;
    let owner = sender
        .claim_execution(&store, true)?
        .ok_or("missing opening owner")?;
    let event_id = store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "accepted".to_owned(),
    })?;
    owner.appended(&store, Ok(&event_id))?;
    assert!(
        matches!(observation.opening_input(), Some(PublicationResolution::Accepted(record)) if record.cursor().source() == &source)
    );
    assert!(observation.changed().now_or_never().is_some());
    assert!(observation.changed().now_or_never().is_none());
    assert!(matches!(
        sender.claim_execution(&store, true),
        Err(ObservationError::Reused { .. })
    ));
    assert!(matches!(
        sender.observe_execution(&store, &source, Uuid::new_v4()),
        Err(ObservationError::Reused { .. })
    ));
    drop(receiver);
    Ok(())
}

#[test]
fn same_identifiers_do_not_make_the_same_owner_and_child_scope_is_clear() -> TestResult {
    let store = EventStore::new();
    let agent = Uuid::new_v4();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
    let (tx, mut receiver) = broadcast::channel(1);
    let root = AgentEventSender::new(tx, agent, "fixture".to_owned());
    let execution = Uuid::new_v4();
    let (sender, first) = root.observe_execution(&store, &source, execution)?;
    let (second_sender, second) = root.observe_execution(&store, &source, execution)?;
    assert!(!first.same_execution(&second));
    assert!(first.same_execution(&first.clone()));
    assert!(
        root.observe_execution(&EventStore::new(), &source, execution)
            .is_err()
    );
    sender
        .for_child(Uuid::new_v4(), "child".to_owned())
        .send(ProviderEvent::TextDelta {
            text: "child".to_owned(),
        });
    assert!(matches!(
        receiver.try_recv()?.event,
        AgentEventKind::Provider(_)
    ));
    drop(second_sender);
    Ok(())
}

#[test]
fn abandoned_and_failed_attempt_tickets_resolve_without_a_queue() -> TestResult {
    let store = EventStore::new();
    let agent = Uuid::new_v4();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
    let (tx, receiver) = broadcast::channel(1);
    let (sender, execution) = AgentEventSender::new(tx, agent, "fixture".to_owned())
        .observe_execution(&store, &source, Uuid::new_v4())?;
    assert!(sender.claim_execution(&store, false)?.is_none());
    let (response, publication) = sender.observe_response(0)?;
    for (number, expected) in [(1, PublicationEnd::Abandoned), (2, PublicationEnd::Failed)] {
        let (attempt, owner) = response.observe_attempt(number)?;
        let Some(ObservationScope::Attempt(ticket)) = &attempt.observation else {
            return Err("missing attempt ticket".into());
        };
        let owner = owner.ok_or("missing attempt owner")?;
        if expected == PublicationEnd::Failed {
            owner.failed()?;
        } else {
            drop(owner);
        }
        assert!(
            matches!(ticket.resolution(), Some(PublicationResolution::NotAccepted(reason)) if *reason == expected)
        );
        assert_eq!(ticket.attempt().attempt, number);
    }
    assert!(execution.changed().now_or_never().is_some());
    assert!(execution.changed().now_or_never().is_none());
    drop(publication);
    drop(receiver);
    Ok(())
}

#[test]
fn nested_observation_is_refused_without_echoing_event_body() -> TestResult {
    let store = EventStore::new();
    let agent = Uuid::new_v4();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
    let (tx, receiver) = broadcast::channel(1);
    let (sender, execution) = AgentEventSender::new(tx, agent, "fixture".to_owned())
        .observe_execution(&store, &source, Uuid::new_v4())?;
    let scope = ObservationScope::Execution(execution);
    let observed = ObservedAgentEvent::new(
        scope.clone(),
        AgentEventKind::Provider(ProviderEvent::TextDelta {
            text: "private fixture body".to_owned(),
        }),
    )?;
    assert!(!format!("{observed:?}").contains("private fixture body"));
    assert!(matches!(
        ObservedAgentEvent::new(scope, AgentEventKind::Observed(observed)),
        Err(ObservationError::Nested { .. })
    ));
    drop(sender);
    drop(receiver);
    Ok(())
}

#[tokio::test]
async fn notify_waiter_registration_and_retained_cells_survive_coalesced_changes() -> TestResult {
    let store = EventStore::new();
    let agent = Uuid::new_v4();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
    let (tx, receiver) = broadcast::channel(1);
    let (sender, execution) = AgentEventSender::new(tx, agent, "fixture".to_owned())
        .observe_execution(&store, &source, Uuid::new_v4())?;
    let owner = sender
        .claim_execution(&store, true)?
        .ok_or("missing opening owner")?;
    let mut waiting = Box::pin(execution.changed());
    assert!(waiting.as_mut().now_or_never().is_none());
    owner.failed()?;
    assert!(waiting.as_mut().now_or_never().is_some());
    drop(waiting);
    assert!(matches!(
        execution.opening_input(),
        Some(PublicationResolution::NotAccepted(PublicationEnd::Failed))
    ));
    assert!(execution.changed().now_or_never().is_none());
    let (response, publication) = sender.observe_response(0)?;
    let (attempt_sender, attempt_owner) = response.observe_attempt(1)?;
    let Some(ObservationScope::Attempt(ticket)) = &attempt_sender.observation else {
        return Err("missing attempt ticket".into());
    };
    let weak = std::sync::Arc::downgrade(&ticket.0);
    attempt_owner.ok_or("missing attempt owner")?.failed()?;
    drop(attempt_sender);
    assert!(
        weak.upgrade().is_none(),
        "failed attempt is not retained by the execution or response"
    );
    drop(publication);
    drop(receiver);
    Ok(())
}
