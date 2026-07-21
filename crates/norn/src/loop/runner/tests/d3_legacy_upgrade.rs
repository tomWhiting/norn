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
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::events::ProviderEpochBoundaryReason;
use crate::session::{ProviderStateProvenance, response_publication_fixture};

const STORED_RESPONSE_ID: &str = "resp_legacy_stored";
const LATER_STATELESS_ID: &str = "resp_later_stateless";

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy)]
enum HistoricalCut {
    Migration,
    Adoption,
    Compaction,
    Suppression,
}

fn provider_for(server: &MockServer) -> Result<OpenAiProvider, ProviderError> {
    let config = ProviderConfig {
        auth_source: AuthSource::ApiKey {
            key: SecretString::new("d3-legacy-upgrade-key"),
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
    let auth: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("d3-legacy-upgrade-key"));
    OpenAiProvider::with_auth_provider(config, auth)
}

fn completed_stream() -> String {
    let terminal = serde_json::json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": "resp_after_upgrade",
            "status": "completed",
            "output": [{
                "id": "msg_after_upgrade",
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": "upgraded answer",
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

fn append_user(store: &EventStore, content: &str) -> Result<(), crate::error::SessionError> {
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(store.last_event_id()),
        content: content.to_owned(),
    })?;
    Ok(())
}

fn append_historical_cut(store: &EventStore, cut: HistoricalCut) -> TestResult {
    append_user(store, "history before cut")?;
    let target_event_id = store
        .last_event_id()
        .ok_or_else(|| io::Error::other("historical cut requires a preceding event"))?;
    let event = match cut {
        HistoricalCut::Migration | HistoricalCut::Adoption => SessionEvent::ProviderEpochBoundary {
            base: EventBase::new(Some(target_event_id)),
            reason: if matches!(cut, HistoricalCut::Migration) {
                ProviderEpochBoundaryReason::MigratedLegacy
            } else {
                ProviderEpochBoundaryReason::ProviderIdentityAdoption
            },
        },
        HistoricalCut::Compaction => SessionEvent::Compaction {
            base: EventBase::new(Some(target_event_id.clone())),
            summary: "history before D3".to_owned(),
            replaced_event_ids: vec![target_event_id],
        },
        HistoricalCut::Suppression => SessionEvent::ContextMark {
            base: EventBase::new(Some(target_event_id.clone())),
            mark: crate::session::events::ContextMarkKind::Suppress,
            target_event_id,
        },
    };
    store.append(event)?;
    Ok(())
}

fn append_bare_reasoning_assistant(
    store: &EventStore,
    response_id: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    append_bare_reasoning_assistant_with_content(store, response_id, "stored answer")
}

fn append_bare_reasoning_assistant_with_content(
    store: &EventStore,
    response_id: Option<&str>,
    content: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let item = ResponseItem::from_value(serde_json::json!({
        "type": "reasoning",
        "id": "rs_legacy_stored",
        "summary": [],
        "status": "completed"
    }))?;
    store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(store.last_event_id()),
        response_items: vec![ResponseTranscriptItem {
            item,
            provenance: ResponseStreamProvenance::default(),
        }],
        content: content.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: response_id.map(str::to_owned),
    })?;
    Ok(())
}

fn append_provenance_bare_reasoning_assistant(
    store: &EventStore,
    response_id: &str,
    stored: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = response_publication_fixture(store.last_event_id(), stored)?;
    let item = ResponseItem::from_value(serde_json::json!({
        "type": "reasoning",
        "id": "rs_provenance_bare",
        "summary": [],
        "status": "completed"
    }))?;
    let assistant = SessionEvent::AssistantMessage {
        base: fixture.assistant_base,
        response_items: vec![ResponseTranscriptItem {
            item,
            provenance: ResponseStreamProvenance::default(),
        }],
        content: "provenance answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    };
    let publication = crate::session::committed_response_publication(
        fixture.boundary,
        fixture.provenance,
        assistant,
    )?;
    store.append_batch(&publication)?;
    Ok(())
}

fn append_replayable_assistant(
    store: &EventStore,
    response_id: &str,
    encrypted_reasoning: Option<&str>,
) -> Result<(), crate::error::SessionError> {
    let reasoning = encrypted_reasoning
        .map(|encrypted_content| ReasoningItem {
            id: "rs_later_stateless".to_owned(),
            summary: Vec::new(),
            content: None,
            encrypted_content: Some(encrypted_content.to_owned()),
        })
        .into_iter()
        .collect();
    store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(store.last_event_id()),
        response_items: Vec::new(),
        content: "replayable answer".to_owned(),
        thinking: String::new(),
        reasoning,
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    })?;
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

