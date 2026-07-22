use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::*;
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode};
use crate::r#loop::conversation_state::ConversationRequestState;
use crate::provider::tools::ProviderCapabilities;
use crate::session::context_edit::ContextEdits;
use crate::session::events::{EventUsage, ProviderEpochBoundaryReason};
use crate::session::persistence::SessionPersistError;
use crate::session::store::PersistenceSink;
use crate::session::{committed_response_publication, response_publication_fixture};
use crate::system_prompt::{PromptPlan, PromptSource};

fn user_event(content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: content.to_owned(),
    }
}

fn append_stored_assistant(
    store: &EventStore,
    response_id: &str,
) -> Result<(), crate::error::SessionError> {
    let fixture = response_publication_fixture(store.last_event_id(), true).map_err(|_source| {
        crate::error::SessionError::StorageError {
            reason: "failed to encode the provider-state provenance fixture".to_owned(),
        }
    })?;
    let assistant = SessionEvent::AssistantMessage {
        base: fixture.assistant_base,
        response_items: Vec::new(),
        content: format!("answer from {response_id}"),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    };
    let publication =
        committed_response_publication(fixture.boundary, fixture.provenance, assistant)?;
    store.append_batch(&publication)?;
    Ok(())
}

fn threaded_state(initial: InitialMessages) -> Result<ConversationRequestState, NornError> {
    Ok(ConversationRequestState::with_prompt_seed(
        &AgentLoopConfig {
            conversation_state: ConversationStateMode::ProviderThreaded,
            ..AgentLoopConfig::default()
        },
        ProviderCapabilities::openai_responses(),
        initial.prefix_len,
        initial.prompt_seed_fingerprint,
        initial.response_thread_anchor,
    )?)
}

#[test]
fn initial_messages_materialize_the_ordered_stable_authority_plan() -> Result<(), NornError> {
    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, "product");
    plan.set(PromptSource::OperatorProfile, "operator");
    plan.set(PromptSource::WorkspaceProfile, "repository");
    plan.set(PromptSource::SkillCatalogPolicy, "skills");
    let mut loop_context = LoopContext::new("legacy compatibility view");
    loop_context.install_stable_prompt_plan(plan);

    let initial = build_initial_messages(Some("task"), &loop_context, &EventStore::new())?;
    assert_eq!(initial.prefix_len, 4);
    let roles = initial
        .messages
        .iter()
        .map(|message| message.role.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        [
            MessageRole::System,
            MessageRole::System,
            MessageRole::Developer,
            MessageRole::User,
            MessageRole::User,
        ]
    );
    let contents = initial
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        contents,
        ["product", "skills", "operator", "repository", "task"]
    );
    Ok(())
}

#[test]
fn context_edited_prompt_respects_provider_epoch_boundaries() -> Result<(), NornError> {
    let store = EventStore::new();
    append_stored_assistant(&store, "response_before_adoption")?;
    store.append(SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(store.last_event_id()),
        reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
    })?;
    let mut loop_context = LoopContext::new("system");
    loop_context.context_edits = Some(ContextEdits::new());

    let initial = build_initial_messages(Some("next"), &loop_context, &store)?;
    let state = threaded_state(initial)?;
    assert!(state.previous_response_id().is_none());

    append_stored_assistant(&store, "response_after_adoption")?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(store.last_event_id()),
        content: "queued after the new response".to_owned(),
    })?;
    let initial = build_initial_messages(Some("next"), &loop_context, &store)?;
    let messages = initial.messages.clone();
    let state = threaded_state(initial)?;
    assert_eq!(
        state.previous_response_id().as_deref(),
        Some("response_after_adoption"),
    );
    let request_messages = state.request_messages(&messages);
    assert_eq!(request_messages.len(), 3);
    assert_eq!(request_messages[0].role, MessageRole::System);
    assert_eq!(
        request_messages[1].content.as_deref(),
        Some("queued after the new response"),
    );
    assert_eq!(request_messages[2].content.as_deref(), Some("next"));
    Ok(())
}

