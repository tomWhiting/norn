#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use super::pending_delivery::PendingDeliveryAttempt;
use super::pending_messages::PendingAgentMessages;
use super::pending_record::{
    AGENT_MESSAGE_DEQUEUED_EVENT_TYPE, AGENT_MESSAGE_QUEUED_EVENT_TYPE, PendingAgentMessage,
    PendingAgentMessageLifecycle,
};
use crate::error::SessionError;
use crate::r#loop::inbound::{ChannelMessage, MessageKind, frame_message};
use crate::provider::agent_event::AGENT_MESSAGE_DELIVERED_EVENT_TYPE;
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;
use crate::session::{MailboxId, SessionBinding};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn mailbox_id() -> MailboxId {
    SessionBinding::ephemeral_root().mailbox_id()
}

fn message(to_id: Uuid, content: &str) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4(),
        sender_id: Uuid::new_v4(),
        from: "/root/sender".to_owned(),
        role: Some("worker".to_owned()),
        to_id,
        content: content.to_owned(),
        kind: MessageKind::Update,
        seq: Some(7),
        timestamp: Utc::now(),
    }
}

fn pre_d8_message(to_id: Uuid, content: &str) -> ChannelMessage {
    let mut message = message(to_id, content);
    message.seq = None;
    message
}

fn canonical_pair(
    historical_recipient: Uuid,
    mailbox_id: MailboxId,
    content: &str,
) -> (ChannelMessage, SessionEvent, SessionEvent) {
    let message = message(historical_recipient, content);
    let mut pending =
        PendingAgentMessage::new(message.clone(), "/root/recipient".to_owned(), Utc::now());
    pending
        .bind_mailbox(historical_recipient, mailbox_id)
        .expect("bind canonical replay fixture");
    let queue = pending
        .prepare_queue_event(&EventStore::new())
        .expect("prepare canonical queue fixture");
    let delivery = PendingDeliveryAttempt::new(&message, Some(queue.base().id.clone()))
        .prepare(&message)
        .delivery_event;
    (message, queue, delivery)
}

fn legacy_queue(message: &ChannelMessage) -> SessionEvent {
    let lifecycle = PendingAgentMessageLifecycle::Queued {
        message_id: message.id,
        from_id: message.sender_id,
        from: message.from.clone(),
        role: message.role.clone(),
        to_id: message.to_id,
        to: "/root/recipient".to_owned(),
        kind: message.kind,
        seq: message.seq,
        content: message.content.clone(),
        queued_at: message.timestamp,
        message_timestamp: None,
        authoritative: None,
        mailbox_id: None,
    };
    custom_event(
        AGENT_MESSAGE_QUEUED_EVENT_TYPE,
        serde_json::to_value(lifecycle).expect("serialize legacy queue fixture"),
    )
}

fn legacy_user(message: &ChannelMessage) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: frame_message(message),
    }
}

fn dequeued(message: &ChannelMessage) -> SessionEvent {
    custom_event(
        AGENT_MESSAGE_DEQUEUED_EVENT_TYPE,
        serde_json::to_value(PendingAgentMessageLifecycle::Dequeued {
            message_id: message.id,
            to_id: message.to_id,
            dequeued_at: Utc::now(),
        })
        .expect("serialize dequeue fixture"),
    )
}

fn delivered(message: &ChannelMessage, current_shape: bool) -> SessionEvent {
    let mut data = json!({
        "phase": "delivered",
        "message_id": message.id,
        "to_id": message.to_id,
    });
    if current_shape {
        data["from_id"] = json!(message.sender_id);
        data["from"] = json!(message.from);
        data["seq"] = json!(message.seq);
        data["delivered_at"] = json!(Utc::now());
    }
    custom_event(AGENT_MESSAGE_DELIVERED_EVENT_TYPE, data)
}

fn custom_event(event_type: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: event_type.to_owned(),
        data,
    }
}

fn fork_provenance(parent_event_anchor: EventId) -> SessionEvent {
    SessionEvent::ChildBranch {
        base: EventBase::new(None),
        parent_session_id: Some(Uuid::new_v4().to_string()),
        child_session_id: Some(Uuid::new_v4().to_string()),
        path_address: "root/fork-legacy".to_owned(),
        parent_event_anchor: Some(parent_event_anchor),
        kind: crate::session::events::ChildBranchKind::Fork,
    }
}

fn assert_replay_error(recipient_id: Uuid, events: &[SessionEvent]) {
    let Err(error) = PendingAgentMessages::from_events(recipient_id, mailbox_id(), events) else {
        panic!("malformed replay protocol must fail closed");
    };
    assert!(matches!(
        error,
        SessionError::PendingMessageReplayInvalid { .. }
    ));
}

