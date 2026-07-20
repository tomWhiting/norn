use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
use crate::provider::openai::OpenAiProvider;
use crate::provider::openai::rate_limiter::RateLimiter;
use crate::provider::openai::request::{
    CATALOG_BACKEND_CODEX_SUBSCRIPTION, CATALOG_BACKEND_RESPONSES_API,
};
use crate::provider::request::{
    Message, MessageRole, ProviderConfig, ProviderRequest, SecretString, ToolCallCaller,
};
use crate::provider::traits::Provider as _;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn request() -> ProviderRequest {
    ProviderRequest {
        messages: vec![Message {
            response_items: Vec::new(),
            reasoning: Vec::new(),
            role: MessageRole::User,
            content: Some("hello".to_owned()),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: ToolCallCaller::Absent,
        }],
        tools: Vec::new(),
        model: "gpt-test".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    }
}

fn completed_stream(codex: bool) -> String {
    response_stream(
        codex,
        &serde_json::json!({
            "type": "response.metadata",
            "headers": {"x-codex-turn-state": "state-from-metadata"}
        }),
    )
}

fn response_stream(codex: bool, metadata: &serde_json::Value) -> String {
    let mut response = serde_json::json!({
        "id": "resp-turn-state",
        "status": "completed",
        "output": [],
        "usage": {
            "input_tokens": 1,
            "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
            "output_tokens": 0,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": 1
        }
    });
    if codex {
        response["end_turn"] = serde_json::Value::Bool(true);
    }
    let completed = serde_json::json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": response
    });
    format!(
        "event: response.metadata\ndata: {metadata}\n\nevent: response.completed\ndata: {completed}\n\n"
    )
}

fn sender(endpoint: String, context: Option<ProviderTurnContext>) -> TestResult<SenderProvider> {
    sender_with_debug(endpoint, context, None)
}

fn sender_with_debug(
    endpoint: String,
    context: Option<ProviderTurnContext>,
    debug_dump_file: Option<PathBuf>,
) -> TestResult<SenderProvider> {
    let auth_provider: Arc<dyn AuthProvider> =
        Arc::new(MockAuthProvider::single("test-access-token"));
    Ok(SenderProvider {
        executor: StreamExecutor {
            client: crate::provider::http_client::build_streaming_client(Duration::from_secs(5))?,
            endpoint,
            timeout: Duration::from_secs(5),
            max_retries: 0,
            retry_backoff: Duration::from_secs(1),
            retry_after_ceiling: None,
            rate_limiter: Arc::new(RateLimiter::new(60, Duration::from_secs(60))),
            auth_provider,
            request_headers: super::super::codex_turn::request_headers(context.as_ref()),
            debug_dump_file,
            backend_label: "responses",
        },
        catalog_backend: CATALOG_BACKEND_CODEX_SUBSCRIPTION,
        turn_context: context,
    })
}

async fn execute_sender(sender: &SenderProvider) -> TestResult<Vec<ProviderEvent>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    sender.execute(request(), tx).await?;
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event?);
    }
    Ok(events)
}

async fn recorded_requests(server: &MockServer) -> TestResult<Vec<wiremock::Request>> {
    server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is disabled").into())
}

fn request_header<'request>(
    request: &'request wiremock::Request,
    name: &str,
) -> Option<&'request str> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn request_body(request: &wiremock::Request) -> TestResult<serde_json::Value> {
    Ok(serde_json::from_slice(&request.body)?)
}

fn required_request(
    requests: &[wiremock::Request],
    index: usize,
) -> TestResult<&wiremock::Request> {
    requests
        .get(index)
        .ok_or_else(|| io::Error::other(format!("missing recorded request {index}")).into())
}

fn required_metadata_event(events: &[ProviderEvent]) -> TestResult<&serde_json::Value> {
    events
        .iter()
        .find_map(|event| match event {
            ProviderEvent::ResponseStreamEvent { event }
                if event.event_type() == "response.metadata" =>
            {
                Some(event.raw())
            }
            _ => None,
        })
        .ok_or_else(|| io::Error::other("missing response.metadata envelope").into())
}

