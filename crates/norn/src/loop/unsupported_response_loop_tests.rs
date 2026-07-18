use futures_util::stream;
use serde_json::json;

use super::LoopContext;
use super::config::AgentStepResult;
use super::runner::{AgentLoopConfig, AgentStepRequest, MockToolExecutor, run_agent_step};
use crate::error::{NornError, ProviderError};
use crate::provider::events::ProviderEvent;
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::request::ProviderRequest;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::provider::tools::ProviderCapabilities;
use crate::provider::traits::{Provider, ProviderStream};
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Copy)]
enum UnsupportedContract {
    OutputItem,
    StreamEvent,
}

impl UnsupportedContract {
    const fn provider_error(self) -> ProviderError {
        match self {
            Self::OutputItem => ProviderError::UnsupportedResponseItem,
            Self::StreamEvent => ProviderError::UnsupportedResponseEvent,
        }
    }
}

struct UnsupportedContractProvider {
    previews: Vec<ProviderEvent>,
    failure: UnsupportedContract,
}

impl Provider for UnsupportedContractProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let mut events = self.previews.iter().cloned().map(Ok).collect::<Vec<_>>();
        events.push(Err(self.failure.provider_error()));
        Ok(Box::pin(stream::iter(events)))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

async fn run(
    provider: &UnsupportedContractProvider,
    store: &EventStore,
) -> Result<AgentStepResult, NornError> {
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();
    let mut loop_context = LoopContext::default();
    run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt: "exercise unsupported Responses contract",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await
}

fn assert_no_ordinary_response_events(store: &EventStore) {
    assert!(store.events().iter().all(|event| !matches!(
        event,
        SessionEvent::AssistantMessage { .. } | SessionEvent::ToolResult { .. }
    )));
}

#[tokio::test]
async fn unknown_output_item_fails_loudly_without_persisting_an_ordinary_turn() -> TestResult {
    let raw_item = json!({
        "type": "future_output_item",
        "id": "future_item_1",
        "payload": {"retained": true}
    });
    let stream_event = ResponseStreamEvent::from_raw(json!({
        "type": "response.output_item.done",
        "sequence_number": 1,
        "output_index": 0,
        "item": raw_item.clone()
    }))?;
    let item = ResponseItem::from_value(raw_item)?;
    let provider = UnsupportedContractProvider {
        previews: vec![
            ProviderEvent::ResponseStreamEvent {
                event: Box::new(stream_event),
            },
            ProviderEvent::ResponseItemDone {
                item: ResponseTranscriptItem {
                    item,
                    provenance: ResponseStreamProvenance {
                        item_id: Some("future_item_1".to_owned()),
                        output_index: Some(0),
                        content_index: None,
                        sequence_number: Some(1),
                    },
                },
            },
        ],
        failure: UnsupportedContract::OutputItem,
    };
    let store = EventStore::new();

    let result = run(&provider, &store).await;

    assert!(matches!(
        result,
        Err(NornError::Provider(ProviderError::UnsupportedResponseItem))
    ));
    assert_no_ordinary_response_events(&store);
    Ok(())
}

#[tokio::test]
async fn unknown_stream_event_fails_loudly_without_persisting_an_ordinary_turn() -> TestResult {
    let stream_event = ResponseStreamEvent::from_raw(json!({
        "type": "response.future.delta",
        "sequence_number": 1,
        "payload": {"retained": true}
    }))?;
    let provider = UnsupportedContractProvider {
        previews: vec![ProviderEvent::ResponseStreamEvent {
            event: Box::new(stream_event),
        }],
        failure: UnsupportedContract::StreamEvent,
    };
    let store = EventStore::new();

    let result = run(&provider, &store).await;

    assert!(matches!(
        result,
        Err(NornError::Provider(ProviderError::UnsupportedResponseEvent))
    ));
    assert_no_ordinary_response_events(&store);
    Ok(())
}