async fn recorded_payload(server: &MockServer) -> TestResultValue {
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is unavailable"))?;
    if requests.len() != 1 {
        return Err(io::Error::other(format!(
            "expected one provider request, observed {}",
            requests.len()
        ))
        .into());
    }
    Ok(serde_json::from_slice(&requests[0].body)?)
}

type TestResultValue = Result<Value, Box<dyn std::error::Error + Send + Sync>>;

fn provenance_count(store: &EventStore) -> usize {
    store
        .events()
        .iter()
        .filter(|event| matches!(ProviderStateProvenance::from_event(event), Ok(Some(_))))
        .count()
}

#[tokio::test]
async fn legacy_bare_state_recovers_through_its_response_id() -> TestResult {
    let server = MockServer::start().await;
    mount_completed_response(&server).await;
    let provider = provider_for(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    append_user(&store, "stored prompt")?;
    append_bare_reasoning_assistant(&store, Some(STORED_RESPONSE_ID))?;

    assert!(matches!(
        run_with_prompt(&provider, &store).await?,
        AgentStepResult::Completed { .. }
    ));
    let payload = recorded_payload(&server).await?;
    assert_eq!(payload["previous_response_id"], STORED_RESPONSE_ID);
    let input = serde_json::to_string(&payload["input"])?;
    assert!(input.contains("new prompt"));
    assert!(!input.contains("stored prompt"));
    assert!(!input.contains("rs_legacy_stored"));
    assert_eq!(provenance_count(&store), 1);
    Ok(())
}

#[tokio::test]
async fn pre_d3_bare_state_after_pre_d3_epoch_cut_recovers_through_response_id() -> TestResult {
    for (cut, response_id) in [
        (HistoricalCut::Migration, "resp_after_migration_cut"),
        (HistoricalCut::Adoption, "resp_after_adoption_cut"),
        (HistoricalCut::Compaction, "resp_after_compaction_cut"),
        (HistoricalCut::Suppression, "resp_after_suppression_cut"),
    ] {
        let server = MockServer::start().await;
        mount_completed_response(&server).await;
        let provider = provider_for(&server)?;
        let store = EventStore::new();
        store.validate_or_bind_provider_state_identity(provider.state_identity())?;
        append_historical_cut(&store, cut)?;
        append_user(&store, "pre-D3 stored prompt")?;
        append_bare_reasoning_assistant(&store, Some(response_id))?;

        assert!(matches!(
            run_with_prompt(&provider, &store).await?,
            AgentStepResult::Completed { .. }
        ));
        let payload = recorded_payload(&server).await?;
        assert_eq!(payload["previous_response_id"], response_id);
        let input = serde_json::to_string(&payload["input"])?;
        assert!(input.contains("new prompt"));
        assert!(!input.contains("pre-D3 stored prompt"));
        assert!(!input.contains("rs_legacy_stored"));
    }
    Ok(())
}

#[tokio::test]
async fn later_replayable_id_cannot_displace_witnessed_legacy_anchor() -> TestResult {
    let server = MockServer::start().await;
    mount_completed_response(&server).await;
    let provider = provider_for(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    append_user(&store, "stored prompt")?;
    append_bare_reasoning_assistant(&store, Some(STORED_RESPONSE_ID))?;
    append_user(&store, "later stateless prompt")?;
    append_replayable_assistant(&store, LATER_STATELESS_ID, Some("encrypted-later"))?;

    assert!(matches!(
        run_with_prompt(&provider, &store).await?,
        AgentStepResult::Completed { .. }
    ));
    let payload = recorded_payload(&server).await?;
    assert_eq!(payload["previous_response_id"], STORED_RESPONSE_ID);
    let input = serde_json::to_string(&payload["input"])?;
    assert!(input.contains("later stateless prompt"));
    assert!(input.contains("encrypted-later"));
    assert!(!input.contains("stored prompt"));
    Ok(())
}

#[tokio::test]
async fn two_witnessed_legacy_candidates_select_the_newest_response_id() -> TestResult {
    let server = MockServer::start().await;
    mount_completed_response(&server).await;
    let provider = provider_for(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    append_user(&store, "old prompt")?;
    append_bare_reasoning_assistant_with_content(&store, Some("resp_old"), "old witness")?;
    append_user(&store, "newer prompt")?;
    append_bare_reasoning_assistant_with_content(&store, Some("resp_new"), "new witness")?;

    run_with_prompt(&provider, &store).await?;

    let payload = recorded_payload(&server).await?;
    assert_eq!(payload["previous_response_id"], "resp_new");
    Ok(())
}

#[tokio::test]
async fn negative_provenance_after_a_proven_anchor_cannot_be_adopted() -> TestResult {
    let server = MockServer::start().await;
    mount_completed_response(&server).await;
    let provider = provider_for(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    append_user(&store, "stored prompt")?;
    append_provenance_bare_reasoning_assistant(&store, STORED_RESPONSE_ID, true)?;
    append_user(&store, "stateless prompt")?;
    append_provenance_bare_reasoning_assistant(&store, LATER_STATELESS_ID, false)?;
    let before = serde_json::to_vec(&store.events())?;

    assert!(matches!(
        run_with_prompt(&provider, &store).await,
        Err(NornError::Provider(
            ProviderError::ProviderStateReplayUnavailable
        ))
    ));
    assert_eq!(serde_json::to_vec(&store.events())?, before);
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is unavailable"))?;
    assert!(requests.is_empty());
    Ok(())
}

#[tokio::test]
async fn replayable_legacy_history_full_replays_before_recording_provenance() -> TestResult {
    let server = MockServer::start().await;
    mount_completed_response(&server).await;
    let provider = provider_for(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    append_user(&store, "replayable old prompt")?;
    append_replayable_assistant(&store, LATER_STATELESS_ID, None)?;

    assert!(matches!(
        run_with_prompt(&provider, &store).await?,
        AgentStepResult::Completed { .. }
    ));
    let payload = recorded_payload(&server).await?;
    assert!(payload.get("previous_response_id").is_none());
    let input = serde_json::to_string(&payload["input"])?;
    assert!(input.contains("replayable old prompt"));
    assert!(input.contains("replayable answer"));
    assert_eq!(provenance_count(&store), 1);
    Ok(())
}

#[tokio::test]
async fn legacy_anchor_with_unreplayable_suffix_fails_before_mutation_or_wire() -> TestResult {
    let server = MockServer::start().await;
    mount_completed_response(&server).await;
    let provider = provider_for(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    append_user(&store, "stored prompt")?;
    append_bare_reasoning_assistant(&store, Some(STORED_RESPONSE_ID))?;
    append_user(&store, "later opaque prompt")?;
    append_bare_reasoning_assistant(&store, None)?;
    let before = serde_json::to_vec(&store.events())?;

    assert!(matches!(
        run_with_prompt(&provider, &store).await,
        Err(NornError::Provider(
            ProviderError::ProviderStateReplayUnavailable
        ))
    ));
    assert_eq!(serde_json::to_vec(&store.events())?, before);
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is unavailable"))?;
    assert!(requests.is_empty());
    Ok(())
}
