//! Durable fallback queueing and route-race behavior.

use std::sync::Arc;

use super::super::*;
use super::test_support::{
    build_infra, child_infra, ctx_with, ctx_with_mailbox, envelope_for, queued_authorities,
    queued_mailbox_ids, register_agent, send_args,
};
use crate::agent::child_policy::MessagingScope;
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::AgentRegistry;
use crate::r#loop::inbound::inbound_channel;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

#[tokio::test]
async fn signal_agent_queues_valid_unrouted_recipient() {
    let sender = Uuid::new_v4();
    let (infra, registry, _router) = build_infra(sender);
    let recipient = register_agent(&registry, "/parent/sleeping-child", Some(sender));
    let sender_store = Arc::clone(&infra.event_store);
    let pending_store = Arc::clone(&infra.pending_messages);

    let mailbox = ctx_with_mailbox(infra, recipient);
    let out = SignalAgentTool::new()
        .execute(
            &envelope_for(
                "signal_agent",
                send_args("/parent/sleeping-child", "update", "queue me"),
            ),
            &mailbox.ctx,
        )
        .await
        .expect("send");

    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(out.content["delivered"], false);
    assert_eq!(out.content["queued"], true);
    assert_eq!(out.content["delivery_state"], "queued");
    assert_eq!(out.content["to"], recipient.to_string());
    assert_eq!(pending_store.pending_for(recipient), 1);
    let follow_ups = SignalAgentTool::new()
        .register_follow_ups(&out, &mailbox.ctx)
        .await;
    assert_eq!(follow_ups.len(), 1);
    assert_eq!(follow_ups[0].action, "wake_agent");
    assert_eq!(follow_ups[0].tool, WAKE_AGENT_TOOL_NAME);
    assert_eq!(follow_ups[0].args_mode, FollowUpArgsMode::Replace);
    assert_eq!(follow_ups[0].args["agent_id"], recipient.to_string());

    let queued_events = sender_store
        .events()
        .into_iter()
        .filter(|event| {
            matches!(
                event,
                SessionEvent::Custom { event_type, .. }
                    if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE
            )
        })
        .count();
    assert_eq!(
        queued_events, 1,
        "queued sends must leave a durable audit record in the sender store",
    );
    assert_eq!(queued_authorities(&sender_store), [false]);
    assert_eq!(queued_authorities(&mailbox.store), [true]);
    let mailbox_id =
        serde_json::to_value(mailbox.binding.mailbox_id()).expect("serialize recipient mailbox id");
    assert_eq!(
        queued_mailbox_ids(&sender_store).as_slice(),
        std::slice::from_ref(&mailbox_id),
    );
    assert_eq!(queued_mailbox_ids(&mailbox.store), [mailbox_id]);
    assert_eq!(
        crate::agent::PendingAgentMessages::from_events(
            recipient,
            mailbox.binding.mailbox_id(),
            &mailbox.store.events(),
        )
        .expect("replay recipient mailbox")
        .pending_for(recipient),
        1,
    );
}

