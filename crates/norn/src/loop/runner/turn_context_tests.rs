use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream;

use super::*;
use crate::error::ProviderError;
use crate::integration::variables::VariableStore;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::retry::{RetryPolicy, RetryableError};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::request::{Message, MessageRole, ProviderRequest};
use crate::provider::tools::ProviderCapabilities;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::provider::{ProviderStateIdentity, ProviderTurnContext};
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContextObservation {
    session_id: Option<String>,
    turn_id: String,
    identity_prebound: bool,
    state_present: bool,
    previous_response_id: Option<String>,
}

struct ContextRecordingProvider {
    calls: AtomicUsize,
    observations: Mutex<Vec<ContextObservation>>,
    state_identity: ProviderStateIdentity,
}

impl Default for ContextRecordingProvider {
    fn default() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            observations: Mutex::new(Vec::new()),
            state_identity: ProviderStateIdentity::derive(
                "norn.runner.turn-context-test",
                b"stable-fixture-identity",
            ),
        }
    }
}

impl ContextRecordingProvider {
    fn with_identity(state_identity: ProviderStateIdentity) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            observations: Mutex::new(Vec::new()),
            state_identity,
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn observations(&self) -> Result<Vec<ContextObservation>, io::Error> {
        self.observations
            .lock()
            .map(|observations| observations.clone())
            .map_err(|error| io::Error::other(format!("observation lock poisoned: {error}")))
    }
}

impl Provider for ContextRecordingProvider {
    fn state_identity(&self) -> Option<ProviderStateIdentity> {
        Some(self.state_identity)
    }

    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::InvalidRequest {
            message: "runner bypassed the required turn-context provider path".to_owned(),
        })
    }

    fn stream_with_context(
        &self,
        request: ProviderRequest,
        context: ProviderTurnContext,
    ) -> Result<ProviderStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let different_identity = ProviderStateIdentity::derive(
            "norn.runner.turn-context-test",
            b"different-fixture-identity",
        );
        if context.bind_state_identity(different_identity).is_ok() {
            return Err(ProviderError::InvalidRequest {
                message: "runner supplied an unbound provider turn context".to_owned(),
            });
        }
        context.bind_state_identity(self.state_identity)?;

        let observation = ContextObservation {
            session_id: context.session_id().map(str::to_owned),
            turn_id: context.turn_id().to_owned(),
            identity_prebound: true,
            state_present: context.codex_turn_state_header().is_some(),
            previous_response_id: request.previous_response_id,
        };
        self.observations
            .lock()
            .map_err(|error| ProviderError::StreamError {
                reason: format!("observation lock poisoned: {error}"),
                transient: None,
            })?
            .push(observation);

        let events = match call {
            0 => {
                context.observe_codex_turn_state("turn-state");
                vec![Err(ProviderError::StreamInterrupted {
                    reason: "retry sentinel".to_owned(),
                })]
            }
            1 => vec![Ok(done_event(StopReason::ContinueTurn))],
            2 => vec![
                Ok(ProviderEvent::TextDelta {
                    text: "first step".to_owned(),
                }),
                Ok(done_event(StopReason::EndTurn)),
            ],
            _ => vec![
                Ok(ProviderEvent::TextDelta {
                    text: "second step".to_owned(),
                }),
                Ok(done_event(StopReason::EndTurn)),
            ],
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

#[derive(Default)]
struct ReplayRejectingProvider {
    calls: AtomicUsize,
}

impl Provider for ReplayRejectingProvider {
    fn validate_replay(&self, messages: &[Message]) -> Result<(), ProviderError> {
        if messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant)
        {
            return Err(ProviderError::ProviderStateReplayUnavailable);
        }
        Ok(())
    }

    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::InvalidRequest {
            message: "replay preflight was bypassed".to_owned(),
        })
    }
}

fn done_event(stop_reason: StopReason) -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason,
        usage: Usage::default(),
        response_id: None,
    }
}