#[test]
fn tracker_free_initial_prompt_uses_persisted_compaction_view() -> Result<(), NornError> {
    let store = EventStore::new();
    let compacted = store.append(user_event("old detail"))?;
    let suppressed = store.append(user_event("noisy aside"))?;
    store.append(user_event("kept detail"))?;
    let mut persisted = ContextEdits::new();
    persisted.summarize(
        &store,
        vec![compacted.clone()],
        "old turn summary".to_owned(),
    )?;
    persisted.suppress(&store, suppressed.clone())?;
    persisted.inject(&store, user_event("operator note"))?;

    let loop_context = LoopContext::new("system");
    assert!(loop_context.context_edits.is_none());
    let initial = build_initial_messages(None, &loop_context, &store)?;
    let contents: Vec<_> = initial
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect();

    assert!(contents.contains(&"kept detail"));
    assert!(contents.contains(&"Prior conversation compaction summary:\nold turn summary"));
    assert!(contents.contains(&"operator note"));
    assert!(!contents.contains(&"old detail"));
    assert!(!contents.contains(&"noisy aside"));
    let canonical = store.events();
    assert!(canonical.iter().any(|event| event.base().id == compacted));
    assert!(canonical.iter().any(|event| event.base().id == suppressed));
    Ok(())
}

/// A sink whose `persist` blocks until a handshake task — which can
/// only run if the executor stays live while the write blocks —
/// releases it.
struct HandshakeSink {
    entered: Arc<AtomicBool>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

impl PersistenceSink for HandshakeSink {
    fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
        self.entered.store(true, Ordering::SeqCst);
        self.release_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|e| {
                SessionPersistError::Io(std::io::Error::other(format!(
                    "executor stalled: no task released the blocking persist: {e}"
                )))
            })?;
        Ok(())
    }
}

/// The off-executor guarantee: with exactly one worker thread, a
/// blocking sink write inside `append_and_notify` must not stall the
/// runtime. The sink blocks until a *separately spawned task* releases
/// it — with an inline append on the single worker that task could
/// never run and the handshake would time out; `block_in_place`
/// hands the worker's queue back to the runtime, so the release task
/// proceeds while the write blocks.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn append_and_notify_does_not_stall_other_tasks_on_the_worker()
-> Result<(), Box<dyn std::error::Error>> {
    let entered = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = EventStore::with_sink(Box::new(HandshakeSink {
        entered: Arc::clone(&entered),
        release_rx,
    }));

    let entered_for_task = Arc::clone(&entered);
    let releaser = tokio::spawn(async move {
        while !entered_for_task.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        release_tx.send(())
    });

    let id = append_and_notify(&store, user_event("hello"), None).await?;
    assert_eq!(store.len(), 1);
    assert_eq!(store.last_event_id(), Some(id));
    releaser.await??;
    Ok(())
}

/// Error-surfacing parity: a failing sink persist comes back as the
/// same typed `StorageError` the inline append produced, and the
/// event is not in the in-memory store.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn append_and_notify_surfaces_sink_errors_unchanged() {
    struct FailingSink;
    impl PersistenceSink for FailingSink {
        fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
            Err(SessionPersistError::Io(std::io::Error::other("disk full")))
        }
    }
    let store = EventStore::with_sink(Box::new(FailingSink));

    let result = append_and_notify(&store, user_event("lost"), None).await;
    assert!(
        matches!(result, Err(SessionError::StorageError { .. })),
        "expected StorageError, got {result:?}",
    );
    assert!(store.is_empty(), "failed append must not reach memory");
}

/// The flavor gate: on a current-thread runtime `block_in_place`
/// panics by contract, so the append must take the inline arm. A
/// plain `#[tokio::test]` runs on the current-thread flavor — this
/// test passing at all proves the gate.
#[tokio::test]
async fn append_and_notify_runs_inline_on_current_thread_runtime() -> Result<(), SessionError> {
    let store = EventStore::new();
    append_and_notify(&store, user_event("inline"), None).await?;
    assert_eq!(store.len(), 1);
    Ok(())
}