/// An unrouted root parent has a real mailbox authority and future consumer.
#[tokio::test]
async fn signal_agent_unrouted_root_parent_is_queued() {
    let registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let root = Uuid::new_v4();
    let sender = register_agent(&registry, "/worker", Some(root));
    let parent_store = Arc::new(EventStore::new());
    let infra = child_infra(
        sender,
        root,
        MessagingScope::ParentOnly,
        &registry,
        &router,
        &parent_store,
    );
    let sender_store = Arc::clone(&infra.event_store);
    let pending_store = Arc::clone(&infra.pending_messages);
    let root_binding = crate::session::SessionBinding::ephemeral_root();
    let root_mailbox_lease = Arc::new(crate::agent::PendingMailboxLease::new());
    pending_store
        .register_root_mailbox(
            root,
            root_binding.mailbox_id(),
            &parent_store,
            &root_mailbox_lease,
        )
        .expect("register root mailbox");

    let ctx = ctx_with(infra);
    let out = SignalAgentTool::new()
        .execute(
            &envelope_for("signal_agent", send_args("parent", "update", "status")),
            &ctx,
        )
        .await
        .expect("executes");
    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(out.content["delivered"], false);
    assert_eq!(out.content["queued"], true);
    assert_eq!(pending_store.pending_for(root), 1);
    for store in [&sender_store, &parent_store] {
        assert!(
            store.events().iter().any(|event| matches!(
                event,
                SessionEvent::Custom { event_type, .. }
                    if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE
            )),
            "queued root-bound sends must be audited in sender and parent stores",
        );
    }
    let root_mailbox_id =
        serde_json::to_value(root_binding.mailbox_id()).expect("serialize root mailbox id");
    assert_eq!(queued_authorities(&sender_store), [false]);
    assert_eq!(queued_authorities(&parent_store), [true]);
    assert_eq!(
        queued_mailbox_ids(&sender_store).as_slice(),
        std::slice::from_ref(&root_mailbox_id),
    );
    assert_eq!(queued_mailbox_ids(&parent_store), [root_mailbox_id]);
}

/// A resolved recipient with no route is queued, not reported as delivered.
#[tokio::test]
async fn signal_agent_queues_not_routed_recipient() {
    let sender = Uuid::new_v4();
    let (infra, registry, router) = build_infra(sender);
    let recipient = register_agent(&registry, "/parent/child", Some(sender));
    let sender_store = Arc::clone(&infra.event_store);
    let pending_store = Arc::clone(&infra.pending_messages);

    let mailbox = ctx_with_mailbox(infra, recipient);
    let out = SignalAgentTool::new()
        .execute(
            &envelope_for("signal_agent", send_args("/parent/child", "steer", "hi")),
            &mailbox.ctx,
        )
        .await
        .expect("executes");
    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(out.content["delivered"], false);
    assert_eq!(out.content["queued"], true);
    assert_eq!(out.content["to"], recipient.to_string());
    assert_eq!(pending_store.pending_for(recipient), 1);
    assert!(!router.is_routed(recipient), "no route was fabricated");
    assert!(
        sender_store.events().iter().any(|event| matches!(
            event,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE
        )),
        "a queued send must leave a queued audit event",
    );
    assert_eq!(queued_authorities(&sender_store), [false]);
    assert_eq!(queued_authorities(&mailbox.store), [true]);
    let mailbox_id =
        serde_json::to_value(mailbox.binding.mailbox_id()).expect("serialize recipient mailbox id");
    assert_eq!(
        queued_mailbox_ids(&sender_store).as_slice(),
        std::slice::from_ref(&mailbox_id),
    );
    assert_eq!(queued_mailbox_ids(&mailbox.store), [mailbox_id]);
}

/// A closed channel queues after the terminal re-check finds a live recipient.
#[tokio::test]
async fn signal_agent_queues_closed_channel_when_recipient_not_terminal() {
    let sender = Uuid::new_v4();
    let (infra, registry, router) = build_infra(sender);
    let _recipient = register_agent(&registry, "/parent/child", Some(sender));
    let sender_store = Arc::clone(&infra.event_store);
    let pending_store = Arc::clone(&infra.pending_messages);

    let recipient_id = registry
        .read()
        .get_by_path("/parent/child")
        .expect("registered recipient")
        .id;
    let (tx, rx) = inbound_channel(4);
    router.register(recipient_id, tx);
    drop(rx);

    let mailbox = ctx_with_mailbox(infra, recipient_id);
    let out = SignalAgentTool::new()
        .execute(
            &envelope_for("signal_agent", send_args("/parent/child", "update", "hi")),
            &mailbox.ctx,
        )
        .await
        .expect("executes");
    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(out.content["delivered"], false);
    assert_eq!(out.content["queued"], true);
    assert_eq!(pending_store.pending_for(recipient_id), 1);
    assert!(
        sender_store.events().iter().any(|event| matches!(
            event,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE
        )),
        "closed-channel queueing must be audited",
    );
    assert_eq!(queued_authorities(&sender_store), [false]);
    assert_eq!(queued_authorities(&mailbox.store), [true]);
    let mailbox_id =
        serde_json::to_value(mailbox.binding.mailbox_id()).expect("serialize recipient mailbox id");
    assert_eq!(
        queued_mailbox_ids(&sender_store).as_slice(),
        std::slice::from_ref(&mailbox_id),
    );
    assert_eq!(queued_mailbox_ids(&mailbox.store), [mailbox_id]);
}

