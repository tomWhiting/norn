//! Streamable HTTP transport for the MCP client.

use std::collections::HashMap;

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::sync::Mutex;

use super::mcp_client::{JsonRpcResponse, MCP_REQUEST_TIMEOUT, Transport};
use crate::error::IntegrationError;

const ACCEPT: &str = "application/json, text/event-stream";
const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";

pub(super) struct HttpTransport {
    client: reqwest::Client,
    url: String,
    state: Mutex<HttpState>,
}

#[derive(Default)]
struct HttpState {
    session_id: Option<reqwest::header::HeaderValue>,
    protocol_version: Option<String>,
}

struct AdmittedResponse {
    response: reqwest::Response,
    _permit: crate::resource::DescriptorPermit,
}

impl HttpTransport {
    pub(super) fn new(
        url: String,
        configured_headers: &HashMap<String, String>,
    ) -> Result<Self, IntegrationError> {
        let headers = configured_headers
            .iter()
            .map(|(name, value)| parse_header(name, value))
            .collect::<Result<reqwest::header::HeaderMap, _>>()?;
        let client = reqwest::Client::builder()
            .timeout(MCP_REQUEST_TIMEOUT)
            .pool_max_idle_per_host(0)
            .default_headers(headers)
            .build()
            .map_err(|error| IntegrationError::McpError {
                reason: format!("failed to build HTTP client: {error}"),
            })?;
        Ok(Self {
            client,
            url,
            state: Mutex::new(HttpState::default()),
        })
    }

    async fn post(&self, payload: String) -> Result<AdmittedResponse, IntegrationError> {
        let governor = crate::resource::DescriptorGovernor::global().map_err(|error| {
            IntegrationError::McpError {
                reason: format!("MCP HTTP descriptor admission unavailable: {error}"),
            }
        })?;
        let permit = governor
            .try_acquire(crate::resource::HTTP_REQUEST_PEAK)
            .map_err(|error| IntegrationError::McpError {
                reason: format!("MCP HTTP descriptor admission failed: {error}"),
            })?;
        let state = self.state.lock().await;
        let mut request = self
            .client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, ACCEPT)
            .body(payload);
        if let Some(session_id) = state.session_id.as_ref() {
            request = request.header(SESSION_HEADER, session_id);
        }
        if let Some(version) = state.protocol_version.as_deref() {
            request = request.header(PROTOCOL_HEADER, version);
        }
        drop(state);
        let response = request
            .send()
            .await
            .map_err(|error| IntegrationError::McpError {
                reason: format!("MCP HTTP request failed: {error}"),
            })?;
        Ok(AdmittedResponse {
            response,
            _permit: permit,
        })
    }

    async fn remember_session(&self, response: &reqwest::Response) {
        if let Some(session_id) = response.headers().get(SESSION_HEADER) {
            self.state.lock().await.session_id = Some(session_id.clone());
        }
    }

    async fn response_from_http(
        &self,
        admitted: AdmittedResponse,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        let response = admitted.response;
        self.remember_session(&response).await;
        if !response.status().is_success() {
            return Err(IntegrationError::McpError {
                reason: format!("MCP HTTP status {}", response.status()),
            });
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/json")
            .to_ascii_lowercase();
        if content_type.starts_with("application/json") {
            let body = response
                .text()
                .await
                .map_err(|error| IntegrationError::McpError {
                    reason: format!("MCP HTTP body read failed: {error}"),
                })?;
            return serde_json::from_str(&body).map_err(|error| IntegrationError::McpError {
                reason: format!("invalid JSON-RPC response: {error}"),
            });
        }
        if !content_type.starts_with("text/event-stream") {
            return Err(IntegrationError::McpError {
                reason: format!("unsupported MCP HTTP content type '{content_type}'"),
            });
        }
        self.response_from_sse(response, request_id).await
    }

    async fn response_from_sse(
        &self,
        response: reqwest::Response,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        let mut decoder = SseDecoder::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| IntegrationError::McpError {
                reason: format!("MCP SSE body read failed: {error}"),
            })?;
            for message in decoder.push(&chunk)? {
                if let Some(response) = self.handle_sse_message(message, request_id).await? {
                    return Ok(response);
                }
            }
        }
        for message in decoder.finish()? {
            if let Some(response) = self.handle_sse_message(message, request_id).await? {
                return Ok(response);
            }
        }
        Err(IntegrationError::McpError {
            reason: format!("MCP SSE stream ended before response {request_id}"),
        })
    }

    async fn handle_sse_message(
        &self,
        message: serde_json::Value,
        request_id: u64,
    ) -> Result<Option<JsonRpcResponse>, IntegrationError> {
        if let Some(method) = message.get("method").and_then(serde_json::Value::as_str) {
            if let Some(id) = message.get("id") {
                self.answer_server_request(method, id).await?;
            }
            return Ok(None);
        }
        let response: JsonRpcResponse =
            serde_json::from_value(message).map_err(|error| IntegrationError::McpError {
                reason: format!("invalid JSON-RPC SSE response: {error}"),
            })?;
        if response.id.as_ref() == Some(&serde_json::json!(request_id)) {
            return Ok(Some(response));
        }
        Ok(None)
    }

    async fn answer_server_request(
        &self,
        method: &str,
        id: &serde_json::Value,
    ) -> Result<(), IntegrationError> {
        let message = if method == "ping" {
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}})
        } else {
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "method not supported by norn"}
            })
        };
        let payload =
            serde_json::to_string(&message).map_err(|error| IntegrationError::McpError {
                reason: format!("failed to serialize MCP client response: {error}"),
            })?;
        let admitted = self.post(payload).await?;
        if admitted.response.status().is_success() {
            Ok(())
        } else {
            Err(IntegrationError::McpError {
                reason: format!("MCP HTTP response status {}", admitted.response.status()),
            })
        }
    }
}

