#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::Utc;
use parking_lot::Mutex;
use uuid::Uuid;

use super::pending_delivery::PendingDeliveryAttempt;
use super::pending_mailbox::PendingMailboxLease;
use super::pending_messages::PendingAgentMessages;
use super::pending_record::{
    PendingAgentMessage, PendingAgentMessageLifecycle, append_pending_message_audit,
};
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::persistence::SessionPersistError;
use crate::session::store::{DurabilityPolicy, EventStore, JsonlSink, PersistenceSink};
use crate::session::{MailboxId, SessionBinding};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn message(to_id: Uuid, content: &str, seq: Option<u64>) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4(),
        sender_id: Uuid::new_v4(),
        from: "/root/sender".to_owned(),
        role: Some("worker".to_owned()),
        to_id,
        content: content.to_owned(),
        kind: MessageKind::Update,
        seq,
        timestamp: Utc::now(),
    }
}

fn pending(message: ChannelMessage) -> PendingAgentMessage {
    let queued_at = message.timestamp;
    PendingAgentMessage::new(message, "/root/recipient".to_owned(), queued_at)
}

struct TestMailbox {
    store: Arc<EventStore>,
    mailbox_id: MailboxId,
    _lease: Arc<PendingMailboxLease>,
}

fn register_mailbox(pending: &PendingAgentMessages, recipient: Uuid) -> TestMailbox {
    register_mailbox_with_store(pending, recipient, Arc::new(EventStore::new()))
}

fn register_mailbox_with_store(
    pending: &PendingAgentMessages,
    recipient: Uuid,
    store: Arc<EventStore>,
) -> TestMailbox {
    let mailbox_id = SessionBinding::ephemeral_root().mailbox_id();
    let lease = Arc::new(PendingMailboxLease::new());
    pending
        .register_child_mailbox(recipient, mailbox_id, &store, &lease)
        .expect("test mailbox registration");
    TestMailbox {
        store,
        mailbox_id,
        _lease: lease,
    }
}

fn publish(
    pending_messages: &PendingAgentMessages,
    mailbox: &TestMailbox,
    mut record: PendingAgentMessage,
) -> bool {
    pending_messages
        .persist_for_registered_store(mailbox.store.as_ref(), &mut record)
        .expect("queue publication")
        .published
}

#[test]
fn durable_publication_is_fifo_and_exact_duplicate_is_idempotent() {
    let pending_messages = PendingAgentMessages::new();
    let recipient = Uuid::new_v4();
    let mailbox = register_mailbox(&pending_messages, recipient);
    let first = pending(message(recipient, "first", Some(1)));
    let second = pending(message(recipient, "second", Some(2)));

    assert!(publish(&pending_messages, &mailbox, first.clone()));
    assert!(!publish(&pending_messages, &mailbox, first));
    assert!(publish(&pending_messages, &mailbox, second));
    assert_eq!(pending_messages.pending_for(recipient), 2);
    assert_eq!(
        pending_messages
            .messages_for_delivery(recipient)
            .into_iter()
            .map(|message| message.content)
            .collect::<Vec<_>>(),
        ["first", "second"],
    );
}

#[test]
fn registered_store_mismatch_fails_before_queue_or_memory_mutation() {
    let pending_messages = PendingAgentMessages::new();
    let recipient = Uuid::new_v4();
    let mailbox = register_mailbox(&pending_messages, recipient);
    let wrong_store = EventStore::new();
    let mut record = pending(message(recipient, "wrong timeline", None));

    let Err(error) = pending_messages.persist_for_registered_store(&wrong_store, &mut record)
    else {
        panic!("a caller-selected store cannot replace mailbox authority");
    };

    assert!(
        error
            .to_string()
            .contains("different pending-message mailbox store")
    );
    assert!(pending_messages.is_empty());
    assert!(mailbox.store.is_empty());
    assert!(wrong_store.is_empty());
}