#[tokio::test]
async fn codex_state_replays_within_turn_and_resets_for_the_next_turn() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-turn-state", "state-from-header")
                .set_body_string(completed_stream(true)),
        )
        .mount(&server)
        .await;
    let endpoint = format!("{}/responses", server.uri());
    let first_turn = ProviderTurnContext::for_turn("session-a", "turn-a")?;

    let first_events = execute_sender(&sender(endpoint.clone(), Some(first_turn.clone()))?).await?;
    execute_sender(&sender(endpoint.clone(), Some(first_turn.clone()))?).await?;
    let second_turn = ProviderTurnContext::for_turn("session-a", "turn-b")?;
    execute_sender(&sender(endpoint, Some(second_turn))?).await?;

    assert_eq!(
        first_turn
            .codex_turn_state_header()
            .as_ref()
            .and_then(|value| value.to_str().ok()),
        Some("state-from-header")
    );
    let raw_metadata = required_metadata_event(&first_events)?;
    assert_eq!(raw_metadata["headers"]["x-codex-turn-state"], "[REDACTED]");
    assert!(!raw_metadata.to_string().contains("state-from-metadata"));

    let requests = recorded_requests(&server).await?;
    assert_eq!(requests.len(), 3);
    let first = required_request(&requests, 0)?;
    let second = required_request(&requests, 1)?;
    let third = required_request(&requests, 2)?;
    assert_eq!(request_header(first, CODEX_TURN_STATE_HEADER), None);
    assert_eq!(
        request_header(second, CODEX_TURN_STATE_HEADER),
        Some("state-from-header")
    );
    assert_eq!(request_header(third, CODEX_TURN_STATE_HEADER), None);

    let first_body = request_body(first)?;
    let second_body = request_body(second)?;
    let third_body = request_body(third)?;
    assert_eq!(
        first_body["client_metadata"],
        second_body["client_metadata"]
    );
    assert_eq!(first_body["client_metadata"]["session_id"], "session-a");
    assert_eq!(first_body["client_metadata"]["thread_id"], "session-a");
    assert_eq!(first_body["client_metadata"]["turn_id"], "turn-a");
    assert_eq!(third_body["client_metadata"]["turn_id"], "turn-b");
    let first_metadata = first_body["client_metadata"]
        .as_object()
        .ok_or_else(|| io::Error::other("client_metadata was not an object"))?;
    assert_eq!(first_metadata.len(), 4);
    let nested = first_metadata["x-codex-turn-metadata"]
        .as_str()
        .ok_or_else(|| io::Error::other("nested Codex turn metadata was not a string"))?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(nested)?,
        serde_json::json!({
            "session_id": "session-a",
            "thread_id": "session-a",
            "turn_id": "turn-a",
            "request_kind": "turn"
        })
    );
    Ok(())
}

#[tokio::test]
async fn noncanonical_metadata_redacts_turn_state_from_every_output_sink() -> TestResult {
    const SECRET: &str = "LEAKED-SECRET-XYZ";

    let metadata = serde_json::json!({
        "type": "response.metadata",
        "headers": [
            {"x-codex-turn-state": SECRET},
            {"nested": {"X-CoDeX-TuRn-StAtE": [SECRET]}}
        ]
    });
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-turn-state", SECRET)
                .set_body_string(response_stream(true, &metadata)),
        )
        .mount(&server)
        .await;
    let directory = tempfile::tempdir()?;
    let debug_dump = directory.path().join("responses-debug.jsonl");
    let context = ProviderTurnContext::for_turn("session-redaction", "turn-redaction")?;

    let events = execute_sender(&sender_with_debug(
        format!("{}/responses", server.uri()),
        Some(context.clone()),
        Some(debug_dump.clone()),
    )?)
    .await?;

    assert_eq!(
        context
            .codex_turn_state_header()
            .as_ref()
            .and_then(|value| value.to_str().ok()),
        Some(SECRET)
    );
    let raw = required_metadata_event(&events)?;
    assert!(!raw.to_string().contains(SECRET));
    assert_eq!(raw["headers"][0]["x-codex-turn-state"], "[REDACTED]");
    assert_eq!(
        raw["headers"][1]["nested"]["X-CoDeX-TuRn-StAtE"],
        "[REDACTED]"
    );
    assert!(!format!("{events:?}").contains(SECRET));

    let debug_output = std::fs::read_to_string(debug_dump)?;
    assert!(!debug_output.contains(SECRET));
    assert!(debug_output.contains("[REDACTED]"));
    Ok(())
}

