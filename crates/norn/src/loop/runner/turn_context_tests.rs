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
use crate::provider::ProviderTurnContext;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::ProviderRequest;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::session::store::EventStore;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContextObservation {
    session_id: Option<String>,
    turn_id: String,
    state_present: bool,
}

#[derive(Default)]
struct ContextRecordingProvider {
    calls: AtomicUsize,
    observations: Mutex<Vec<ContextObservation>>,
}

impl ContextRecordingProvider {
    fn observations(&self) -> Result<Vec<ContextObservation>, io::Error> {
        self.observations
            .lock()
            .map(|observations| observations.clone())
            .map_err(|error| io::Error::other(format!("observation lock poisoned: {error}")))
    }
}

impl Provider for ContextRecordingProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::InvalidRequest {
            message: "runner bypassed the required turn-context provider path".to_owned(),
        })
    }

    fn stream_with_context(
        &self,
        _request: ProviderRequest,
        context: ProviderTurnContext,
    ) -> Result<ProviderStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let observation = ContextObservation {
            session_id: context.session_id().map(str::to_owned),
            turn_id: context.turn_id().to_owned(),
            state_present: context.codex_turn_state_header().is_some(),
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
) -> TestResult<AgentStepResult> {
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();
    Ok(run_agent_step(AgentStepRequest {
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
    .await?)
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
    assert!(!first.state_present);
    assert!(retry.state_present);
    assert!(continuation.state_present);
    assert!(!next_step.state_present);
    Ok(())
}