#[test]
fn canonical_queue_rebinds_to_fresh_runtime_with_matching_mailbox() {
    let historical_recipient = Uuid::new_v4();
    let resumed_recipient = Uuid::new_v4();
    let mailbox_id = mailbox_id();
    let (message, queue, _) = canonical_pair(historical_recipient, mailbox_id, "resume me");

    let replayed = PendingAgentMessages::from_events(resumed_recipient, mailbox_id, &[queue])
        .expect("matching mailbox replay");

    assert_eq!(replayed.pending_for(historical_recipient), 0);
    assert_eq!(replayed.pending_for(resumed_recipient), 1);
    let delivered = replayed.messages_for_delivery(resumed_recipient);
    assert_eq!(delivered[0].id, message.id);
    assert_eq!(delivered[0].to_id, historical_recipient);
}

#[test]
fn fork_inherited_foreign_queue_is_not_child_pending_authority() {
    let parent_mailbox = mailbox_id();
    let child_mailbox = mailbox_id();
    assert_ne!(parent_mailbox, child_mailbox);
    let (_, inherited_queue, _) = canonical_pair(Uuid::new_v4(), parent_mailbox, "parent only");

    let replayed =
        PendingAgentMessages::from_events(Uuid::new_v4(), child_mailbox, &[inherited_queue])
            .expect("foreign fork-prefix queue remains valid history");

    assert!(replayed.is_empty());
}

#[test]
fn foreign_complete_pair_validates_without_reclassifying_user_history() {
    let parent_mailbox = mailbox_id();
    let child_mailbox = mailbox_id();
    let (message, queue, delivery) =
        canonical_pair(Uuid::new_v4(), parent_mailbox, "already delivered");
    let events = vec![queue, delivery];
    let before = serde_json::to_vec(&events).expect("serialize immutable prefix");

    let replayed = PendingAgentMessages::from_events(Uuid::new_v4(), child_mailbox, &events)
        .expect("complete foreign pair validates");

    assert!(replayed.is_empty());
    assert_eq!(
        serde_json::to_vec(&events).expect("serialize unchanged prefix"),
        before
    );
    assert!(matches!(
        &events[1],
        SessionEvent::UserMessage { content, .. } if content == &frame_message(&message)
    ));
}

#[test]
fn historical_runtime_match_does_not_authorize_unresolved_legacy_queue() {
    let historical_recipient = Uuid::new_v4();
    let queue = legacy_queue(&pre_d8_message(historical_recipient, "legacy pending"));

    assert!(matches!(
        PendingAgentMessages::from_events(historical_recipient, mailbox_id(), &[queue]),
        Err(SessionError::PreD8PendingMessageOwnershipUnknown)
    ));
}

#[test]
fn direct_fresh_runtime_resume_fails_closed_for_unowned_legacy_pending_row() -> TestResult {
    let historical_recipient = Uuid::new_v4();
    let fresh_recipient = Uuid::new_v4();
    let secret = "legacy payload must not enter the resume error";
    let queue = legacy_queue(&pre_d8_message(historical_recipient, secret));

    let error = PendingAgentMessages::from_events(fresh_recipient, mailbox_id(), &[queue])
        .err()
        .ok_or_else(|| {
            std::io::Error::other(
                "fresh runtime unexpectedly adopted an unowned pre-D8 pending row",
            )
        })?;

    assert!(matches!(
        &error,
        SessionError::PreD8PendingMessageOwnershipUnknown
    ));
    let rendered = error.to_string();
    let debug = format!("{error:?}");
    assert!(!rendered.contains(secret));
    assert!(!rendered.contains(&historical_recipient.to_string()));
    assert!(!rendered.contains(&fresh_recipient.to_string()));
    assert!(!rendered.contains("/root/sender"));
    assert!(!rendered.contains("/root/recipient"));
    assert!(!debug.contains(secret));
    assert!(!debug.contains(&historical_recipient.to_string()));
    assert!(!debug.contains(&fresh_recipient.to_string()));
    assert!(!debug.contains("/root/sender"));
    assert!(!debug.contains("/root/recipient"));
    Ok(())
}

#[test]
fn fork_inherited_unresolved_legacy_observation_fails_closed() {
    let parent_recipient = Uuid::new_v4();
    let fork_recipient = Uuid::new_v4();
    let inherited = legacy_queue(&pre_d8_message(parent_recipient, "parent pending"));
    let provenance = fork_provenance(inherited.base().id.clone());
    let events = [provenance, inherited];

    assert!(matches!(
        PendingAgentMessages::from_events(fork_recipient, mailbox_id(), &events),
        Err(SessionError::PreD8PendingMessageOwnershipUnknown)
    ));
}

#[test]
fn completed_foreign_legacy_observation_is_not_fork_mailbox_authority() -> TestResult {
    let parent_recipient = Uuid::new_v4();
    let fork_recipient = Uuid::new_v4();
    let message = pre_d8_message(parent_recipient, "already delivered in parent history");
    let queue = legacy_queue(&message);
    let user = legacy_user(&message);
    let audit = dequeued(&message);
    let provenance = fork_provenance(audit.base().id.clone());
    let events = [provenance, queue, user, audit];

    let replayed = PendingAgentMessages::from_events(fork_recipient, mailbox_id(), &events)?;

    assert!(replayed.is_empty());
    Ok(())
}

