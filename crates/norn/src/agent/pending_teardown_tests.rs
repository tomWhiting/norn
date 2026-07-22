use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use uuid::Uuid;

use super::message_router::MessageRouter;
use super::pending_mailbox::PendingMailboxLease;
use super::pending_messages::PendingAgentMessages;
use super::pending_record::{PendingAgentMessage, PendingAgentMessageLifecycle};
use super::pending_teardown::TerminalPendingRetryOutcome;
use crate::r#loop::UndeliveredWindow;
use crate::r#loop::inbound::{ChannelMessage, MessageKind, inbound_channel};
use crate::session::SessionBinding;
use crate::session::events::SessionEvent;
use crate::session::persistence::SessionPersistError;
use crate::session::store::{EventStore, PersistenceSink};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
type DurableEvents = Arc<Mutex<Vec<SessionEvent>>>;
type FailingStore = (Arc<EventStore>, DurableEvents, Arc<AtomicUsize>);

#[derive(Clone, Copy)]
enum FailureMode {
    BeforeWrite,
    AfterWrite,
}

struct FailFirstAppend {
    attempts: Arc<AtomicUsize>,
    durable: DurableEvents,
    mode: FailureMode,
    first_failure_attempt: usize,
    failure_attempts: usize,
}

impl PersistenceSink for FailFirstAppend {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        let should_fail = (self.first_failure_attempt
            ..self.first_failure_attempt + self.failure_attempts)
            .contains(&attempt);
        match self.mode {
            FailureMode::BeforeWrite if should_fail => {
                return Err(SessionPersistError::Io(std::io::Error::other(
                    "injected fail-before terminal queue write",
                )));
            }
            FailureMode::AfterWrite => {
                persist_exact_once(&self.durable, event)?;
                if should_fail {
                    return Err(SessionPersistError::Io(std::io::Error::other(
                        "injected fail-after terminal queue write",
                    )));
                }
                return Ok(());
            }
            FailureMode::BeforeWrite => {}
        }
        persist_exact_once(&self.durable, event)
    }
}

fn persist_exact_once(
    durable: &Mutex<Vec<SessionEvent>>,
    event: &SessionEvent,
) -> Result<(), SessionPersistError> {
    let mut durable = durable.lock();
    if let Some(existing) = durable
        .iter()
        .find(|existing| existing.base().id == event.base().id)
    {
        let existing = serde_json::to_vec(existing)
            .map_err(|error| SessionPersistError::Io(std::io::Error::other(error)))?;
        let proposed = serde_json::to_vec(event)
            .map_err(|error| SessionPersistError::Io(std::io::Error::other(error)))?;
        if existing != proposed {
            return Err(SessionPersistError::Io(std::io::Error::other(
                "stable queue identity changed across retry",
            )));
        }
        return Ok(());
    }
    durable.push(event.clone());
    Ok(())
}

fn message(recipient_id: Uuid, content: &str) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4(),
        sender_id: Uuid::new_v4(),
        from: "/root/sender".to_owned(),
        role: Some("worker".to_owned()),
        to_id: recipient_id,
        content: content.to_owned(),
        kind: MessageKind::Update,
        seq: None,
        timestamp: Utc::now(),
    }
}

fn exact_message(
    recipient_id: Uuid,
    ordinal: u128,
    content: &str,
    kind: MessageKind,
    seq: Option<u64>,
    timestamp: DateTime<Utc>,
) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::from_u128(ordinal),
        sender_id: Uuid::from_u128(ordinal + 100),
        from: format!("/root/sender-{ordinal}<&\""),
        role: (ordinal == 1).then(|| "worker<&\"".to_owned()),
        to_id: recipient_id,
        content: content.to_owned(),
        kind,
        seq,
        timestamp,
    }
}

fn assert_exact_message(actual: &ChannelMessage, expected: &ChannelMessage) -> TestResult {
    assert_eq!(
        serde_json::to_value(actual)?,
        serde_json::to_value(expected)?
    );
    Ok(())
}