#[test]
fn ambiguous_jsonl_queue_append_reconciles_one_exact_stable_row() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("pending-queue.jsonl");
    let mut sink =
        JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent).expect("open pending mailbox");
    sink.fail_after_write_once();
    let pending_messages = PendingAgentMessages::new();
    let recipient = Uuid::new_v4();
    let mailbox = register_mailbox_with_store(
        &pending_messages,
        recipient,
        Arc::new(EventStore::with_sink(Box::new(sink))),
    );
    let record = pending(message(recipient, "ambiguous queue", Some(8)));
    let message_id = record.message.id;

    assert!(publish(&pending_messages, &mailbox, record));
    assert_eq!(pending_messages.pending_for(recipient), 1);
    assert_eq!(mailbox.store.len(), 1);
    assert_eq!(
        mailbox.store.events()[0].base().id.as_str(),
        format!("norn:pending-agent-message:queued:{message_id}"),
    );

    let durable = crate::session::persistence::io::read_session_events_from(
        BufReader::new(std::fs::File::open(path).expect("open durable mailbox")),
        "pending-queue",
    )
    .expect("read durable mailbox");
    assert_eq!(
        durable.events.len(),
        1,
        "the retry must adopt, not duplicate"
    );
    assert_eq!(
        durable.events[0].base().id.as_str(),
        format!("norn:pending-agent-message:queued:{message_id}"),
    );
}

struct FailFirstTwoWrites {
    attempts: Arc<AtomicUsize>,
    durable: Arc<Mutex<Vec<SessionEvent>>>,
}

impl PersistenceSink for FailFirstTwoWrites {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) < 2 {
            return Err(SessionPersistError::Io(std::io::Error::other(
                "injected uncertain queue write",
            )));
        }
        self.durable.lock().push(event.clone());
        Ok(())
    }
}

#[test]
fn indeterminate_queue_write_retains_exact_q_and_reconciles_before_later_fifo_work() -> TestResult {
    let attempts = Arc::new(AtomicUsize::new(0));
    let durable = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(EventStore::with_sink(Box::new(FailFirstTwoWrites {
        attempts: Arc::clone(&attempts),
        durable: Arc::clone(&durable),
    })));
    let pending_messages = PendingAgentMessages::new();
    let recipient = Uuid::new_v4();
    let mailbox = register_mailbox_with_store(&pending_messages, recipient, store);
    let mut first = pending(message(recipient, "first uncertain", Some(1)));
    let first_id = first.message.id;

    let Err(error) =
        pending_messages.persist_for_registered_store(mailbox.store.as_ref(), &mut first)
    else {
        panic!("two uncertain writes must not be reported as durable");
    };
    assert!(error.to_string().contains("do not resend"));
    assert_eq!(pending_messages.pending_for(recipient), 1);
    assert!(mailbox.store.is_empty());
    assert!(durable.lock().is_empty());

    let retry =
        pending_messages.persist_for_registered_store(mailbox.store.as_ref(), &mut first)?;
    assert!(!retry.published, "an exact retry must not publish Q twice");
    assert_eq!(
        mailbox.store.len(),
        1,
        "an exact retry cannot report success until the retained Q is durable",
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 3);

    let second = pending(message(recipient, "second", Some(2)));
    let second_id = second.message.id;
    assert!(publish(&pending_messages, &mailbox, second));

    assert_eq!(pending_messages.pending_for(recipient), 2);
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
    assert_eq!(mailbox.store.len(), 2);
    let durable_ids = durable
        .lock()
        .iter()
        .map(|event| event.base().id.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        durable_ids,
        [
            format!("norn:pending-agent-message:queued:{first_id}"),
            format!("norn:pending-agent-message:queued:{second_id}"),
        ],
        "the retained first Q must become durable before later FIFO work",
    );
    Ok(())
}

#[test]
fn delivery_flush_claim_rejects_same_recipient_until_drop() {
    let pending_messages = PendingAgentMessages::new();
    let recipient = Uuid::new_v4();

    let first = pending_messages
        .try_delivery_flush(recipient)
        .expect("first recipient claim");
    assert!(pending_messages.try_delivery_flush(recipient).is_none());
    drop(first);
    assert!(pending_messages.try_delivery_flush(recipient).is_some());
}

#[test]
fn delivery_flush_claims_are_independent_per_recipient() {
    let pending_messages = PendingAgentMessages::new();
    let first = pending_messages
        .try_delivery_flush(Uuid::new_v4())
        .expect("first recipient claim");
    let second = pending_messages
        .try_delivery_flush(Uuid::new_v4())
        .expect("different recipient claim");
    drop((first, second));
}

