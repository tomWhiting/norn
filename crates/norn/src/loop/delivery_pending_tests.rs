#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;

use crate::agent::{PendingAgentMessage, PendingAgentMessages, PendingMailboxLease};
use crate::integration::hooks::{Hook, HookRegistry, SessionEventHook};
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::provider::request::Message;
use crate::session::events::SessionEvent;
use crate::session::persistence::SessionPersistError;
use crate::session::store::{DurabilityPolicy, EventStore, JsonlSink, PersistenceSink};
use crate::session::{MailboxId, SessionBinding};

use super::delivery_pending::flush_pending_agent_messages;
use super::loop_context::LoopContext;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct BlockOnUserMessage {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl SessionEventHook for BlockOnUserMessage {
    async fn on_event(&self, event: &SessionEvent) {
        if matches!(event, SessionEvent::UserMessage { .. }) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

fn message(recipient: uuid::Uuid, content: &str, seq: Option<u64>) -> ChannelMessage {
    ChannelMessage {
        id: uuid::Uuid::new_v4(),
        sender_id: uuid::Uuid::new_v4(),
        from: "/root/sender".to_owned(),
        role: Some("worker".to_owned()),
        to_id: recipient,
        content: content.to_owned(),
        kind: MessageKind::Update,
        seq,
        timestamp: chrono::Utc::now(),
    }
}

struct TestMailbox {
    store: Arc<EventStore>,
    mailbox_id: MailboxId,
    lease: Arc<PendingMailboxLease>,
}

fn register_mailbox(
    pending: &PendingAgentMessages,
    recipient: uuid::Uuid,
    store: Arc<EventStore>,
) -> Result<TestMailbox, crate::error::SessionError> {
    let mailbox_id = SessionBinding::ephemeral_root().mailbox_id();
    let lease = Arc::new(PendingMailboxLease::new());
    pending.register_child_mailbox(recipient, mailbox_id, &store, &lease)?;
    Ok(TestMailbox {
        store,
        mailbox_id,
        lease,
    })
}

fn queue(
    pending: &PendingAgentMessages,
    mailbox: &TestMailbox,
    message: ChannelMessage,
) -> Result<(), crate::error::SessionError> {
    let queued_at = message.timestamp;
    let mut record = PendingAgentMessage::new(message, "/root/recipient".to_owned(), queued_at);
    pending.persist_for_registered_store(mailbox.store.as_ref(), &mut record)?;
    Ok(())
}

fn context(agent_id: uuid::Uuid, pending: Arc<PendingAgentMessages>) -> LoopContext {
    let mut context = LoopContext::new("system");
    context.agent_id = Some(agent_id);
    context.pending_agent_messages = Some(pending);
    context
}

fn user_message_count(events: &[SessionEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, SessionEvent::UserMessage { .. }))
        .count()
}

#[tokio::test]
async fn cancellation_after_authoritative_append_cannot_redeliver_pending_message() -> TestResult {
    let agent_id = uuid::Uuid::new_v4();
    let pending = Arc::new(PendingAgentMessages::new());
    let mailbox = register_mailbox(&pending, agent_id, Arc::new(EventStore::new()))?;
    let queued_message = message(agent_id, "cancel seam", Some(7));
    let queued_message_id = queued_message.id;
    queue(&pending, &mailbox, queued_message)?;

    let entered = Arc::new(tokio::sync::Notify::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(BlockOnUserMessage {
        entered: Arc::clone(&entered),
    })));
    let mut loop_context = context(agent_id, Arc::clone(&pending));
    loop_context.hooks = Some(Arc::new(hooks));
    let mut prompt_messages: Vec<Message> = Vec::new();
    let mut flush = Box::pin(flush_pending_agent_messages(
        mailbox.store.as_ref(),
        &mut prompt_messages,
        &loop_context,
        None,
    ));

    tokio::select! {
        () = entered.notified() => {}
        result = flush.as_mut() => {
            return Err(std::io::Error::other(
                format!("blocking hook unexpectedly returned: {result:?}"),
            ).into());
        }
    }
    drop(flush);

    assert!(
        pending.is_empty(),
        "authoritative append consumes the queue"
    );
    assert_eq!(user_message_count(&mailbox.store.events()), 1);
    let delivery = mailbox
        .store
        .events()
        .into_iter()
        .find(|event| matches!(event, SessionEvent::UserMessage { .. }))
        .ok_or_else(|| std::io::Error::other("stable delivery row missing"))?;
    assert_eq!(
        delivery.base().id.as_str(),
        format!("norn:pending-agent-message:delivered:{queued_message_id}"),
    );
    assert_eq!(prompt_messages.len(), 1);

    let rebuilt = Arc::new(PendingAgentMessages::from_events(
        agent_id,
        mailbox.mailbox_id,
        &mailbox.store.events(),
    )?);
    assert!(
        rebuilt.is_empty(),
        "restart must observe durable consumption"
    );
    let mut replay_messages = Vec::new();
    let ids = flush_pending_agent_messages(
        mailbox.store.as_ref(),
        &mut replay_messages,
        &context(agent_id, rebuilt),
        None,
    )
    .await?;
    assert!(ids.is_empty());
    assert!(replay_messages.is_empty());
    assert_eq!(user_message_count(&mailbox.store.events()), 1);
    Ok(())
}

#[tokio::test]
async fn jsonl_ambiguous_user_append_retries_same_event_without_duplicate() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("pending.jsonl");
    let agent_id = uuid::Uuid::new_v4();
    let pending = Arc::new(PendingAgentMessages::new());
    let seed_sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    let seed_mailbox = register_mailbox(
        &pending,
        agent_id,
        Arc::new(EventStore::with_sink(Box::new(seed_sink))),
    )?;
    queue(
        &pending,
        &seed_mailbox,
        message(agent_id, "ambiguous", Some(11)),
    )?;
    let queued_events = seed_mailbox.store.events();
    let mailbox_id = seed_mailbox.mailbox_id;
    let closed = pending
        .close_child_mailbox(agent_id, &seed_mailbox.lease)
        .ok_or_else(|| std::io::Error::other("close seed mailbox"))?;
    drop(closed);
    drop(seed_mailbox);

    let mut failing_sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    failing_sink.fail_after_write_once();
    let store = Arc::new(EventStore::with_sink_and_events(
        Box::new(failing_sink),
        queued_events,
    ));
    let lease = Arc::new(PendingMailboxLease::new());
    pending.register_child_mailbox(agent_id, mailbox_id, &store, &lease)?;
    let loop_context = context(agent_id, Arc::clone(&pending));
    let mut messages = Vec::new();

    let first =
        flush_pending_agent_messages(store.as_ref(), &mut messages, &loop_context, None).await?;
    assert_eq!(
        first.len(),
        1,
        "the exact retry reconciles ambiguity inline"
    );
    assert!(pending.is_empty());
    assert_eq!(messages.len(), 1);

    let durable_after_error = crate::session::persistence::io::read_session_events_from(
        BufReader::new(std::fs::File::open(&path)?),
        "pending",
    )?;
    assert_eq!(user_message_count(&durable_after_error.events), 1);

    let second =
        flush_pending_agent_messages(store.as_ref(), &mut messages, &loop_context, None).await?;
    assert!(second.is_empty());
    assert_eq!(user_message_count(&store.events()), 1);

    let replay = crate::session::persistence::io::read_session_events_from(
        BufReader::new(std::fs::File::open(&path)?),
        "pending",
    )?;
    assert_eq!(user_message_count(&replay.events), 1);
    Ok(())
}

