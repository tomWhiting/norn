use std::collections::HashMap;

use crate::integration::{
    DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, McpClient, McpClientConfig, McpTransport,
};
use wiremock::matchers::{body_json, body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestError = Box<dyn std::error::Error + Send + Sync>;

fn initialize_result(version: &str) -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "adversarial-fixture", "version": "1"}
    })
}

fn config(server: &MockServer) -> McpClientConfig {
    McpClientConfig {
        name: "adversarial-fixture".to_owned(),
        transport: McpTransport::Http { url: server.uri() },
        env: HashMap::new(),
        headers: HashMap::new(),
        working_dir: None,
        max_inbound_message_bytes: DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        request_timeout_ms: None,
    }
}

async fn rejected_initialize(response: serde_json::Value) -> Result<String, TestError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method": "initialize"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(&server)
        .await;

    let rendered = match McpClient::connect(config(&server)).await {
        Ok(_client) => {
            return Err(std::io::Error::other("hostile initialize response was accepted").into());
        }
        Err(error) => error.to_string(),
    };
    let requests = server
        .received_requests()
        .await
        .ok_or("wiremock request recording is unavailable")?;
    if requests.len() != 1 {
        return Err(std::io::Error::other(format!(
            "hostile initialize fixture received {} requests instead of one",
            requests.len()
        ))
        .into());
    }
    Ok(rendered)
}

#[tokio::test]
async fn real_http_rejects_hostile_initialize_envelopes() -> Result<(), TestError> {
    let valid_result = initialize_result("2025-11-25");
    let cases = [
        (
            serde_json::json!({"jsonrpc": "1.0", "id": 1, "result": valid_result}),
            "did not declare JSON-RPC 2.0",
        ),
        (
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 999,
                "result": initialize_result("2025-11-25")
            }),
            "response id did not match",
        ),
        (serde_json::json!([]), "did not declare JSON-RPC 2.0"),
        (
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}}
                }
            }),
            "invalid MCP initialize result",
        ),
        (
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": initialize_result("1900-01-01")
            }),
            "unsupported protocol version",
        ),
    ];

    for (response, expected) in cases {
        let rendered = rejected_initialize(response).await?;
        assert!(
            rendered.contains(expected),
            "unexpected initialize rejection: {rendered}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn real_http_rejects_repeated_pagination_cursor() -> Result<(), TestError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method": "initialize"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": initialize_result("2025-11-25")
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({
            "method": "notifications/initialized"
        })))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {"tools": [], "nextCursor": "same"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/list",
            "params": {"cursor": "same"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {"tools": [], "nextCursor": "same"}
        })))
        .mount(&server)
        .await;

    let rendered = match McpClient::connect(config(&server)).await {
        Ok(_client) => {
            return Err(std::io::Error::other(
                "repeated tools/list pagination cursor was accepted",
            )
            .into());
        }
        Err(error) => error.to_string(),
    };
    assert!(
        rendered.contains("repeated a pagination cursor"),
        "unexpected pagination rejection: {rendered}"
    );
    let requests = server
        .received_requests()
        .await
        .ok_or("wiremock request recording is unavailable")?;
    assert_eq!(requests.len(), 4);
    Ok(())
}