#[test]
fn replay_requires_canonical_authority_and_matching_mailbox() {
    let recipient = Uuid::new_v4();
    let resumed_recipient = Uuid::new_v4();
    let pending_messages = PendingAgentMessages::new();
    let mailbox = register_mailbox(&pending_messages, recipient);
    let mut record = pending(message(recipient, "canonical", Some(41)));
    let observer_store = EventStore::new();

    pending_messages
        .persist_for_registered_store(mailbox.store.as_ref(), &mut record)
        .unwrap();
    append_pending_message_audit(&observer_store, &record.queued_observation()).unwrap();

    let rebuilt = PendingAgentMessages::from_events(
        resumed_recipient,
        mailbox.mailbox_id,
        &mailbox.store.events(),
    )
    .unwrap();
    assert_eq!(rebuilt.pending_for(resumed_recipient), 1);
    assert_eq!(
        rebuilt.messages_for_delivery(resumed_recipient)[0].seq,
        Some(41),
    );
    assert!(
        PendingAgentMessages::from_events(
            resumed_recipient,
            mailbox.mailbox_id,
            &observer_store.events(),
        )
        .unwrap()
        .is_empty(),
        "sender/parent observations cannot become mailbox authority",
    );
    assert!(
        PendingAgentMessages::from_events(
            resumed_recipient,
            SessionBinding::ephemeral_root().mailbox_id(),
            &mailbox.store.events(),
        )
        .unwrap()
        .is_empty(),
        "a canonical row belongs only to its stable session mailbox",
    );
}

#[test]
fn replay_preserves_message_timestamp_distinct_from_queue_time() {
    let recipient = Uuid::new_v4();
    let resumed_recipient = Uuid::new_v4();
    let pending_messages = PendingAgentMessages::new();
    let mailbox = register_mailbox(&pending_messages, recipient);
    let mut routed = message(recipient, "timestamp fidelity", Some(42));
    routed.timestamp = Utc::now() - chrono::TimeDelta::hours(1);
    let message_timestamp = routed.timestamp;
    let queued_at = Utc::now();
    let mut record = PendingAgentMessage::new(routed, "/root/recipient".to_owned(), queued_at);

    pending_messages
        .persist_for_registered_store(mailbox.store.as_ref(), &mut record)
        .unwrap();
    let rebuilt = PendingAgentMessages::from_events(
        resumed_recipient,
        mailbox.mailbox_id,
        &mailbox.store.events(),
    )
    .unwrap();
    let replayed = &rebuilt.messages_for_delivery(resumed_recipient)[0];

    assert_ne!(message_timestamp, queued_at);
    assert_eq!(replayed.timestamp, message_timestamp);
}

#[test]
fn delivery_witness_before_queue_replay_fails_typed() {
    let recipient = Uuid::new_v4();
    let pending_messages = PendingAgentMessages::new();
    let mailbox = register_mailbox(&pending_messages, recipient);
    let mut record = pending(message(recipient, "out of order", Some(9)));
    pending_messages
        .persist_for_registered_store(mailbox.store.as_ref(), &mut record)
        .unwrap();
    let queue = mailbox.store.events()[0].clone();
    let delivery = PendingDeliveryAttempt::new(&record.message, None)
        .prepare(&record.message)
        .delivery_event;

    let Err(error) =
        PendingAgentMessages::from_events(recipient, mailbox.mailbox_id, &[delivery, queue])
    else {
        panic!("a stable delivery cannot precede its queue authority");
    };
    assert!(matches!(
        error,
        crate::error::SessionError::PendingMessageReplayInvalid { .. }
    ));
}

#[test]
fn secondary_dequeue_and_delivered_audits_never_consume_content() {
    let recipient = Uuid::new_v4();
    let pending_messages = PendingAgentMessages::new();
    let mailbox = register_mailbox(&pending_messages, recipient);
    let mut record = pending(message(recipient, "keep me", None));
    pending_messages
        .persist_for_registered_store(mailbox.store.as_ref(), &mut record)
        .unwrap();
    append_pending_message_audit(
        mailbox.store.as_ref(),
        &PendingAgentMessageLifecycle::Dequeued {
            message_id: record.message.id,
            to_id: recipient,
            dequeued_at: Utc::now(),
        },
    )
    .unwrap();
    mailbox
        .store
        .append(SessionEvent::Custom {
            base: EventBase::new(mailbox.store.last_event_id()),
            event_type: crate::provider::agent_event::AGENT_MESSAGE_DELIVERED_EVENT_TYPE.to_owned(),
            data: serde_json::json!({
                "phase": "delivered",
                "message_id": record.message.id,
                "from_id": record.message.sender_id,
                "to_id": recipient,
                "delivered_at": Utc::now(),
            }),
        })
        .unwrap();

    assert_eq!(
        PendingAgentMessages::from_events(recipient, mailbox.mailbox_id, &mailbox.store.events())
            .unwrap()
            .pending_for(recipient),
        1,
    );
}