fn queued_message(event: &SessionEvent) -> TestResult<ChannelMessage> {
    let SessionEvent::Custom {
        event_type, data, ..
    } = event
    else {
        return Err("durable queue row has the wrong event shape".into());
    };
    if event_type != super::pending_record::AGENT_MESSAGE_QUEUED_EVENT_TYPE {
        return Err(format!("unexpected durable event type {event_type}").into());
    }
    let lifecycle: PendingAgentMessageLifecycle = serde_json::from_value(data.clone())?;
    let Some((record, authoritative)) = PendingAgentMessage::from_queued_event(lifecycle) else {
        return Err("durable queue row is not a queued lifecycle record".into());
    };
    if authoritative != Some(true) {
        return Err("durable queue row is not authoritative".into());
    }
    Ok(record.message)
}

fn assert_durable_messages(
    durable: &Mutex<Vec<SessionEvent>>,
    expected: &[ChannelMessage],
) -> TestResult {
    let durable = durable.lock();
    assert_eq!(durable.len(), expected.len());
    for (event, expected) in durable.iter().zip(expected) {
        assert_exact_message(&queued_message(event)?, expected)?;
    }
    Ok(())
}

fn failing_store(mode: FailureMode) -> (Arc<EventStore>, Arc<Mutex<Vec<SessionEvent>>>) {
    failing_store_for_attempts(mode, 2)
}

fn failing_store_for_attempts(
    mode: FailureMode,
    failure_attempts: usize,
) -> (Arc<EventStore>, Arc<Mutex<Vec<SessionEvent>>>) {
    let durable = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(EventStore::with_sink(Box::new(FailFirstAppend {
        attempts: Arc::new(AtomicUsize::new(0)),
        durable: Arc::clone(&durable),
        mode,
        first_failure_attempt: 0,
        failure_attempts,
    })));
    (store, durable)
}

fn store_failing_on_second(mode: FailureMode) -> FailingStore {
    let attempts = Arc::new(AtomicUsize::new(0));
    let durable = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(EventStore::with_sink(Box::new(FailFirstAppend {
        attempts: Arc::clone(&attempts),
        durable: Arc::clone(&durable),
        mode,
        first_failure_attempt: 1,
        failure_attempts: 2,
    })));
    (store, durable, attempts)
}

fn terminal_second_write_failure_preserves_exact_fifo(mode: FailureMode) -> TestResult {
    let recipient_id = Uuid::from_u128(900);
    let pending = PendingAgentMessages::new();
    let (store, durable, attempts) = store_failing_on_second(mode);
    let lease = Arc::new(PendingMailboxLease::new());
    pending.register_child_mailbox(
        recipient_id,
        SessionBinding::ephemeral_root().mailbox_id(),
        &store,
        &lease,
    )?;
    let closed = pending
        .close_child_mailbox(recipient_id, &lease)
        .ok_or("mailbox did not close")?;
    let first = exact_message(
        recipient_id,
        1,
        "first <accepted> & \"verbatim\"\nline two",
        MessageKind::Steer,
        Some(41),
        "2026-07-23T01:02:03Z".parse()?,
    );
    let second = exact_message(
        recipient_id,
        2,
        "second terminal payload\n<& must survive>",
        MessageKind::Update,
        None,
        "2026-07-23T04:05:06Z".parse()?,
    );
    let mut messages = vec![first.clone(), second.clone()];
    let error = crate::r#loop::persist_undelivered_after_close(
        &pending,
        &closed,
        &mut messages,
        UndeliveredWindow::Deregistration,
    )
    .err()
    .ok_or("second queue write failure unexpectedly reported success")?;
    assert!(error.to_string().contains("terminal queue write"));
    assert!(messages.is_empty());
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(pending.pending_for(recipient_id), 1);
    assert_eq!(
        pending
            .terminal_pending_recovery_status(recipient_id)
            .map(|status| status.pending_count),
        Some(1),
    );
    let retained = pending.messages_for_delivery(recipient_id);
    assert_eq!(retained.len(), 1);
    assert_exact_message(&retained[0], &second)?;
    match mode {
        FailureMode::BeforeWrite => {
            assert_durable_messages(&durable, std::slice::from_ref(&first))?;
        }
        FailureMode::AfterWrite => {
            assert_durable_messages(&durable, &[first.clone(), second.clone()])?;
        }
    }

    assert_eq!(
        pending.retry_terminal_pending(recipient_id)?,
        TerminalPendingRetryOutcome::Recovered { retained_count: 1 },
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
    assert_durable_messages(&durable, &[first, second])?;
    assert_eq!(store.len(), 2);
    assert_eq!(pending.pending_for(recipient_id), 0);
    assert!(
        pending
            .terminal_pending_recovery_status(recipient_id)
            .is_none()
    );
    Ok(())
}

