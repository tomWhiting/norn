use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::error::ErrorClass;
use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
use crate::provider::openai::OpenAiProvider;
use crate::provider::request::{ProviderConfig, SecretString};
use crate::session::response_publication_fixture;

const STORED_ANCHOR: &str = "resp_stored_anchor_fixture";
const HOSTILE_BODY: &str = "authority-error-body-must-not-escape";

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
async fn anchored_client_rejection_never_retries_without_the_anchor() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_string(HOSTILE_BODY))
        .mount(&server)
        .await;

    let directory = tempdir()?;
    let debug_dump = directory.path().join("responses-debug.jsonl");
    let provider_config = ProviderConfig {
        auth_source: AuthSource::ApiKey {
            key: SecretString::new("fixture-api-key"),
        },
        base_url: Some(format!("{}/v1", server.uri())),
        timeout: Duration::from_secs(5),
        max_retries: 2,
        provider_options: None,
        debug_dump_file: Some(debug_dump.clone()),
        rate_limit: None,
        rate_limit_interval: None,
        retry_backoff: None,
        retry_after_ceiling: None,
    };
    let auth_provider: Arc<dyn AuthProvider> =
        Arc::new(MockAuthProvider::single("fixture-api-key"));
    let provider = OpenAiProvider::with_auth_provider(provider_config, auth_provider)?;

    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "prior prompt".to_owned(),
    })?;
    let fixture = response_publication_fixture(store.last_event_id(), true)?;
    let assistant = SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: fixture.assistant_base,
        content: "prior answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(STORED_ANCHOR.to_owned()),
    };
    store.append_batch(&[fixture.boundary, fixture.provenance, assistant])?;

    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();
    let mut loop_context = LoopContext::new("system");
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
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
    .await;

    let error = result
        .err()
        .ok_or_else(|| std::io::Error::other("anchored HTTP 400 unexpectedly succeeded"))?;
    assert_eq!(error.class(), ErrorClass::Terminal);
    let NornError::Provider(ProviderError::StreamError {
        reason,
        transient: None,
    }) = &error
    else {
        return Err(std::io::Error::other(format!(
            "anchored HTTP 400 returned an unexpected error: {error:?}",
        ))
        .into());
    };
    assert!(reason.contains("400"));
    assert!(!format!("{error:?}").contains(HOSTILE_BODY));
    assert!(!error.to_string().contains(HOSTILE_BODY));

    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("wiremock request recording is unavailable"))?;
    assert_eq!(
        requests.len(),
        1,
        "terminal anchored rejection must not retry"
    );
    let payload: Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(payload["store"], Value::Bool(true));
    assert_eq!(payload["previous_response_id"], STORED_ANCHOR);

    let dumped = std::fs::read_to_string(debug_dump)?;
    assert!(!dumped.contains(HOSTILE_BODY));
    Ok(())
}