fn parse_header(
    name: &str,
    value: &str,
) -> Result<(reqwest::header::HeaderName, reqwest::header::HeaderValue), IntegrationError> {
    let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
        IntegrationError::McpError {
            reason: format!("invalid MCP HTTP header name: {error}"),
        }
    })?;
    let value = reqwest::header::HeaderValue::from_str(value).map_err(|error| {
        IntegrationError::McpError {
            reason: format!("invalid value for MCP HTTP header '{name}': {error}"),
        }
    })?;
    Ok((name, value))
}

#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    data: String,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<serde_json::Value>, IntegrationError> {
        self.pending.extend_from_slice(chunk);
        let mut messages = Vec::new();
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<_> = self.pending.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(&line, &mut messages)?;
        }
        Ok(messages)
    }

    fn finish(&mut self) -> Result<Vec<serde_json::Value>, IntegrationError> {
        let mut messages = Vec::new();
        let final_line = std::mem::take(&mut self.pending);
        if !final_line.is_empty() {
            self.process_line(&final_line, &mut messages)?;
        }
        self.process_line(&[], &mut messages)?;
        Ok(messages)
    }

    fn process_line(
        &mut self,
        line: &[u8],
        messages: &mut Vec<serde_json::Value>,
    ) -> Result<(), IntegrationError> {
        if line.is_empty() {
            if !self.data.is_empty() {
                let message = serde_json::from_str(&self.data).map_err(|error| {
                    IntegrationError::McpError {
                        reason: format!("invalid JSON-RPC SSE event: {error}"),
                    }
                })?;
                messages.push(message);
                self.data.clear();
            }
            return Ok(());
        }
        let line = std::str::from_utf8(line).map_err(|error| IntegrationError::McpError {
            reason: format!("MCP SSE event is not UTF-8: {error}"),
        })?;
        if let Some(value) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(value.trim_start());
        }
        Ok(())
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn request(
        &self,
        payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        let exchange = async {
            let response = self.post(payload).await?;
            self.response_from_http(response, request_id).await
        };
        tokio::time::timeout(MCP_REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_elapsed| IntegrationError::McpError {
                reason: "MCP HTTP request timed out".to_owned(),
            })?
    }

    async fn notify(&self, payload: String) -> Result<(), IntegrationError> {
        let admitted = self.post(payload).await?;
        if admitted.response.status().is_success() {
            Ok(())
        } else {
            Err(IntegrationError::McpError {
                reason: format!(
                    "MCP HTTP notification status {}",
                    admitted.response.status()
                ),
            })
        }
    }

    fn supports_protocol_version(&self, version: &str) -> bool {
        matches!(version, "2025-11-25" | "2025-06-18" | "2025-03-26")
    }

    async fn set_protocol_version(&self, version: &str) {
        self.state.lock().await.protocol_version = Some(version.to_owned());
    }
}

#[cfg(test)]
#[path = "mcp_http_tests.rs"]
mod tests;
