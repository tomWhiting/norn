//! Real runner publication receipts bind opening and assistant records without provider IDs.

use std::collections::HashSet;

use tokio::sync::broadcast;
use uuid::Uuid;

use super::*;
use crate::provider::agent_event::{AgentEventKind, ObservationScope, PublicationResolution};
use crate::session::branch::SessionBinding;
use crate::session_view::HistoryPosition;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn generic_response_publishes_exact_records_without_provider_response_id() -> TestResult {
    let store = EventStore::new();
    let agent = Uuid::new_v4();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
    let (tx, mut receiver) = broadcast::channel(8);
    let (sender, execution) = AgentEventSender::new(tx, agent, "root".to_owned())
        .observe_execution(&store, &source, Uuid::new_v4())?;
    let provider = MockProvider::new(vec![vec![
        text_delta("one answer"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();
    let mut context = LoopContext::new("system");
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "one prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: Some(&sender),
        inbound: None,
        loop_context: &mut context,
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));
    let Some(PublicationResolution::Accepted(opening)) = execution.opening_input() else {
        return Err("opening receipt was not accepted".into());
    };
    let HistoryPosition::Event {
        event_id: opening_id,
        ..
    } = opening.cursor().position()
    else {
        return Err("opening cursor had no event".into());
    };
    assert!(store.events().iter().any(|event| event.base().id == *opening_id && matches!(event, SessionEvent::UserMessage { content, .. } if content == "one prompt")));
    let mut attempts = HashSet::new();
    while let Ok(event) = receiver.try_recv() {
        let AgentEventKind::Observed(observed) = event.event else {
            return Err("runner event lost its execution scope".into());
        };
        if let ObservationScope::Attempt(ticket) = observed.scope() {
            assert!(ticket.execution_observation().same_execution(&execution));
            let Some(PublicationResolution::Accepted(record)) = ticket.resolution() else {
                return Err("assistant ticket was not accepted".into());
            };
            let HistoryPosition::Event { event_id, .. } = record.cursor().position() else {
                return Err("assistant cursor had no event".into());
            };
            assert!(store.events().iter().any(|event| event.base().id == *event_id && matches!(event, SessionEvent::AssistantMessage { content, response_id: None, .. } if content == "one answer")));
            attempts.insert(ticket.attempt().clone());
        }
    }
    assert_eq!(attempts.len(), 1);
    Ok(())
}

use crate::integration::hooks::{
    Hook, HookOutcome, HookRegistry, SessionEventHook, UserPromptHook,
};
use crate::provider::agent_event::{AttemptObservation, ExecutionObservation, PublicationEnd};
use crate::session::events::EventId;

struct ObservationFixture {
    sender: AgentEventSender,
    execution: ExecutionObservation,
    receiver: broadcast::Receiver<crate::provider::agent_event::AgentEvent>,
}

impl ObservationFixture {
    fn new(store: &EventStore) -> Result<Self, Box<dyn std::error::Error>> {
        let agent = Uuid::new_v4();
        let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
        Self::bound(store, &source)
    }

    fn bound(
        store: &EventStore,
        source: &crate::session_view::ViewSource,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Each fixture emits at most two responses and one retry; overflow is a test error.
        let (tx, receiver) = broadcast::channel(32);
        let (sender, execution) = AgentEventSender::new(tx, source.agent_id, "root".to_owned())
            .observe_execution(store, source, Uuid::new_v4())?;
        Ok(Self {
            sender,
            execution,
            receiver,
        })
    }

    fn attempts(&mut self) -> Result<Vec<AttemptObservation>, Box<dyn std::error::Error>> {
        let mut tickets = Vec::new();
        let mut keys = HashSet::new();
        loop {
            let event = match self.receiver.try_recv() {
                Ok(event) => event,
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(error) => return Err(error.into()),
            };
            let AgentEventKind::Observed(observed) = event.event else {
                return Err("execution event had no scoped envelope".into());
            };
            assert!(
                observed
                    .scope()
                    .execution_observation()
                    .same_execution(&self.execution)
            );
            if let ObservationScope::Attempt(ticket) = observed.scope()
                && keys.insert(ticket.attempt().clone())
            {
                tickets.push(ticket.clone());
            }
        }
        Ok(tickets)
    }
}

async fn observed_step(
    fixture: &ObservationFixture,
    store: &EventStore,
    provider: &dyn Provider,
    context: &mut LoopContext,
    prompt: &str,
) -> Result<AgentStepResult, NornError> {
    let executor = MockToolExecutor::empty();
    run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt: prompt,
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: Some(&fixture.sender),
        inbound: None,
        loop_context: context,
        cancel: None,
    })
    .await
}