async fn run_step(
    provider: &dyn Provider,
    store: &EventStore,
    loop_context: &mut LoopContext,
    prompt: &str,
) -> Result<AgentStepResult, crate::error::NornError> {
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();
    run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt: prompt,
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context,
        cancel: None,
    })
    .await
}

fn required_observation(
    observations: &[ContextObservation],
    index: usize,
) -> TestResult<&ContextObservation> {
    observations
        .get(index)
        .ok_or_else(|| io::Error::other(format!("missing context observation {index}")).into())
}

#[tokio::test]
async fn turn_context_survives_retry_and_continuation_but_not_the_next_step() -> TestResult {
    let provider = ContextRecordingProvider::default();
    let store = EventStore::new();
    let variables = VariableStore::with_builtins().with_session_id("session-context-test");
    let mut loop_context = LoopContext::new("system");
    loop_context.variables = Some(Arc::new(variables));
    loop_context.retry_policy = RetryPolicy {
        max_retries: 1,
        initial_backoff: Duration::ZERO,
        backoff_multiplier: 1.0,
        retryable_errors: vec![RetryableError::ConnectionReset],
    };

    let first_result = run_step(&provider, &store, &mut loop_context, "first prompt").await?;
    assert!(matches!(first_result, AgentStepResult::Completed { .. }));
    let second_result = run_step(&provider, &store, &mut loop_context, "second prompt").await?;
    assert!(matches!(second_result, AgentStepResult::Completed { .. }));

    let observations = provider.observations()?;
    assert_eq!(observations.len(), 4);
    let first = required_observation(&observations, 0)?;
    let retry = required_observation(&observations, 1)?;
    let continuation = required_observation(&observations, 2)?;
    let next_step = required_observation(&observations, 3)?;
    assert_eq!(first.session_id.as_deref(), Some("session-context-test"));
    assert_eq!(first.session_id, retry.session_id);
    assert_eq!(first.session_id, continuation.session_id);
    assert_eq!(first.session_id, next_step.session_id);
    assert_eq!(first.turn_id, retry.turn_id);
    assert_eq!(first.turn_id, continuation.turn_id);
    assert_ne!(first.turn_id, next_step.turn_id);
    assert!(
        observations
            .iter()
            .all(|observation| observation.identity_prebound),
        "every retry, continuation, and new-step request must receive an already-bound context",
    );
    assert!(!first.state_present);
    assert!(retry.state_present);
    assert!(continuation.state_present);
    assert!(!next_step.state_present);
    Ok(())
}

#[tokio::test]
async fn store_identity_mismatch_precedes_prompt_append_and_provider_dispatch() -> TestResult {
    let bound_identity =
        ProviderStateIdentity::derive("norn.runner.affinity-test", b"bound-provider-fixture");
    let other_identity =
        ProviderStateIdentity::derive("norn.runner.affinity-test", b"other-provider-fixture");
    let provider = ContextRecordingProvider::with_identity(other_identity);
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(Some(bound_identity))?;
    let before = serde_json::to_vec(&store.events())?;
    let mut loop_context = LoopContext::new("system");

    let result = run_step(&provider, &store, &mut loop_context, "must not persist").await;

    assert!(matches!(
        result,
        Err(crate::error::NornError::Provider(
            ProviderError::ProviderStateIdentityMismatch
        ))
    ));
    assert_eq!(
        provider.call_count(),
        0,
        "mismatch must make no provider call"
    );
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        before,
        "mismatch must not mutate the prompt log",
    );
    Ok(())
}

#[tokio::test]
async fn replay_validation_precedes_prompt_append_and_provider_dispatch() -> TestResult {
    let provider = ReplayRejectingProvider::default();
    let store = EventStore::new();
    store.append(SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: "prior provider response".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    })?;
    let before = serde_json::to_vec(&store.events())?;
    let mut loop_context = LoopContext::new("system");

    let result = run_step(&provider, &store, &mut loop_context, "must not persist").await;

    assert!(matches!(
        result,
        Err(crate::error::NornError::Provider(
            ProviderError::ProviderStateReplayUnavailable
        ))
    ));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "unreplayable history must dispatch no request",
    );
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        before,
        "replay validation must precede user-prompt persistence",
    );
    Ok(())
}