/// A full bounded channel applies backpressure until the recipient drains.
#[tokio::test]
async fn signal_agent_awaits_full_channel_until_capacity_frees() {
    let sender = Uuid::new_v4();
    let (infra, registry, router) = build_infra(sender);
    let recipient = register_agent(&registry, "/parent/child", Some(sender));
    let (tx, mut rx) = inbound_channel(1);
    router.register(recipient, tx);

    router
        .deliver(
            recipient,
            ChannelMessage {
                id: Uuid::new_v4(),
                sender_id: sender,
                from: "root".to_owned(),
                role: None,
                to_id: recipient,
                content: "first".to_owned(),
                kind: MessageKind::Update,
                seq: None,
                timestamp: Utc::now(),
            },
        )
        .await
        .expect("first deliver");

    let ctx = ctx_with(infra);
    let pending = tokio::spawn(async move {
        SignalAgentTool::new()
            .execute(
                &envelope_for(
                    "signal_agent",
                    send_args("/parent/child", "steer", "second"),
                ),
                &ctx,
            )
            .await
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert!(!pending.is_finished(), "the send must park on backpressure");

    assert_eq!(rx.drain().len(), 1);
    let out = pending.await.expect("join").expect("send");
    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(out.content["seq"], 2);
    let drained = rx.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].content, "second");
}

/// A route lost during backpressure reports terminal registry truth.
#[tokio::test]
async fn signal_agent_rechecks_recipient_when_route_disappears_mid_send() {
    let sender = Uuid::new_v4();
    let (infra, registry, router) = build_infra(sender);
    let recipient = register_agent(&registry, "/parent/child", Some(sender));
    let sender_store = Arc::clone(&infra.event_store);
    let (tx, mut rx) = inbound_channel(1);
    router.register(recipient, tx);

    router
        .deliver(
            recipient,
            ChannelMessage {
                id: Uuid::new_v4(),
                sender_id: sender,
                from: "root".to_owned(),
                role: None,
                to_id: recipient,
                content: "first".to_owned(),
                kind: MessageKind::Update,
                seq: None,
                timestamp: Utc::now(),
            },
        )
        .await
        .expect("first deliver");

    let ctx = ctx_with(infra);
    let pending = tokio::spawn(async move {
        SignalAgentTool::new()
            .execute(
                &envelope_for("signal_agent", send_args("/parent/child", "steer", "late")),
                &ctx,
            )
            .await
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert!(!pending.is_finished(), "the send must park on backpressure");

    router.deregister(recipient);
    registry
        .write()
        .mark_completed(recipient)
        .expect("complete recipient");
    assert_eq!(rx.drain().len(), 1, "free capacity for parked sender");

    let out = pending.await.expect("join").expect("send");
    assert!(out.is_error());
    assert_eq!(out.content["delivered"], false);
    assert_eq!(out.content["recipient_status"], "completed");
    let message = out.content["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("already finished") && message.contains("completed at"),
        "the completion race must surface recorded registry truth: {message}",
    );
    assert!(
        sender_store.events().is_empty(),
        "a send rejected after lifecycle re-check must not emit Sent",
    );
}
