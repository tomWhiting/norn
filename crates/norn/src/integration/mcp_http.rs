//! Streamable HTTP transport for the MCP client.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::sync::Mutex;

use super::mcp_client::{JsonRpcResponse, Transport};
use super::mcp_protocol::{ClientProtocolState, InboundMessage};
use super::mcp_transport_bounds::{
    SseDecoder, http_read_error, read_bounded_json_body, with_request_timeout,
};
use crate::error::IntegrationError;

const ACCEPT: &str = "application/json, text/event-stream";
const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";

pub(super) struct HttpTransport {
    client: reqwest::Client,
    url: String,
    state: Mutex<HttpState>,
    protocol: Arc<ClientProtocolState>,
    max_inbound_message_bytes: usize,
    request_timeout_ms: Option<u64>,
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
        protocol: Arc<ClientProtocolState>,
        max_inbound_message_bytes: usize,
        request_timeout_ms: Option<u64>,
    ) -> Result<Self, IntegrationError> {
        let headers = configured_headers
            .iter()
            .map(|(name, value)| parse_header(name, value))
            .collect::<Result<reqwest::header::HeaderMap, _>>()?;
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(0)
            .default_headers(headers);
        if let Some(timeout_ms) = request_timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }
        let client = builder
            .build()
            .map_err(|error| IntegrationError::McpError {
                reason: format!("failed to build HTTP client: {error}"),
            })?;
        Ok(Self {
            client,
            url,
            state: Mutex::new(HttpState::default()),
            protocol,
            max_inbound_message_bytes,
            request_timeout_ms,
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
        let response = request.send().await.map_err(|error| {
            http_read_error(error, self.request_timeout_ms, "MCP HTTP request failed")
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
        if !response.status().is_success() {
            return Err(IntegrationError::McpError {
                reason: format!("MCP HTTP status {}", response.status()),
            });
        }
        self.remember_session(&response).await;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/json")
            .to_ascii_lowercase();
        if content_type.starts_with("application/json") {
            let body = read_bounded_json_body(
                response,
                self.max_inbound_message_bytes,
                self.request_timeout_ms,
            )
            .await?;
            return serde_json::from_slice(&body).map_err(|error| IntegrationError::McpError {
                reason: format!("invalid JSON-RPC response: {error}"),
            });
        }
        if !content_type.starts_with("text/event-stream") {
            return Err(IntegrationError::McpError {
                reason: "unsupported MCP HTTP content type".to_owned(),
            });
        }
        self.response_from_sse(response, request_id).await
    }

    async fn response_from_sse(
        &self,
        response: reqwest::Response,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        let mut decoder = SseDecoder::new(self.max_inbound_message_bytes);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                http_read_error(error, self.request_timeout_ms, "MCP SSE body read failed")
            })?;
            let mut remaining = chunk.as_ref();
            while !remaining.is_empty() {
                let Some(message) = decoder.push_next(&mut remaining)? else {
                    continue;
                };
                if let Some(response) = self.handle_sse_message(message, request_id).await? {
                    return Ok(response);
                }
            }
        }
        if let Some(message) = decoder.finish()?
            && let Some(response) = self.handle_sse_message(message, request_id).await?
        {
            return Ok(response);
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
        match self.protocol.inspect(&message)? {
            InboundMessage::Consumed => Ok(None),
            InboundMessage::Reply(reply) => {
                self.answer_server_request(reply).await?;
                Ok(None)
            }
            InboundMessage::Response => {
                let response: JsonRpcResponse =
                    serde_json::from_value(message).map_err(|error| {
                        IntegrationError::McpError {
                            reason: format!("invalid JSON-RPC SSE response: {error}"),
                        }
                    })?;
                if response.id.as_ref() == Some(&serde_json::json!(request_id)) {
                    return Ok(Some(response));
                }
                Err(IntegrationError::McpError {
                    reason: format!("JSON-RPC response id did not match request {request_id}"),
                })
            }
        }
    }

    async fn answer_server_request(
        &self,
        message: serde_json::Value,
    ) -> Result<(), IntegrationError> {
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

#[async_trait]
impl Transport for HttpTransport {
    async fn request(
        &self,
        payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        with_request_timeout("HTTP", self.request_timeout_ms, async {
            let response = self.post(payload).await?;
            self.response_from_http(response, request_id).await
        })
        .await
    }

    async fn notify(&self, payload: String) -> Result<(), IntegrationError> {
        with_request_timeout("HTTP", self.request_timeout_ms, async {
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
        })
        .await
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

#[cfg(test)]
#[path = "mcp_http_adversarial_tests.rs"]
mod adversarial_tests;

#[cfg(test)]
#[path = "mcp_http_protocol_tests.rs"]
mod protocol_tests;

#[cfg(test)]
#[path = "mcp_http_bounds_tests.rs"]
mod bounds_tests;