#[test]
fn terminal_fail_before_second_q_preserves_exact_messages_and_fifo() -> TestResult {
    terminal_second_write_failure_preserves_exact_fifo(FailureMode::BeforeWrite)
}

#[test]
fn terminal_fail_after_second_q_preserves_exact_messages_and_fifo() -> TestResult {
    terminal_second_write_failure_preserves_exact_fifo(FailureMode::AfterWrite)
}

#[test]
fn terminal_transition_retains_fail_before_q_until_explicit_retry() -> TestResult {
    let recipient_id = Uuid::new_v4();
    let pending = PendingAgentMessages::new();
    let router = MessageRouter::new();
    let (store, durable) = failing_store(FailureMode::BeforeWrite);
    let lease = Arc::new(PendingMailboxLease::new());
    pending.register_child_mailbox(
        recipient_id,
        SessionBinding::ephemeral_root().mailbox_id(),
        &store,
        &lease,
    )?;
    let (sender, mut inbound) = inbound_channel(2);
    router.register(recipient_id, sender.clone());
    sender
        .try_send(message(recipient_id, "retained fail-before payload"))
        .map_err(|error| format!("message was not accepted: {error}"))?;

    let transition = pending.transition_live_route(
        recipient_id,
        store.as_ref(),
        &router,
        &mut inbound,
        Some(&lease),
    )?;
    assert!(transition.first_error.is_some());
    assert!(!transition.hard_failure);
    assert_eq!(pending.pending_for(recipient_id), 1);
    assert_eq!(
        pending
            .terminal_pending_recovery_status(recipient_id)
            .map(|status| status.pending_count),
        Some(1),
    );
    assert!(durable.lock().is_empty());

    assert_eq!(
        pending.retry_terminal_pending(recipient_id)?,
        TerminalPendingRetryOutcome::Recovered { retained_count: 1 },
    );
    assert_eq!(durable.lock().len(), 1);
    assert_eq!(store.len(), 1);
    assert_eq!(pending.pending_for(recipient_id), 0);
    assert!(
        pending
            .terminal_pending_recovery_status(recipient_id)
            .is_none()
    );
    Ok(())
}

#[test]
fn final_closed_drain_reconciles_fail_after_without_duplicate_q() -> TestResult {
    let recipient_id = Uuid::new_v4();
    let pending = PendingAgentMessages::new();
    let (store, durable) = failing_store(FailureMode::AfterWrite);
    let lease = Arc::new(PendingMailboxLease::new());
    pending.register_child_mailbox(
        recipient_id,
        SessionBinding::ephemeral_root().mailbox_id(),
        &store,
        &lease,
    )?;
    let closed = pending
        .close_child_mailbox(recipient_id, &lease)
        .ok_or("mailbox did not close")?;
    let mut messages = vec![message(recipient_id, "retained fail-after payload")];

    let error = crate::r#loop::persist_undelivered_after_close(
        &pending,
        &closed,
        &mut messages,
        UndeliveredWindow::Deregistration,
    )
    .err()
    .ok_or("fail-after injection unexpectedly reported durable success")?;
    assert!(
        error
            .to_string()
            .contains("fail-after terminal queue write")
    );
    assert!(messages.is_empty());
    assert_eq!(pending.pending_for(recipient_id), 1);
    assert_eq!(durable.lock().len(), 1, "fail-after wrote one physical Q");
    assert_eq!(
        store.len(),
        0,
        "the uncertain Q is not yet adopted in memory"
    );

    assert_eq!(
        pending.retry_terminal_pending(recipient_id)?,
        TerminalPendingRetryOutcome::Recovered { retained_count: 1 },
    );
    assert_eq!(durable.lock().len(), 1, "retry must adopt, not duplicate Q");
    assert_eq!(store.len(), 1);
    assert_eq!(pending.pending_for(recipient_id), 0);
    Ok(())
}

