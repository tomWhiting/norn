//! Failure, cancellation, identity and bounded-sweep tests for owned channel delivery.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::FutureExt;
use parking_lot::Mutex;

use super::*;
use crate::integration::hooks::{Hook, SessionEventHook};
use crate::integration::{McpChannelOverflow, McpChannelPolicy, McpChannelRefusal};
use crate::session::SessionPersistError;
use crate::session::store::PersistenceSink;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn owner() -> Result<(LoopContext, McpChannelHost), Box<dyn std::error::Error + Send + Sync>> {
    let mut context = LoopContext::new("system");
    context.agent_id = Some(Uuid::new_v4());
    let host = context.install_mcp_channel_inbox(McpChannelLimits::new(4, 8192)?)?;
    Ok((context, host))
}

fn source(
    host: &McpChannelHost,
    policy: McpChannelPolicy,
    generation: u64,
) -> Result<impl Fn(&str) + Send + Sync + 'static, McpChannelError> {
    let source = host
        .attachment(policy, McpChannelOverflow::RejectNew)
        .bind(format!("source-{generation}"), generation)?;
    source.negotiated()?;
    source.activate()?;
    Ok(move |content: &str| source.receive(serde_json::json!({"content":content})))
}

#[tokio::test]
async fn installation_and_delivery_require_the_same_real_identity() -> TestResult {
    let mut missing = LoopContext::new("system");
    assert!(
        missing
            .install_mcp_channel_inbox(McpChannelLimits::new(1, 1024)?)
            .is_err()
    );
    let (mut context, host) = owner()?;
    assert_eq!(Some(host.status().recipient_id), context.agent_id);
    assert!(
        context
            .install_mcp_channel_inbox(McpChannelLimits::new(1, 1024)?)
            .is_err()
    );
    let send = source(&host, McpChannelPolicy::Wake, 1)?;
    send("bound to original recipient");
    context.agent_id = Some(Uuid::new_v4());
    let store = EventStore::new();
    assert!(
        flush_mcp_channel_messages(&store, &mut Vec::new(), &mut context, false, None)
            .await
            .is_err()
    );
    assert!(store.events().is_empty());
    assert_eq!(host.status().retained_messages, 1);
    Ok(())
}

#[tokio::test]
async fn busy_delivery_consumes_only_wake_and_new_turn_accepts_next_turn() -> TestResult {
    let (mut context, host) = owner()?;
    let next = source(&host, McpChannelPolicy::NextTurn, 1)?;
    let held = source(&host, McpChannelPolicy::Hold, 2)?;
    let wake = source(&host, McpChannelPolicy::Wake, 3)?;
    next("next");
    held("held");
    assert!(
        context
            .mcp_channel_session
            .as_ref()
            .ok_or("missing session")?
            .wake_ready()
            .now_or_never()
            .is_none()
    );
    wake("wake");
    let store = EventStore::new();
    let mut messages = Vec::new();
    assert_eq!(
        flush_mcp_channel_messages(&store, &mut messages, &mut context, true, None)
            .await?
            .len(),
        1
    );
    assert!(
        messages[0]
            .content
            .as_deref()
            .is_some_and(|text| text.contains("wake"))
    );
    assert_eq!(host.status().retained_messages, 2);
    assert_eq!(
        flush_mcp_channel_messages(&store, &mut messages, &mut context, false, None)
            .await?
            .len(),
        1
    );
    assert_eq!(host.status().retained_messages, 1);
    assert_eq!(
        context
            .mcp_channel_session
            .as_ref()
            .ok_or("missing session")?
            .held_message_ids()
            .len(),
        1
    );
    Ok(())
}

struct ControlledSink {
    fail: Arc<AtomicBool>,
    attempts: Arc<Mutex<Vec<SessionEvent>>>,
}

impl PersistenceSink for ControlledSink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        self.attempts.lock().push(event.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(SessionPersistError::Io(std::io::Error::other(
                "injected channel sink failure",
            )));
        }
        Ok(())
    }
}

#[tokio::test]
async fn failed_append_retains_exact_prepared_event_and_full_charge() -> TestResult {
    let mut context = LoopContext::new("system");
    context.agent_id = Some(Uuid::new_v4());
    let host = context.install_mcp_channel_inbox(McpChannelLimits::new(1, 4096)?)?;
    let send = source(&host, McpChannelPolicy::Wake, 1)?;
    send("must survive failure");
    let charged = host.status().retained_bytes;
    let fail = Arc::new(AtomicBool::new(true));
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let store = EventStore::with_sink(Box::new(ControlledSink {
        fail: Arc::clone(&fail),
        attempts: Arc::clone(&attempts),
    }));
    let mut messages = Vec::new();
    assert!(
        flush_mcp_channel_messages(&store, &mut messages, &mut context, false, None)
            .await
            .is_err()
    );
    assert_eq!(host.status().retained_bytes, charged);
    assert_eq!(host.status().retained_messages, 1);
    send("cannot displace retained claim");
    assert_eq!(
        host.status()
            .last_rejection
            .map(|rejection| rejection.reason),
        Some(McpChannelRefusal::FullCount)
    );
    let prepared = serde_json::to_value(attempts.lock().first().ok_or("missing sink attempt")?)?;
    fail.store(false, Ordering::SeqCst);
    assert_eq!(
        flush_mcp_channel_messages(&store, &mut messages, &mut context, false, None)
            .await?
            .len(),
        1
    );
    assert_eq!(host.status().retained_messages, 0);
    assert_eq!(host.status().retained_bytes, 0);
    for event in attempts.lock().iter() {
        assert_eq!(serde_json::to_value(event)?, prepared);
    }
    assert_eq!(store.events().len(), 1);
    assert_eq!(messages.len(), 1);
    Ok(())
}