fn accepted_event(
    resolution: Option<&PublicationResolution>,
) -> Result<EventId, Box<dyn std::error::Error>> {
    let Some(PublicationResolution::Accepted(record)) = resolution else {
        return Err("publication did not resolve as accepted".into());
    };
    let HistoryPosition::Event { event_id, .. } = record.cursor().position() else {
        return Err("accepted record had an empty cursor".into());
    };
    Ok(event_id.clone())
}

#[derive(Clone, Copy)]
enum PausedEvent {
    Opening,
    Assistant,
}

struct PauseAfterAcceptance {
    kind: PausedEvent,
    reached: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl SessionEventHook for PauseAfterAcceptance {
    async fn on_event(&self, event: &SessionEvent) {
        let selected = matches!(
            (self.kind, event),
            (PausedEvent::Opening, SessionEvent::UserMessage { .. })
                | (
                    PausedEvent::Assistant,
                    SessionEvent::AssistantMessage { .. }
                )
        );
        if selected {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

#[tokio::test]
async fn cancellation_inside_session_hooks_keeps_accepted_opening_and_assistant() -> TestResult {
    for kind in [PausedEvent::Opening, PausedEvent::Assistant] {
        let store = EventStore::new();
        let mut fixture = ObservationFixture::new(&store)?;
        let provider = MockProvider::new(vec![vec![
            text_delta("accepted answer"),
            done_event(StopReason::EndTurn),
        ]]);
        let reached = Arc::new(tokio::sync::Notify::new());
        let mut hooks = HookRegistry::new();
        hooks.register(Hook::SessionEvent(Box::new(PauseAfterAcceptance {
            kind,
            reached: Arc::clone(&reached),
        })));
        let mut context = LoopContext::new("system");
        context.hooks = Some(Arc::new(hooks));
        let mut running = Box::pin(observed_step(
            &fixture,
            &store,
            &provider,
            &mut context,
            "accepted prompt",
        ));
        tokio::select! {
            () = reached.notified() => {},
            result = &mut running => return Err(format!("step ended before selected session hook: {result:?}").into()),
        }
        drop(running);
        let opening = accepted_event(fixture.execution.opening_input())?;
        assert!(
            store
                .events()
                .iter()
                .any(|event| event.base().id == opening)
        );
        let tickets = fixture.attempts()?;
        match kind {
            PausedEvent::Opening => {
                assert!(tickets.is_empty());
                assert_eq!(provider.call_count(), 0);
            }
            PausedEvent::Assistant => {
                assert_eq!(tickets.len(), 1);
                let assistant = accepted_event(tickets[0].resolution())?;
                assert!(
                    store
                        .events()
                        .iter()
                        .any(|event| event.base().id == assistant
                            && matches!(event, SessionEvent::AssistantMessage { .. }))
                );
            }
        }
    }
    Ok(())
}

struct RefusePrompt;

#[async_trait::async_trait]
impl UserPromptHook for RefusePrompt {
    async fn on_user_prompt(&self, _: &str, _: &str) -> HookOutcome {
        HookOutcome::Block {
            reason: "fixture prompt refusal".to_owned(),
        }
    }
}

#[tokio::test]
async fn preappend_prompt_refusal_resolves_without_any_accepted_record() -> TestResult {
    let store = EventStore::new();
    let mut fixture = ObservationFixture::new(&store)?;
    let provider = MockProvider::new(Vec::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::UserPrompt(Box::new(RefusePrompt)));
    let mut context = LoopContext::new("system");
    context.hooks = Some(Arc::new(hooks));
    assert!(matches!(
        observed_step(&fixture, &store, &provider, &mut context, "refused").await,
        Err(NornError::HookBlocked { .. })
    ));
    assert!(matches!(
        fixture.execution.opening_input(),
        Some(PublicationResolution::NotAccepted(
            PublicationEnd::Abandoned
        ))
    ));
    assert_eq!(provider.call_count(), 0);
    assert!(store.events().is_empty());
    assert!(fixture.attempts()?.is_empty());
    Ok(())
}

struct RefusingSink {
    opening: bool,
}

impl crate::session::store::PersistenceSink for RefusingSink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), crate::session::SessionPersistError> {
        if self.opening && matches!(event, SessionEvent::UserMessage { .. }) {
            return Err(crate::session::SessionPersistError::Io(
                std::io::Error::other("fixture opening append refusal"),
            ));
        }
        Ok(())
    }

    fn persist_batch(
        &mut self,
        _: &[SessionEvent],
    ) -> Result<(), crate::session::SessionPersistError> {
        Err(crate::session::SessionPersistError::Io(
            std::io::Error::other("fixture response append refusal"),
        ))
    }
}

#[tokio::test]
async fn failed_opening_or_assistant_append_never_reports_acceptance() -> TestResult {
    for opening in [true, false] {
        let store = EventStore::with_sink(Box::new(RefusingSink { opening }));
        let mut fixture = ObservationFixture::new(&store)?;
        let provider = MockProvider::new(vec![vec![
            text_delta("unaccepted answer"),
            done_event(StopReason::EndTurn),
        ]]);
        let mut context = LoopContext::new("system");
        assert!(matches!(
            observed_step(&fixture, &store, &provider, &mut context, "prompt").await,
            Err(NornError::Session(_))
        ));
        let tickets = fixture.attempts()?;
        if opening {
            assert!(matches!(
                fixture.execution.opening_input(),
                Some(PublicationResolution::NotAccepted(PublicationEnd::Failed))
            ));
            assert!(tickets.is_empty());
            assert_eq!(provider.call_count(), 0);
        } else {
            accepted_event(fixture.execution.opening_input())?;
            assert_eq!(tickets.len(), 1);
            assert!(matches!(
                tickets[0].resolution(),
                Some(PublicationResolution::NotAccepted(PublicationEnd::Failed))
            ));
        }
        assert!(
            store
                .events()
                .iter()
                .all(|event| !matches!(event, SessionEvent::AssistantMessage { .. }))
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn retry_receipts_keep_failed_and_winning_attempts_distinct() -> TestResult {
    let store = EventStore::new();
    let mut fixture = ObservationFixture::new(&store)?;
    let provider = MockProvider::new(vec![
        vec![
            text_delta("failed fragment"),
            ProviderEvent::Error {
                error: ProviderError::StreamError {
                    reason: "fixture disconnect".to_owned(),
                    transient: Some(crate::error::TransientKind::ConnectionReset),
                },
            },
        ],
        vec![
            text_delta("accepted answer"),
            done_event(StopReason::EndTurn),
        ],
    ]);
    let mut context = LoopContext::new("system");
    context.retry_policy.jitter = false;
    assert!(matches!(
        observed_step(&fixture, &store, &provider, &mut context, "prompt").await?,
        AgentStepResult::Completed { .. }
    ));
    let tickets = fixture.attempts()?;
    assert_eq!(tickets.len(), 2);
    assert_eq!(tickets[0].attempt().response, 0);
    assert_eq!(tickets[1].attempt().response, 0);
    assert_eq!(tickets[0].attempt().attempt, 1);
    assert_eq!(tickets[1].attempt().attempt, 2);
    assert!(matches!(
        tickets[0].resolution(),
        Some(PublicationResolution::NotAccepted(PublicationEnd::Failed))
    ));
    let accepted = accepted_event(tickets[1].resolution())?;
    assert!(store.events().iter().any(|event| event.base().id == accepted && matches!(event, SessionEvent::AssistantMessage { content, .. } if content == "accepted answer")));
    Ok(())
}

#[tokio::test]
async fn identical_prompts_and_answers_remain_distinct_across_executions() -> TestResult {
    let store = EventStore::new();
    let mut first = ObservationFixture::new(&store)?;
    let mut second = ObservationFixture::bound(&store, first.execution.source())?;
    let provider = MockProvider::new(vec![
        vec![text_delta("same answer"), done_event(StopReason::EndTurn)],
        vec![text_delta("same answer"), done_event(StopReason::EndTurn)],
    ]);
    let mut context = LoopContext::new("system");
    for fixture in [&first, &second] {
        assert!(matches!(
            observed_step(fixture, &store, &provider, &mut context, "same prompt").await?,
            AgentStepResult::Completed { .. }
        ));
    }
    assert!(!first.execution.same_execution(&second.execution));
    assert_ne!(
        accepted_event(first.execution.opening_input())?,
        accepted_event(second.execution.opening_input())?
    );
    let first_attempt = first.attempts()?;
    let second_attempt = second.attempts()?;
    assert_eq!(first_attempt.len(), 1);
    assert_eq!(second_attempt.len(), 1);
    assert_ne!(
        accepted_event(first_attempt[0].resolution())?,
        accepted_event(second_attempt[0].resolution())?
    );
    Ok(())
}

#[tokio::test]
async fn canonical_responses_and_tool_loop_have_distinct_producer_response_ordinals() -> TestResult
{
    let store = EventStore::new();
    let mut fixture = ObservationFixture::new(&store)?;
    let message_parts = serde_json::json!([{"type":"output_text","text":"canonical answer","annotations":[],"logprobs":[]}]);
    let provider = MockProvider::new(vec![
        vec![
            tool_call_delta("read", Some("read_file"), "{}"),
            done_event(StopReason::ToolUse),
        ],
        vec![
            completed_message_item("message", &message_parts)?,
            done_event(StopReason::EndTurn),
        ],
    ]);
    let executor = MockToolExecutor::new(read_file_handlers());
    let mut context = LoopContext::new("system");
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "read then answer",
        tools: &[read_file_tool_def()],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: Some(&fixture.sender),
        inbound: None,
        loop_context: &mut context,
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));
    let tickets = fixture.attempts()?;
    assert_eq!(tickets.len(), 2);
    assert_eq!(tickets[0].attempt().response, 0);
    assert_eq!(tickets[1].attempt().response, 1);
    assert_eq!(tickets[0].attempt().attempt, 1);
    assert_eq!(tickets[1].attempt().attempt, 1);
    let first = accepted_event(tickets[0].resolution())?;
    let second = accepted_event(tickets[1].resolution())?;
    assert_ne!(first, second);
    assert!(store.events().iter().any(|event| event.base().id == second && matches!(event, SessionEvent::AssistantMessage { response_items, .. } if response_items.len() == 1)));
    Ok(())
}

#[tokio::test]
async fn active_steer_acknowledges_exact_event_before_hook_cancellation() -> TestResult {
    let store = EventStore::new();
    let (input, mut pending, mut deliveries) = crate::r#loop::active_input::active_input_channel();
    let id = input.send_steer("steer")?;
    let reached = Arc::new(tokio::sync::Notify::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(PauseAfterAcceptance {
        kind: PausedEvent::Opening,
        reached: Arc::clone(&reached),
    })));
    let mut messages = Vec::new();
    let mut flushing = Box::pin(crate::r#loop::delivery_inputs::flush_active_inputs(
        &store,
        &mut messages,
        Some(&mut pending),
        Some(&hooks),
    ));
    tokio::select! {
        () = reached.notified() => {},
        result = &mut flushing => return Err(format!("steer ended before hook: {result:?}").into()),
    }
    drop(flushing);
    let accepted = deliveries.try_recv().ok_or("missing steer acceptance")?;
    assert_eq!(accepted.id, id);
    assert_eq!(accepted.content, "steer");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content.as_deref(), Some("steer"));
    assert!(store.events().iter().any(|event| event.base().id == accepted.event_id && matches!(event, SessionEvent::UserMessage { content, .. } if content == "steer")));
    assert!(deliveries.try_recv().is_none());
    Ok(())
}