#[tokio::test]
async fn jsonl_restart_reconstructs_ambiguous_user_append_as_consumed() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("restart.jsonl");
    let agent_id = uuid::Uuid::new_v4();
    let pending = Arc::new(PendingAgentMessages::new());
    let seed_sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    let mailbox = register_mailbox(
        &pending,
        agent_id,
        Arc::new(EventStore::with_sink(Box::new(seed_sink))),
    )?;
    queue(&pending, &mailbox, message(agent_id, "restart", Some(19)))?;
    let prepared = pending
        .prepare_next_delivery(agent_id, mailbox.store.last_event_id())
        .ok_or_else(|| std::io::Error::other("queued delivery fixture missing"))?;
    let mailbox_id = mailbox.mailbox_id;
    drop(mailbox);

    // Model a hard process stop after the exact User row reached JSONL but
    // before EventStore memory, queue consumption, hooks, or audits ran.
    let mut killed_process_sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    killed_process_sink.persist(&prepared.delivery_event)?;
    drop(killed_process_sink);
    drop(pending);

    let replay = crate::session::persistence::io::read_session_events_from(
        BufReader::new(std::fs::File::open(&path)?),
        "restart",
    )?;
    assert_eq!(user_message_count(&replay.events), 1);
    let rebuilt = PendingAgentMessages::from_events(agent_id, mailbox_id, &replay.events)?;
    assert!(rebuilt.is_empty());
    let provider_messages = crate::session::conversion::prompt_events_to_messages(&replay.events);
    assert_eq!(
        provider_messages
            .iter()
            .filter(|message| message.role == crate::provider::request::MessageRole::User)
            .count(),
        1,
        "restart projects exactly one model-visible User message",
    );
    Ok(())
}