struct AmbiguousSink {
    first: bool,
    written: Arc<Mutex<Vec<SessionEvent>>>,
}

impl PersistenceSink for AmbiguousSink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        if self.first {
            self.first = false;
            self.written.lock().push(event.clone());
            return Err(SessionPersistError::Io(std::io::Error::other(
                "written, acknowledgement failed",
            )));
        }
        let written = self.written.lock();
        let Some(first) = written.first() else {
            return Err(SessionPersistError::Io(std::io::Error::other(
                "missing ambiguous event",
            )));
        };
        if serde_json::to_value(first)? != serde_json::to_value(event)? {
            return Err(SessionPersistError::Io(std::io::Error::other(
                "retry changed event identity or content",
            )));
        }
        Ok(())
    }
}

#[tokio::test]
async fn ambiguous_append_reconciles_one_exact_event() -> TestResult {
    let (mut context, host) = owner()?;
    let send = source(&host, McpChannelPolicy::Wake, 1)?;
    send("one event");
    let written = Arc::new(Mutex::new(Vec::new()));
    let store = EventStore::with_sink(Box::new(AmbiguousSink {
        first: true,
        written: Arc::clone(&written),
    }));
    let mut messages = Vec::new();
    flush_mcp_channel_messages(&store, &mut messages, &mut context, false, None).await?;
    assert_eq!(written.lock().len(), 1);
    assert_eq!(store.events().len(), 1);
    assert_eq!(host.status().retained_messages, 0);
    Ok(())
}

struct BlockingHook {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl SessionEventHook for BlockingHook {
    async fn on_event(&self, event: &SessionEvent) {
        if matches!(event, SessionEvent::UserMessage { .. }) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

#[tokio::test]
async fn cancelling_a_hook_after_persistence_cannot_redeliver() -> TestResult {
    let (mut context, host) = owner()?;
    let (events, mut receiver) = tokio::sync::broadcast::channel(4);
    let sender = AgentEventSender::new(events, host.status().recipient_id, "root".to_owned());
    let send = source(&host, McpChannelPolicy::Wake, 1)?;
    send("persist before hook");
    let entered = Arc::new(tokio::sync::Notify::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(BlockingHook {
        entered: Arc::clone(&entered),
    })));
    context.hooks = Some(Arc::new(hooks));
    let store = EventStore::new();
    let mut messages = Vec::new();
    let mut flush = Box::pin(flush_mcp_channel_messages(
        &store,
        &mut messages,
        &mut context,
        false,
        Some(&sender),
    ));
    tokio::select! {
        () = entered.notified() => {},
        result = flush.as_mut() => return Err(format!("hook unexpectedly returned: {result:?}").into()),
    }
    drop(flush);
    assert_eq!(host.status().retained_messages, 0);
    assert_eq!(messages.len(), 1);
    let observed = receiver.try_recv()?;
    let crate::provider::AgentEventKind::McpChannel(delivered) = observed.event else {
        return Err("external input was not published as a distinct channel event".into());
    };
    assert_eq!(delivered.recipient_id, host.status().recipient_id);
    assert_eq!(delivered.source, "source-1");
    assert_eq!(delivered.generation, 1);
    assert_eq!(delivered.sequence, 1);
    assert_eq!(delivered.content, "persist before hook");
    assert!(store.event_by_id(&delivered.event_id).is_some());
    assert!(!format!("{delivered:?}").contains("persist before hook"));
    context.hooks = None;
    assert!(
        flush_mcp_channel_messages(&store, &mut messages, &mut context, false, None)
            .await?
            .is_empty()
    );
    assert_eq!(store.events().len(), 1);
    Ok(())
}

struct RefillHook {
    send: Box<dyn Fn(&str) + Send + Sync>,
}

#[async_trait::async_trait]
impl SessionEventHook for RefillHook {
    async fn on_event(&self, event: &SessionEvent) {
        if matches!(event, SessionEvent::UserMessage { .. }) {
            (self.send)("arrived during this sweep");
        }
    }
}

#[tokio::test]
async fn continuously_refilled_inbox_cannot_extend_one_boundary_sweep() -> TestResult {
    let (mut context, host) = owner()?;
    let send = source(&host, McpChannelPolicy::Wake, 1)?;
    send("initial work");
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(RefillHook {
        send: Box::new(send),
    })));
    context.hooks = Some(Arc::new(hooks));
    let store = EventStore::new();
    assert_eq!(
        flush_mcp_channel_messages(&store, &mut Vec::new(), &mut context, true, None)
            .await?
            .len(),
        1
    );
    assert_eq!(host.status().retained_messages, 1);
    Ok(())
}

#[tokio::test]
async fn idle_linger_wakes_on_push_without_polling() -> TestResult {
    use crate::r#loop::linger::{
        BoundaryOutcome, LingerPolicy, StopBoundary, resolve_stop_boundary,
    };
    let (mut context, host) = owner()?;
    let send = source(&host, McpChannelPolicy::Wake, 1)?;
    let store = EventStore::new();
    let mut messages = Vec::new();
    let mut follow_up = Vec::new();
    let mut boundary = Box::pin(resolve_stop_boundary(StopBoundary {
        store: &store,
        messages: &mut messages,
        inbound: None,
        follow_up_buffer: &mut follow_up,
        loop_context: &mut context,
        linger: Some(LingerPolicy {
            deadline: std::time::Duration::from_secs(5),
        }),
        cancel: None,
        event_tx: None,
    }));
    assert!(boundary.as_mut().now_or_never().is_none());
    send("wake a waiting session");
    assert!(matches!(boundary.await?, BoundaryOutcome::Continue));
    assert_eq!(messages.len(), 1);
    assert_eq!(host.status().retained_messages, 0);
    Ok(())
}