#[tokio::test]
async fn failed_steer_append_emits_no_acknowledgement_or_conversation_message() -> TestResult {
    let store = EventStore::with_sink(Box::new(RefusingSink { opening: true }));
    let (input, mut pending, mut deliveries) = crate::r#loop::active_input::active_input_channel();
    input.send_steer("steer")?;
    let mut messages = Vec::new();
    assert!(
        crate::r#loop::delivery_inputs::flush_active_inputs(
            &store,
            &mut messages,
            Some(&mut pending),
            None
        )
        .await
        .is_err()
    );
    assert!(deliveries.try_recv().is_none());
    assert!(messages.is_empty());
    assert!(store.events().is_empty());
    Ok(())
}

struct ControlledSink {
    entered: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::mpsc::Receiver<()>,
}

impl crate::session::store::PersistenceSink for ControlledSink {
    fn persist(&mut self, _: &SessionEvent) -> Result<(), crate::session::SessionPersistError> {
        self.entered.notify_one();
        self.release.recv().map_err(|error| {
            crate::session::SessionPersistError::Io(std::io::Error::other(format!(
                "controlled sink release failed: {error}"
            )))
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_waiter_cannot_abandon_the_actual_append_owners_receipt() -> TestResult {
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let (release, release_rx) = std::sync::mpsc::channel();
    let store = std::sync::Arc::new(EventStore::with_sink(Box::new(ControlledSink {
        entered: std::sync::Arc::clone(&entered),
        release: release_rx,
    })));
    let agent = Uuid::new_v4();
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
    let (tx, receiver) = broadcast::channel(1);
    let (sender, observation) = AgentEventSender::new(tx, agent, "fixture".to_owned())
        .observe_execution(&store, &source, Uuid::new_v4())?;
    let owner = sender
        .claim_execution(&store, true)?
        .ok_or("missing opening owner")?;
    let event = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "committed after waiter cancellation".to_owned(),
    };
    let expected_id = event.base().id.clone();
    let append_store = std::sync::Arc::clone(&store);
    let (completed, completion) = tokio::sync::oneshot::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let result = crate::r#loop::helpers::append_with_observer(&append_store, event, |result| {
            owner
                .appended(&append_store, result.as_ref())
                .map_err(crate::error::SessionError::from)
        });
        completed.send(result).map_err(|result| {
            drop(result);
            std::io::Error::other("append completion receiver was dropped")
        })
    });
    let waiter = tokio::spawn(worker);
    entered.notified().await;
    waiter.abort();
    assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
    assert!(observation.opening_input().is_none());
    release.send(())?;
    assert_eq!(completion.await??, expected_id);
    let Some(PublicationResolution::Accepted(record)) = observation.opening_input() else {
        return Err("append owner did not retain accepted receipt".into());
    };
    assert!(
        matches!(record.cursor().position(), crate::session_view::HistoryPosition::Event { event_id, .. } if *event_id == expected_id)
    );
    assert_eq!(store.events().len(), 1);
    drop(receiver);
    Ok(())
}

