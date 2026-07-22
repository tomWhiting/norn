use std::sync::{Arc, Barrier};

use chrono::Utc;
use uuid::Uuid;

use super::message_router::MessageRouter;
use super::pending_delivery::pending_queue_event_id;
use super::pending_mailbox::PendingMailboxLease;
use super::pending_messages::PendingAgentMessages;
use crate::r#loop::inbound::{ChannelMessage, InboundTrySendError, MessageKind, inbound_channel};
use crate::session::SessionBinding;
use crate::session::store::EventStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

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

#[test]
fn terminal_transition_closes_receiver_even_when_mailbox_validation_fails() {
    let recipient_id = Uuid::new_v4();
    let pending = PendingAgentMessages::new();
    let router = MessageRouter::new();
    let store = EventStore::new();
    let lease = Arc::new(PendingMailboxLease::new());
    let (sender, mut inbound) = inbound_channel(4);
    router.register(recipient_id, sender.clone());

    let result =
        pending.transition_live_route(recipient_id, &store, &router, &mut inbound, Some(&lease));

    assert!(result.is_err());
    assert_eq!(
        sender.try_send(message(recipient_id, "too late")),
        Err(InboundTrySendError::Closed),
        "even an error path must close direct senders before returning",
    );
}

#[test]
fn terminal_transition_closes_before_drain_and_persists_every_accepted_message() -> TestResult {
    let recipient_id = Uuid::new_v4();
    let pending = PendingAgentMessages::new();
    let router = MessageRouter::new();
    let store = Arc::new(EventStore::new());
    let lease = Arc::new(PendingMailboxLease::new());
    let mailbox_id = SessionBinding::ephemeral_root().mailbox_id();
    pending.register_child_mailbox(recipient_id, mailbox_id, &store, &lease)?;
    let (sender, mut inbound) = inbound_channel(4);
    router.register(recipient_id, sender.clone());
    let accepted = message(recipient_id, "accepted before close");
    sender
        .try_send(accepted.clone())
        .map_err(|error| format!("test message should be accepted: {error}"))?;

    let transition = pending.transition_live_route(
        recipient_id,
        store.as_ref(),
        &router,
        &mut inbound,
        Some(&lease),
    )?;

    assert!(transition.closed.is_some());
    assert!(transition.first_error.is_none());
    assert!(!router.is_routed(recipient_id));
    assert_eq!(
        sender.try_send(message(recipient_id, "after close")),
        Err(InboundTrySendError::Closed),
    );
    assert!(
        store
            .event_by_id(&pending_queue_event_id(accepted.id))
            .is_some(),
        "the successful pre-close send must have one durable Q",
    );
    Ok(())
}

#[test]
fn terminal_send_race_never_loses_a_successful_direct_send() -> TestResult {
    const TRIALS: usize = 64;
    let mut accepted = 0;
    let mut closed = 0;

    for trial in 0..TRIALS {
        let recipient_id = Uuid::new_v4();
        let pending = PendingAgentMessages::new();
        let router = MessageRouter::new();
        let store = Arc::new(EventStore::new());
        let lease = Arc::new(PendingMailboxLease::new());
        pending.register_child_mailbox(
            recipient_id,
            SessionBinding::ephemeral_root().mailbox_id(),
            &store,
            &lease,
        )?;
        let (sender, mut inbound) = inbound_channel(1);
        router.register(recipient_id, sender.clone());
        let candidate = message(recipient_id, &format!("trial {trial}"));
        let candidate_id = candidate.id;
        let (send_result, transition) = if trial == 0 {
            let send_result = sender.try_send(candidate);
            let transition = pending.transition_live_route(
                recipient_id,
                store.as_ref(),
                &router,
                &mut inbound,
                Some(&lease),
            )?;
            (send_result, transition)
        } else if trial == 1 {
            let transition = pending.transition_live_route(
                recipient_id,
                store.as_ref(),
                &router,
                &mut inbound,
                Some(&lease),
            )?;
            (sender.try_send(candidate), transition)
        } else {
            let barrier = Arc::new(Barrier::new(2));
            let sender_barrier = Arc::clone(&barrier);
            let sender_task = std::thread::spawn(move || {
                sender_barrier.wait();
                sender.try_send(candidate)
            });

            barrier.wait();
            let transition = pending.transition_live_route(
                recipient_id,
                store.as_ref(),
                &router,
                &mut inbound,
                Some(&lease),
            )?;
            let Ok(send_result) = sender_task.join() else {
                return Err("direct sender thread panicked".into());
            };
            (send_result, transition)
        };
        let durable = store
            .event_by_id(&pending_queue_event_id(candidate_id))
            .is_some();
        match send_result {
            Ok(()) => {
                accepted += 1;
                assert!(
                    durable,
                    "successful direct send in trial {trial} lost its Q"
                );
            }
            Err(InboundTrySendError::Closed) => {
                closed += 1;
                assert!(!durable, "closed direct send in trial {trial} produced a Q");
            }
            Err(InboundTrySendError::Full) => {
                return Err(format!("empty trial channel was unexpectedly full in {trial}").into());
            }
        }
        assert!(transition.closed.is_some());
        assert!(transition.first_error.is_none());
    }

    assert_eq!(accepted + closed, TRIALS);
    assert!(accepted > 0, "the race matrix observed no accepted sends");
    assert!(closed > 0, "the race matrix observed no close-first sends");
    Ok(())
}
