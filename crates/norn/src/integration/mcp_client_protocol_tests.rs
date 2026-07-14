use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use super::super::mcp_wire::{JsonRpcError, JsonRpcResponse};
use super::{McpClient, McpServerConfig, McpTransport, Transport};
use crate::error::IntegrationError;
use crate::integration::DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES;

struct MalformedShapeTransport {
    calls: Arc<AtomicUsize>,
}

struct DelayedTransport;

#[async_trait]
impl Transport for MalformedShapeTransport {
    async fn request(
        &self,
        _payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(JsonRpcResponse {
            jsonrpc: Some("2.0".to_owned()),
            id: Some(serde_json::json!(request_id)),
            result: Some(serde_json::json!({"tools": []})),
            error: Some(JsonRpcError {
                code: -32000,
                message: "ambiguous response".to_owned(),
            }),
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

#[async_trait]
impl Transport for DelayedTransport {
    async fn request(
        &self,
        _payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        tokio::time::sleep(Duration::from_secs(31)).await;
        Ok(JsonRpcResponse {
            jsonrpc: Some("2.0".to_owned()),
            id: Some(serde_json::json!(request_id)),
            result: Some(serde_json::json!({"tools": []})),
            error: None,
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

#[tokio::test]
async fn malformed_result_error_shape_invalidates_client_before_reuse()
-> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(AtomicUsize::new(0));
    let client = McpClient::from_transport(
        "malformed-shape",
        Box::new(MalformedShapeTransport {
            calls: Arc::clone(&calls),
        }),
    );

    let first = client
        .inner
        .rpc("tools/list", serde_json::json!({}))
        .await
        .err()
        .ok_or("malformed response shape unexpectedly succeeded")?;
    assert!(first.to_string().contains("exactly one"));
    assert!(!client.is_live());

    let second = client
        .inner
        .rpc("tools/list", serde_json::json!({}))
        .await
        .err()
        .ok_or("invalidated MCP client unexpectedly accepted another request")?;
    assert!(second.to_string().contains("no longer usable"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn direct_client_config_rejects_zero_connection_bounds_before_dial()
-> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (0, None, "max_inbound_message_bytes"),
        (
            DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
            Some(0),
            "request_timeout_ms",
        ),
    ];
    for (max_inbound_message_bytes, request_timeout_ms, expected_setting) in cases {
        let error = McpClient::connect(McpServerConfig {
            name: "invalid-bounds".to_owned(),
            transport: McpTransport::Http {
                url: "http://127.0.0.1:1".to_owned(),
            },
            env: HashMap::new(),
            headers: HashMap::new(),
            working_dir: None,
            max_inbound_message_bytes,
            request_timeout_ms,
        })
        .await
        .err()
        .ok_or("zero-valued direct MCP client setting was accepted")?;
        assert!(matches!(
            error,
            IntegrationError::McpInvalidClientSetting { setting }
                if setting == expected_setting
        ));
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn client_has_no_implicit_thirty_second_request_timeout()
-> Result<(), Box<dyn std::error::Error>> {
    let client = McpClient::from_transport("delayed", Box::new(DelayedTransport));
    let started = tokio::time::Instant::now();

    let tools = client.discover_tools().await?;

    assert!(tools.is_empty());
    assert_eq!(started.elapsed(), Duration::from_secs(31));
    Ok(())
}