#[test]
fn legacy_dequeued_audit_consumes_exact_preceding_user_frame() {
    let recipient = Uuid::new_v4();
    let message = pre_d8_message(recipient, "legacy dequeued");
    let events = [
        legacy_queue(&message),
        legacy_user(&message),
        dequeued(&message),
    ];

    let replayed = PendingAgentMessages::from_events(recipient, mailbox_id(), &events)
        .expect("legacy dequeue replay");

    assert!(replayed.is_empty());
}

#[test]
fn early_and_current_delivered_audits_consume_legacy_frames() {
    for current_shape in [false, true] {
        let recipient = Uuid::new_v4();
        let message = pre_d8_message(recipient, "legacy delivered");
        let events = [
            legacy_queue(&message),
            legacy_user(&message),
            delivered(&message, current_shape),
        ];

        let replayed = PendingAgentMessages::from_events(recipient, mailbox_id(), &events)
            .expect("legacy delivered replay");

        assert!(replayed.is_empty());
    }
}

#[test]
fn stable_delivery_before_queue_fails_typed() {
    let (_, queue, delivery) = canonical_pair(Uuid::new_v4(), mailbox_id(), "bad order");
    assert_replay_error(Uuid::new_v4(), &[delivery, queue]);
}

#[test]
fn stable_delivery_conflicting_with_canonical_queue_fails_typed() {
    let recipient = Uuid::new_v4();
    let mailbox = mailbox_id();
    let (mut conflicting, queue, _) = canonical_pair(recipient, mailbox, "canonical content");
    conflicting.content = "conflicting content".to_owned();
    let delivery = PendingDeliveryAttempt::new(&conflicting, Some(queue.base().id.clone()))
        .prepare(&conflicting)
        .delivery_event;

    assert!(matches!(
        PendingAgentMessages::from_events(recipient, mailbox, &[queue, delivery]),
        Err(SessionError::PendingMessageReplayInvalid { .. })
    ));
}

#[test]
fn duplicate_stable_delivery_for_one_canonical_queue_fails_typed() {
    let recipient = Uuid::new_v4();
    let mailbox = mailbox_id();
    let (_, queue, delivery) = canonical_pair(recipient, mailbox, "deliver once");

    assert!(matches!(
        PendingAgentMessages::from_events(recipient, mailbox, &[queue, delivery.clone(), delivery],),
        Err(SessionError::PendingMessageReplayInvalid { .. })
    ));
}

#[test]
fn legacy_audit_before_user_frame_fails_typed() {
    let recipient = Uuid::new_v4();
    let message = pre_d8_message(recipient, "late user frame");
    assert_replay_error(
        recipient,
        &[
            legacy_queue(&message),
            dequeued(&message),
            legacy_user(&message),
        ],
    );
}

#[test]
fn one_legacy_user_frame_cannot_consume_two_identical_queues() {
    let recipient = Uuid::new_v4();
    let first = pre_d8_message(recipient, "same frame");
    let mut second = first.clone();
    second.id = Uuid::new_v4();
    assert_eq!(frame_message(&first), frame_message(&second));

    assert_replay_error(
        recipient,
        &[
            legacy_queue(&first),
            legacy_queue(&second),
            legacy_user(&first),
            dequeued(&first),
            dequeued(&second),
        ],
    );
}

#[test]
fn forged_legacy_audits_do_not_consume_canonical_queue_authority() {
    let recipient = Uuid::new_v4();
    let mailbox = mailbox_id();
    let (message, queue, _) = canonical_pair(recipient, mailbox, "canonical pending");
    let events = [queue, dequeued(&message), delivered(&message, true)];

    let replayed = PendingAgentMessages::from_events(recipient, mailbox, &events)
        .expect("secondary audits cannot consume canonical authority");

    assert_eq!(replayed.pending_for(recipient), 1);
}

#[test]
fn noncanonical_and_malformed_reserved_ids_fail_typed() {
    let message_id =
        Uuid::parse_str("abcdef01-2345-4678-9abc-def012345678").expect("fixed test UUID is valid");
    let timestamp: DateTime<Utc> = Utc::now();
    let suffixes = [
        message_id.hyphenated().to_string().to_uppercase(),
        message_id.simple().to_string(),
        "not-a-uuid".to_owned(),
    ];

    for suffix in suffixes {
        let queue = SessionEvent::Custom {
            base: EventBase {
                id: EventId::from_stable_namespace(format!(
                    "norn:pending-agent-message:queued:{suffix}"
                )),
                parent_id: None,
                timestamp,
            },
            event_type: AGENT_MESSAGE_QUEUED_EVENT_TYPE.to_owned(),
            data: json!({}),
        };
        let delivery = SessionEvent::UserMessage {
            base: EventBase {
                id: EventId::from_stable_namespace(format!(
                    "norn:pending-agent-message:delivered:{suffix}"
                )),
                parent_id: None,
                timestamp,
            },
            content: "reserved".to_owned(),
        };

        assert_replay_error(Uuid::new_v4(), &[queue]);
        assert_replay_error(Uuid::new_v4(), &[delivery]);
    }
}