#[tokio::test]
async fn public_backend_ignores_private_turn_context() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-turn-state", "public-state")
                .set_body_string(completed_stream(false)),
        )
        .mount(&server)
        .await;
    let config = ProviderConfig {
        auth_source: AuthSource::ApiKey {
            key: SecretString::new("test-access-token"),
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
    let auth_provider: Arc<dyn AuthProvider> =
        Arc::new(MockAuthProvider::single("test-access-token"));
    let provider = OpenAiProvider::with_auth_provider(config, auth_provider)?;
    let context = ProviderTurnContext::for_turn("session-public", "turn-public")?;

    for _ in 0..2 {
        let mut stream = provider.stream_with_context(request(), context.clone())?;
        while let Some(event) = stream.next().await {
            event?;
        }
    }

    assert!(context.codex_turn_state_header().is_none());
    let requests = recorded_requests(&server).await?;
    assert_eq!(requests.len(), 2);
    for recorded in &requests {
        assert_eq!(request_header(recorded, CODEX_TURN_STATE_HEADER), None);
        assert!(request_body(recorded)?.get("client_metadata").is_none());
    }
    Ok(())
}

#[tokio::test]
async fn turn_context_rejects_another_credential_before_dispatch() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(completed_stream(false)),
        )
        .mount(&server)
        .await;
    let config_for = |key: &str| ProviderConfig {
        auth_source: AuthSource::ApiKey {
            key: SecretString::new(key),
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
    let first = OpenAiProvider::with_auth_provider(
        config_for("first-credential-sentinel"),
        Arc::new(MockAuthProvider::single("first-credential-sentinel")),
    )?;
    let second = OpenAiProvider::with_auth_provider(
        config_for("second-credential-sentinel"),
        Arc::new(MockAuthProvider::single("second-credential-sentinel")),
    )?;
    let context = ProviderTurnContext::for_turn("session-affinity", "turn-affinity")?;

    let mut stream = first.stream_with_context(request(), context.clone())?;
    while let Some(event) = stream.next().await {
        event?;
    }

    let mismatch = second.stream_with_context(request(), context.clone());
    let error = mismatch
        .err()
        .ok_or_else(|| io::Error::other("another credential reused a bound turn context"))?;
    assert!(matches!(
        error,
        ProviderError::ProviderStateIdentityMismatch
    ));
    let rendered = format!("{error:?} {context:?}");
    assert!(!rendered.contains("first-credential-sentinel"));
    assert!(!rendered.contains("second-credential-sentinel"));

    let requests = recorded_requests(&server).await?;
    assert_eq!(requests.len(), 1, "mismatch must fail before dispatch");
    Ok(())
}

#[test]
fn mapper_validates_metadata_before_seeding_turn_state() -> TestResult {
    let context = ProviderTurnContext::for_turn("session-mapper", "turn-mapper")?;
    let malformed = SseEvent {
        event_type: "response.metadata".to_owned(),
        data: serde_json::json!({
            "type": "response.future",
            "headers": {"x-codex-turn-state": "poison-state"}
        }),
    };
    let mut mapper = ResponsesMapper::with_turn_context(
        CATALOG_BACKEND_CODEX_SUBSCRIPTION,
        Some(context.clone()),
    );
    let malformed_events = mapper.map_event(&malformed);
    assert!(matches!(
        malformed_events.as_slice(),
        [Err(ProviderError::ResponseParseError { .. })]
    ));
    assert!(context.codex_turn_state_header().is_none());

    let invalid_header = SseEvent {
        event_type: "response.metadata".to_owned(),
        data: serde_json::json!({
            "type": "response.metadata",
            "headers": {"x-codex-turn-state": "invalid\nstate"}
        }),
    };
    let mut mapper = ResponsesMapper::with_turn_context(
        CATALOG_BACKEND_CODEX_SUBSCRIPTION,
        Some(context.clone()),
    );
    let invalid_events = mapper
        .map_event(&invalid_header)
        .into_iter()
        .collect::<Result<Vec<_>, ProviderError>>()?;
    let invalid_raw = required_metadata_event(&invalid_events)?;
    assert_eq!(invalid_raw["headers"]["x-codex-turn-state"], "[REDACTED]");
    assert!(context.codex_turn_state_header().is_none());

    let valid = SseEvent {
        event_type: "response.metadata".to_owned(),
        data: serde_json::json!({
            "type": "response.metadata",
            "headers": {"X-CoDeX-TuRn-StAtE": ["valid-state", "ignored-state"]}
        }),
    };
    let mut mapper = ResponsesMapper::with_turn_context(
        CATALOG_BACKEND_CODEX_SUBSCRIPTION,
        Some(context.clone()),
    );
    let valid_events = mapper
        .map_event(&valid)
        .into_iter()
        .collect::<Result<Vec<_>, ProviderError>>()?;
    let raw = required_metadata_event(&valid_events)?;
    assert_eq!(raw["headers"]["X-CoDeX-TuRn-StAtE"], "[REDACTED]");
    assert_eq!(
        context
            .codex_turn_state_header()
            .as_ref()
            .and_then(|value| value.to_str().ok()),
        Some("valid-state")
    );
    Ok(())
}

#[test]
fn public_catalog_mapper_has_no_private_turn_context() {
    let mapper = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_RESPONSES_API);
    assert!(mapper.turn_context.is_none());
}