struct FailSecondPendingUserOnce {
    remaining_failures: AtomicUsize,
    persisted: Arc<Mutex<Vec<SessionEvent>>>,
}

impl PersistenceSink for FailSecondPendingUserOnce {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        if let SessionEvent::UserMessage { content, .. } = event
            && content.contains("second")
            && self
                .remaining_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
        {
            return Err(SessionPersistError::Io(std::io::Error::other(
                "injected second pending-message failure",
            )));
        }
        self.persisted.lock().push(event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn custom_sink_multi_message_failure_consumes_only_committed_prefix() -> TestResult {
    let persisted = Arc::new(Mutex::new(Vec::new()));
    let agent_id = uuid::Uuid::new_v4();
    let pending = Arc::new(PendingAgentMessages::new());
    let mailbox = register_mailbox(
        &pending,
        agent_id,
        Arc::new(EventStore::with_sink(Box::new(FailSecondPendingUserOnce {
            remaining_failures: AtomicUsize::new(2),
            persisted: Arc::clone(&persisted),
        }))),
    )?;
    for (content, seq) in [("first", 1), ("second", 2)] {
        queue(&pending, &mailbox, message(agent_id, content, Some(seq)))?;
    }
    let loop_context = context(agent_id, Arc::clone(&pending));
    let mut messages = Vec::new();

    let first =
        flush_pending_agent_messages(mailbox.store.as_ref(), &mut messages, &loop_context, None)
            .await;
    assert!(first.is_err());
    assert_eq!(pending.pending_for(agent_id), 1);
    assert_eq!(messages.len(), 1);
    assert_eq!(user_message_count(&mailbox.store.events()), 1);

    let second =
        flush_pending_agent_messages(mailbox.store.as_ref(), &mut messages, &loop_context, None)
            .await?;
    assert_eq!(second.len(), 1);
    assert!(pending.is_empty());
    assert_eq!(messages.len(), 2);
    assert_eq!(user_message_count(&mailbox.store.events()), 2);
    assert_eq!(user_message_count(&persisted.lock()), 2);
    Ok(())
}

#[tokio::test]
async fn flush_rejects_a_different_store_without_delivery_or_queue_loss() -> TestResult {
    let agent_id = uuid::Uuid::new_v4();
    let pending = Arc::new(PendingAgentMessages::new());
    let mailbox = register_mailbox(&pending, agent_id, Arc::new(EventStore::new()))?;
    queue(
        &pending,
        &mailbox,
        message(agent_id, "stay on the bound timeline", Some(23)),
    )?;
    let wrong_store = EventStore::new();
    let mut messages = Vec::new();

    let result = flush_pending_agent_messages(
        &wrong_store,
        &mut messages,
        &context(agent_id, Arc::clone(&pending)),
        None,
    )
    .await;

    assert!(result.is_err());
    assert_eq!(pending.pending_for(agent_id), 1);
    assert!(messages.is_empty());
    assert!(wrong_store.is_empty());
    assert_eq!(mailbox.store.len(), 1, "only the stable queue row exists");
    Ok(())
}
