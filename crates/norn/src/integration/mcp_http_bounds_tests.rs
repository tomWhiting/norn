use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::integration::{DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, McpClient};
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestError = Box<dyn std::error::Error + Send + Sync>;

fn client(
    server: &MockServer,
    max_inbound_message_bytes: usize,
    request_timeout_ms: Option<u64>,
) -> Result<McpClient, IntegrationError> {
    let transport = HttpTransport::new(
        server.uri(),
        &HashMap::new(),
        Arc::new(ClientProtocolState::new(Vec::new())),
        max_inbound_message_bytes,
        request_timeout_ms,
    )?;
    Ok(McpClient::from_transport(
        "bounds-fixture",
        Box::new(transport),
    ))
}

#[tokio::test]
async fn oversized_json_body_is_typed_and_does_not_disclose_content() -> Result<(), TestError> {
    const SECRET: &str = "oversized-http-json-secret";
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"tools": [], "padding": SECRET.repeat(8)}
    })
    .to_string();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    let error = client(&server, 64, None)?
        .refreshed_tools()
        .await
        .err()
        .ok_or("oversized JSON body was accepted")?;

    assert!(matches!(
        &error,
        IntegrationError::McpInboundMessageTooLarge {
            transport: "HTTP JSON",
            limit_bytes: 64
        }
    ));
    assert!(!error.to_string().contains(SECRET));
    assert!(!format!("{error:?}").contains(SECRET));
    Ok(())
}

#[tokio::test]
async fn oversized_sse_event_is_typed_and_does_not_disclose_content() -> Result<(), TestError> {
    const SECRET: &str = "oversized-http-sse-secret";
    let server = MockServer::start().await;
    let event = format!(
        "data: {}\n\n",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"tools": [], "padding": SECRET.repeat(8)}
        })
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(event, "text/event-stream"))
        .mount(&server)
        .await;

    let error = client(&server, 64, None)?
        .refreshed_tools()
        .await
        .err()
        .ok_or("oversized SSE event was accepted")?;

    assert!(matches!(
        &error,
        IntegrationError::McpInboundMessageTooLarge {
            transport: "HTTP SSE",
            limit_bytes: 64
        }
    ));
    assert!(!error.to_string().contains(SECRET));
    assert!(!format!("{error:?}").contains(SECRET));
    Ok(())
}

#[tokio::test]
async fn timeout_is_opt_in_and_none_has_no_client_deadline() -> Result<(), TestError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(75))
                .set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"tools": []}
                })),
        )
        .mount(&server)
        .await;

    tokio::time::timeout(
        Duration::from_secs(1),
        client(&server, DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, None)?.refreshed_tools(),
    )
    .await??;

    let error = client(&server, DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, Some(10))?
        .refreshed_tools()
        .await
        .err()
        .ok_or("explicit HTTP request timeout was not enforced")?;
    assert!(matches!(
        error,
        IntegrationError::McpRequestTimedOut {
            transport: "HTTP",
            timeout_ms: 10
        }
    ));
    Ok(())
}

#[tokio::test]
async fn timeout_covers_complete_sse_protocol_handling() -> Result<(), TestError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method": "tools/list"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"jsonrpc\":\"2.0\",\"id\":\"ping-1\",\"method\":\"ping\"}\n\n",
                "data: {\"jsonrpc\":\"2.0\",\"id\":\"ping-2\",\"method\":\"ping\"}\n\n",
                "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n",
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({"result": {}})))
        .respond_with(ResponseTemplate::new(202).set_delay(Duration::from_millis(200)))
        .mount(&server)
        .await;

    let error = client(&server, DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, Some(300))?
        .refreshed_tools()
        .await
        .err()
        .ok_or("cumulative SSE protocol work escaped the request deadline")?;

    assert!(matches!(
        error,
        IntegrationError::McpRequestTimedOut {
            transport: "HTTP",
            timeout_ms: 300
        }
    ));
    Ok(())
}

#[tokio::test]
async fn unsupported_content_type_does_not_disclose_header_value() -> Result<(), TestError> {
    const SECRET: &str = "content-type-secret-sentinel";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", format!("application/x-{SECRET}")),
        )
        .mount(&server)
        .await;

    let error = client(&server, DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, None)?
        .refreshed_tools()
        .await
        .err()
        .ok_or("unsupported response content type was accepted")?;
    let displayed = error.to_string();
    let debugged = format!("{error:?}");

    assert!(displayed.contains("unsupported MCP HTTP content type"));
    assert!(!displayed.contains(SECRET));
    assert!(!debugged.contains(SECRET));
    Ok(())
}
