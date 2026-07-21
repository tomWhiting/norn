use std::io;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
use crate::provider::openai::OpenAiProvider;
use crate::provider::reasoning::ReasoningItem;
use crate::provider::request::{ProviderConfig, SecretString};
use crate::provider::response_item::{
    ResponseItem, ResponseItemError, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::response_publication_fixture;

const PRIOR_RESPONSE_ID: &str = "resp_d3_replay_prior";

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy)]
enum BareReasoningEncoding {
    Canonical,
    Legacy,
}

fn provider_for(server: &MockServer) -> Result<OpenAiProvider, ProviderError> {
    let config = ProviderConfig {
        auth_source: AuthSource::ApiKey {
            key: SecretString::new("d3-replay-fixture-key"),
        },
        base_url: Some(format!("{}/v1", server.uri())),
        timeout: Duration::from_secs(5),
        max_retries: 0,
        provider_options: None,
        debug_dump_file: None,
        rate_limit: None,
        rate_limit_interval: None,
        retry_backoff: None,
        retry_after_ceiling: None,
    };
    let auth: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("d3-replay-fixture-key"));
    OpenAiProvider::with_auth_provider(config, auth)
}

fn completed_stream() -> String {
    let terminal = serde_json::json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": "resp_d3_replay_current",
            "status": "completed",
            "output": [{
                "id": "msg_d3_replay_current",
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": "current answer",
                    "annotations": [],
                    "logprobs": []
                }]
            }],
            "incomplete_details": null,
            "usage": {
                "input_tokens": 4,
                "input_tokens_details": {
                    "cached_tokens": 0,
                    "cache_write_tokens": 0
                },
                "output_tokens": 2,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 6
            }
        }
    });
    format!("event: response.completed\ndata: {terminal}\n\n")
}

async fn mount_completed_response(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(completed_stream()),
        )
        .mount(server)
        .await;
}

fn bare_reasoning_event(
    base: EventBase,
    encoding: BareReasoningEncoding,
    response_id: Option<&str>,
) -> Result<SessionEvent, ResponseItemError> {
    let (response_items, reasoning) = match encoding {
        BareReasoningEncoding::Canonical => {
            let item =
                crate::provider::response_item::ResponseItem::from_value(serde_json::json!({
                    "type": "reasoning",
                    "id": "rs_d3_canonical_bare",
                    "summary": [],
                    "status": "completed"
                }))?;
            (
                vec![ResponseTranscriptItem {
                    item,
                    provenance: ResponseStreamProvenance::default(),
                }],
                Vec::new(),
            )
        }
        BareReasoningEncoding::Legacy => (
            Vec::new(),
            vec![ReasoningItem {
                id: "rs_d3_legacy_bare".to_owned(),
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
            }],
        ),
    };

    Ok(SessionEvent::AssistantMessage {
        response_items,
        base,
        content: "prior answer".to_owned(),
        thinking: String::new(),
        reasoning,
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: response_id.map(str::to_owned),
    })
}

fn seed_history(
    store: &EventStore,
    encoding: BareReasoningEncoding,
    response_id: Option<&str>,
) -> TestResult {
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(store.last_event_id()),
        content: "prior prompt".to_owned(),
    })?;
    if response_id.is_some_and(|id| !id.is_empty()) {
        let fixture = response_publication_fixture(store.last_event_id(), true)?;
        let assistant = bare_reasoning_event(fixture.assistant_base, encoding, response_id)?;
        store.append_batch(&[fixture.boundary, fixture.provenance, assistant])?;
    } else {
        let assistant_base = EventBase::new(store.last_event_id());
        store.append(bare_reasoning_event(assistant_base, encoding, response_id)?)?;
    }
    Ok(())
}

async fn run_with_prompt(
    provider: &OpenAiProvider,
    store: &EventStore,
) -> Result<AgentStepResult, NornError> {
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();
    let mut loop_context = LoopContext::new("system");
    run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt: "new prompt",
        tools: &[],
        output_schema: None,
        model: "gpt-test",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await
}

async fn assert_unanchored_replay_rejected(encoding: BareReasoningEncoding) -> TestResult {
    let server = MockServer::start().await;
    mount_completed_response(&server).await;
    let provider = provider_for(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    seed_history(&store, encoding, None)?;
    let before = serde_json::to_vec(&store.events())?;

    let result = run_with_prompt(&provider, &store).await;
    assert!(matches!(
        result,
        Err(NornError::Provider(
            ProviderError::ProviderStateReplayUnavailable
        ))
    ));
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        before,
        "replay validation must precede the new UserMessage append",
    );
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is unavailable"))?;
    assert!(
        requests.is_empty(),
        "unreplayable provider state must fail before HTTP dispatch",
    );
    Ok(())
}

async fn assert_anchored_replay_succeeds(encoding: BareReasoningEncoding) -> TestResult {
    let server = MockServer::start().await;
    mount_completed_response(&server).await;
    let provider = provider_for(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    seed_history(&store, encoding, Some(PRIOR_RESPONSE_ID))?;

    let result = run_with_prompt(&provider, &store).await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));

    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is unavailable"))?;
    assert_eq!(requests.len(), 1);
    let payload: Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(payload["store"], Value::Bool(true));
    assert_eq!(payload["previous_response_id"], PRIOR_RESPONSE_ID);
    let input = serde_json::to_string(&payload["input"])?;
    assert!(input.contains("new prompt"));
    assert!(!input.contains("prior prompt"));
    assert!(!input.contains("prior answer"));
    assert!(!input.contains("rs_d3_canonical_bare"));
    assert!(!input.contains("rs_d3_legacy_bare"));
    Ok(())
}

#[tokio::test]
async fn empty_provider_compaction_fails_before_wire_or_prompt_append() -> TestResult {
    let server = MockServer::start().await;
    mount_completed_response(&server).await;
    let provider = provider_for(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(store.last_event_id()),
        content: "prior prompt".to_owned(),
    })?;
    store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(store.last_event_id()),
        response_items: vec![ResponseTranscriptItem {
            item: ResponseItem::malformed_empty_compaction_for_replay_test(),
            provenance: ResponseStreamProvenance::default(),
        }],
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    })?;
    let before = serde_json::to_vec(&store.events())?;

    let result = run_with_prompt(&provider, &store).await;
    assert!(matches!(
        result,
        Err(NornError::Provider(
            ProviderError::ProviderStateReplayUnavailable
        ))
    ));
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        before,
        "replay validation must precede the new UserMessage append",
    );
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is unavailable"))?;
    assert!(
        requests.is_empty(),
        "empty provider compaction state must fail before HTTP dispatch",
    );
    Ok(())
}

#[tokio::test]
async fn canonical_bare_reasoning_fails_before_wire_or_prompt_append() -> TestResult {
    assert_unanchored_replay_rejected(BareReasoningEncoding::Canonical).await
}

#[tokio::test]
async fn legacy_bare_reasoning_fails_before_wire_or_prompt_append() -> TestResult {
    assert_unanchored_replay_rejected(BareReasoningEncoding::Legacy).await
}

#[tokio::test]
async fn anchored_canonical_bare_reasoning_stays_behind_response_id() -> TestResult {
    assert_anchored_replay_succeeds(BareReasoningEncoding::Canonical).await
}

#[tokio::test]
async fn anchored_legacy_bare_reasoning_stays_behind_response_id() -> TestResult {
    assert_anchored_replay_succeeds(BareReasoningEncoding::Legacy).await
}