#[tokio::test]
async fn missing_identity_cannot_bypass_a_bound_store() -> TestResult {
    let provider = MockProvider::new(vec![vec![done_event(StopReason::EndTurn)]]);
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(Some(ProviderStateIdentity::derive(
        "norn.runner.affinity-test",
        b"bound-provider-fixture",
    )))?;
    let before = serde_json::to_vec(&store.events())?;
    let mut loop_context = LoopContext::new("system");

    let result = run_step(&provider, &store, &mut loop_context, "must not persist").await;

    assert!(matches!(
        result,
        Err(crate::error::NornError::Provider(
            ProviderError::ProviderStateIdentityMismatch
        ))
    ));
    assert_eq!(
        provider.call_count(),
        0,
        "absence must make no provider call"
    );
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        before,
        "absence must not mutate the prompt log",
    );
    Ok(())
}

#[tokio::test]
async fn threaded_provider_requires_identity_before_its_first_turn() -> TestResult {
    let provider = MockProvider::with_capabilities(
        vec![vec![done_event(StopReason::EndTurn)]],
        ProviderCapabilities::openai_responses(),
    );
    let store = EventStore::new();
    let mut loop_context = LoopContext::new("system");

    let result = run_step(&provider, &store, &mut loop_context, "must not persist").await;

    assert!(matches!(
        result,
        Err(crate::error::NornError::Provider(
            ProviderError::ProviderStateIdentityRequired
        ))
    ));
    assert_eq!(
        provider.call_count(),
        0,
        "missing identity must dispatch no request"
    );
    assert!(
        store.is_empty(),
        "missing identity must append no user prompt"
    );
    Ok(())
}

#[tokio::test]
async fn identityless_threaded_provider_cannot_reuse_an_existing_response_anchor() -> TestResult {
    let provider = MockProvider::with_capabilities(
        vec![vec![done_event(StopReason::EndTurn)]],
        ProviderCapabilities::openai_responses(),
    );
    let store = EventStore::new();
    store.append(SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: "prior provider response".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_affinity_anchor".to_owned()),
    })?;
    let before = serde_json::to_vec(&store.events())?;
    let mut loop_context = LoopContext::new("system");

    let result = run_step(&provider, &store, &mut loop_context, "must not persist").await;

    assert!(matches!(
        result,
        Err(crate::error::NornError::Provider(
            ProviderError::ProviderStateIdentityRequired
        ))
    ));
    assert_eq!(
        provider.call_count(),
        0,
        "missing identity must dispatch no request"
    );
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        before,
        "identity validation must precede response-anchor reuse and prompt persistence",
    );
    Ok(())
}

#[tokio::test]
async fn first_sinkless_identity_adoption_cuts_the_existing_response_anchor() -> TestResult {
    let identity =
        ProviderStateIdentity::derive("norn.runner.affinity-test", b"adopting-provider-fixture");
    let provider = ContextRecordingProvider::with_identity(identity);
    let store = EventStore::new();
    store.append(SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: "prior provider response".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_owned_by_unknown_credentials".to_owned()),
    })?;
    let mut loop_context = LoopContext::new("system");

    let result = run_step(&provider, &store, &mut loop_context, "new epoch").await?;

    assert!(matches!(result, AgentStepResult::Completed { .. }));
    let observations = provider.observations()?;
    assert!(!observations.is_empty());
    assert!(
        observations
            .iter()
            .all(|observation| observation.previous_response_id.is_none()),
        "no retry or continuation may revive the pre-adoption response anchor",
    );
    assert!(matches!(
        store.events().get(1),
        Some(SessionEvent::ProviderEpochBoundary {
            reason: crate::session::events::ProviderEpochBoundaryReason::ProviderIdentityAdoption,
            ..
        })
    ));
    assert_eq!(store.provider_state_identity(), Some(identity));
    Ok(())
}