#[test]
fn empty_terminal_drain_reconciles_an_earlier_live_queue_failure() -> TestResult {
    let recipient_id = Uuid::new_v4();
    let pending = PendingAgentMessages::new();
    let (store, durable) = failing_store(FailureMode::BeforeWrite);
    let lease = Arc::new(PendingMailboxLease::new());
    pending.register_child_mailbox(
        recipient_id,
        SessionBinding::ephemeral_root().mailbox_id(),
        &store,
        &lease,
    )?;
    let mut record = super::pending_record::PendingAgentMessage::new(
        message(recipient_id, "retained before terminal close"),
        recipient_id.to_string(),
        Utc::now(),
    );

    let first_error = pending
        .persist_for_registered_recipient(&mut record)
        .err()
        .ok_or("the injected live queue failure unexpectedly succeeded")?;
    assert!(
        first_error
            .to_string()
            .contains("indeterminate queue durability")
    );
    assert_eq!(pending.pending_for(recipient_id), 1);
    assert!(
        pending
            .terminal_pending_recovery_status(recipient_id)
            .is_none()
    );

    let closed = pending
        .close_child_mailbox(recipient_id, &lease)
        .ok_or("mailbox did not close")?;
    let mut empty = Vec::new();
    crate::r#loop::persist_undelivered_after_close(
        &pending,
        &closed,
        &mut empty,
        UndeliveredWindow::Deregistration,
    )?;

    assert!(empty.is_empty());
    assert_eq!(durable.lock().len(), 1);
    assert_eq!(store.len(), 1);
    assert_eq!(pending.pending_for(recipient_id), 0);
    assert!(
        pending
            .terminal_pending_recovery_status(recipient_id)
            .is_none()
    );
    Ok(())
}

#[test]
fn empty_terminal_drain_adopts_an_earlier_failure_until_retry_succeeds() -> TestResult {
    let recipient_id = Uuid::new_v4();
    let pending = PendingAgentMessages::new();
    let (store, durable) = failing_store_for_attempts(FailureMode::BeforeWrite, 4);
    let lease = Arc::new(PendingMailboxLease::new());
    pending.register_child_mailbox(
        recipient_id,
        SessionBinding::ephemeral_root().mailbox_id(),
        &store,
        &lease,
    )?;
    let mut record = super::pending_record::PendingAgentMessage::new(
        message(recipient_id, "retained across terminal retry failure"),
        recipient_id.to_string(),
        Utc::now(),
    );
    assert!(
        pending
            .persist_for_registered_recipient(&mut record)
            .is_err()
    );

    let closed = pending
        .close_child_mailbox(recipient_id, &lease)
        .ok_or("mailbox did not close")?;
    let mut empty = Vec::new();
    assert!(
        crate::r#loop::persist_undelivered_after_close(
            &pending,
            &closed,
            &mut empty,
            UndeliveredWindow::Deregistration,
        )
        .is_err()
    );
    assert_eq!(pending.pending_for(recipient_id), 1);
    assert_eq!(
        pending
            .terminal_pending_recovery_status(recipient_id)
            .map(|status| status.pending_count),
        Some(1),
    );
    assert!(durable.lock().is_empty());

    assert_eq!(
        pending.retry_terminal_pending(recipient_id)?,
        TerminalPendingRetryOutcome::Recovered { retained_count: 1 },
    );
    assert_eq!(durable.lock().len(), 1);
    assert_eq!(store.len(), 1);
    assert_eq!(pending.pending_for(recipient_id), 0);
    assert!(
        pending
            .terminal_pending_recovery_status(recipient_id)
            .is_none()
    );
    Ok(())
}