struct PauseBeforePublication {
    reached: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl crate::integration::hooks::PostLlmHook for PauseBeforePublication {
    async fn after_llm(&self, _: &crate::integration::hooks::LlmCallSummary) {
        self.reached.notify_one();
        std::future::pending::<()>().await;
    }
}

#[tokio::test]
async fn cancellation_after_assembly_before_append_abandons_only_assistant_receipt() -> TestResult {
    let store = EventStore::new();
    let mut fixture = ObservationFixture::new(&store)?;
    let provider = MockProvider::new(vec![vec![
        text_delta("assembled answer"),
        done_event(StopReason::EndTurn),
    ]]);
    let reached = Arc::new(tokio::sync::Notify::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PostLlm(Box::new(PauseBeforePublication {
        reached: Arc::clone(&reached),
    })));
    let mut context = LoopContext::new("system");
    context.hooks = Some(Arc::new(hooks));
    let mut running = Box::pin(observed_step(
        &fixture,
        &store,
        &provider,
        &mut context,
        "prompt",
    ));
    tokio::select! {
        () = reached.notified() => {},
        result = &mut running => return Err(format!("step ended before post-LLM hook: {result:?}").into()),
    }
    drop(running);
    accepted_event(fixture.execution.opening_input())?;
    let tickets = fixture.attempts()?;
    assert_eq!(tickets.len(), 1);
    assert!(matches!(
        tickets[0].resolution(),
        Some(PublicationResolution::NotAccepted(
            PublicationEnd::Abandoned
        ))
    ));
    assert!(
        store
            .events()
            .iter()
            .all(|event| !matches!(event, SessionEvent::AssistantMessage { .. }))
    );
    Ok(())
}

#[tokio::test]
async fn invalid_replay_provenance_never_accepts_the_opening_prompt() -> TestResult {
    let store = EventStore::new();
    let boundary_id = store.append_unvalidated_for_test(SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    })?;
    store.append_unvalidated_for_test(SessionEvent::Custom {
        base: EventBase::new(Some(boundary_id)),
        event_type: crate::session::PROVIDER_STATE_PROVENANCE_EVENT_TYPE.to_owned(),
        data: serde_json::json!({"version": "private malformed fixture"}),
    })?;
    let before = serde_json::to_vec(&store.events())?;
    let mut fixture = ObservationFixture::new(&store)?;
    let provider = MockProvider::new(Vec::new());
    let mut context = LoopContext::new("system");
    let result = observed_step(
        &fixture,
        &store,
        &provider,
        &mut context,
        "must not persist",
    )
    .await;
    assert!(
        matches!(
            &result,
            Err(NornError::Provider(
                ProviderError::ProviderStateProvenanceInvalid
            ))
        ),
        "framed malformed provenance must refuse before prompt acceptance: {result:?}",
    );
    assert!(matches!(
        fixture.execution.opening_input(),
        Some(PublicationResolution::NotAccepted(
            PublicationEnd::Abandoned
        ))
    ));
    assert_eq!(serde_json::to_vec(&store.events())?, before);
    assert_eq!(provider.call_count(), 0);
    assert!(fixture.attempts()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn external_message_wake_has_no_operator_opening_receipt() -> TestResult {
    let store = EventStore::new();
    let mut fixture = ObservationFixture::new(&store)?;
    let provider = MockProvider::new(vec![vec![
        text_delta("wake answer"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();
    let mut context = LoopContext::new("system");
    let result = run_agent_step_from_messages(AgentMessageStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: Some(&fixture.sender),
        initial_messages: vec![make_channel_message(
            "peer",
            "wake",
            crate::r#loop::inbound::MessageKind::Steer,
            0,
        )],
        inbound: None,
        loop_context: &mut context,
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));
    assert!(fixture.execution.opening_input().is_none());
    let tickets = fixture.attempts()?;
    assert_eq!(tickets.len(), 1);
    accepted_event(tickets[0].resolution())?;
    assert!(
        store
            .events()
            .iter()
            .any(|event| matches!(event, SessionEvent::UserMessage { .. }))
    );
    Ok(())
}
