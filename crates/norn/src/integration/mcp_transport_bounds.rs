//! Bounded inbound framing shared by MCP transports.

use std::future::Future;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use crate::error::IntegrationError;

pub(super) async fn with_request_timeout<F, T>(
    transport: &'static str,
    timeout_ms: Option<u64>,
    future: F,
) -> Result<T, IntegrationError>
where
    F: Future<Output = Result<T, IntegrationError>>,
{
    let Some(timeout_ms) = timeout_ms else {
        return future.await;
    };
    tokio::time::timeout(Duration::from_millis(timeout_ms), future)
        .await
        .map_err(|_elapsed| IntegrationError::McpRequestTimedOut {
            transport,
            timeout_ms,
        })?
}

/// Read one newline-delimited stdio frame without growing `line` past the
/// configured limit. The line terminator is consumed but not retained.
pub(super) async fn read_bounded_stdio_line<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
    limit_bytes: usize,
) -> Result<bool, IntegrationError>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();
    let mut pending_carriage_return = false;
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| IntegrationError::McpError {
                reason: format!("MCP stdio read failed: {error}"),
            })?;
        if available.is_empty() {
            if pending_carriage_return {
                append_bounded(line, b"\r", limit_bytes, "stdio")?;
            }
            return Ok(!line.is_empty());
        }
        if pending_carriage_return {
            if available.first() == Some(&b'\n') {
                reader.consume(1);
                return Ok(true);
            }
            append_bounded(line, b"\r", limit_bytes, "stdio")?;
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            let frame = available[..newline]
                .strip_suffix(b"\r")
                .unwrap_or(&available[..newline]);
            append_bounded(line, frame, limit_bytes, "stdio")?;
            reader.consume(newline + 1);
            return Ok(true);
        }
        let consumed = available.len();
        let frame = available.strip_suffix(b"\r").unwrap_or(available);
        append_bounded(line, frame, limit_bytes, "stdio")?;
        pending_carriage_return = frame.len() != available.len();
        reader.consume(consumed);
    }
}

/// Read a JSON HTTP response incrementally rather than asking reqwest to
/// allocate the complete authority-controlled body.
pub(super) async fn read_bounded_json_body(
    response: reqwest::Response,
    limit_bytes: usize,
    request_timeout_ms: Option<u64>,
) -> Result<Vec<u8>, IntegrationError> {
    let limit_u64 = u64::try_from(limit_bytes).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|length| length > limit_u64)
    {
        return Err(limit_error("HTTP JSON", limit_bytes));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            http_read_error(error, request_timeout_ms, "MCP HTTP body read failed")
        })?;
        append_bounded(&mut body, &chunk, limit_bytes, "HTTP JSON")?;
    }
    Ok(body)
}

pub(super) fn http_read_error(
    error: reqwest::Error,
    request_timeout_ms: Option<u64>,
    context: &'static str,
) -> IntegrationError {
    if error.is_timeout()
        && let Some(timeout_ms) = request_timeout_ms
    {
        return IntegrationError::McpRequestTimedOut {
            transport: "HTTP",
            timeout_ms,
        };
    }
    IntegrationError::McpError {
        reason: format!("{context}: {}", error.without_url()),
    }
}

/// Incremental SSE decoder bounded by the wire bytes and data payload of one
/// event. At most one parsed message is returned per call.
pub(super) struct SseDecoder {
    pending: Vec<u8>,
    data: String,
    event_bytes: usize,
    limit_bytes: usize,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::new(super::mcp_types::DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES)
    }
}

impl SseDecoder {
    pub(super) fn new(limit_bytes: usize) -> Self {
        Self {
            pending: Vec::new(),
            data: String::new(),
            event_bytes: 0,
            limit_bytes,
        }
    }

    /// Consume through at most one complete SSE event, leaving any remaining
    /// bytes in `chunk` for the caller to process after handling the message.
    pub(super) fn push_next(
        &mut self,
        chunk: &mut &[u8],
    ) -> Result<Option<serde_json::Value>, IntegrationError> {
        while let Some(newline) = chunk.iter().position(|byte| *byte == b'\n') {
            let current = *chunk;
            self.append_pending(&current[..newline])?;
            let wire_bytes = self.pending.len() + 1;
            let mut line = std::mem::take(&mut self.pending);
            *chunk = &current[newline + 1..];
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if !line.is_empty() {
                self.account_completed_line(wire_bytes)?;
            }
            if let Some(message) = self.process_line(&line)? {
                return Ok(Some(message));
            }
        }
        self.append_pending(chunk)?;
        *chunk = &[];
        Ok(None)
    }

    pub(super) fn finish(&mut self) -> Result<Option<serde_json::Value>, IntegrationError> {
        let mut final_line = std::mem::take(&mut self.pending);
        if !final_line.is_empty() {
            let wire_bytes = final_line.len();
            if final_line.last() == Some(&b'\r') {
                final_line.pop();
            }
            if !final_line.is_empty() {
                self.account_completed_line(wire_bytes)?;
            }
            if let Some(message) = self.process_line(&final_line)? {
                return Ok(Some(message));
            }
        }
        self.process_line(&[])
    }

    fn process_line(&mut self, line: &[u8]) -> Result<Option<serde_json::Value>, IntegrationError> {
        if line.is_empty() {
            self.event_bytes = 0;
            if self.data.is_empty() {
                return Ok(None);
            }
            let data = std::mem::take(&mut self.data);
            let message =
                serde_json::from_str(&data).map_err(|error| IntegrationError::McpError {
                    reason: format!("invalid JSON-RPC SSE event: {error}"),
                })?;
            return Ok(Some(message));
        }
        let line = std::str::from_utf8(line).map_err(|error| IntegrationError::McpError {
            reason: format!("MCP SSE event is not UTF-8: {error}"),
        })?;
        if let Some(value) = line.strip_prefix("data:") {
            let value = value.trim_start();
            let separator_bytes = usize::from(!self.data.is_empty());
            let additional = separator_bytes.saturating_add(value.len());
            if additional > self.limit_bytes.saturating_sub(self.data.len()) {
                return Err(limit_error("HTTP SSE", self.limit_bytes));
            }
            if separator_bytes == 1 {
                self.data.push('\n');
            }
            self.data.push_str(value);
        }
        Ok(None)
    }

    fn append_pending(&mut self, bytes: &[u8]) -> Result<(), IntegrationError> {
        let retained = self.event_bytes.saturating_add(self.pending.len());
        if bytes.len() > self.limit_bytes.saturating_sub(retained) {
            return Err(limit_error("HTTP SSE", self.limit_bytes));
        }
        self.pending.extend_from_slice(bytes);
        Ok(())
    }

    fn account_completed_line(&mut self, wire_bytes: usize) -> Result<(), IntegrationError> {
        if wire_bytes > self.limit_bytes.saturating_sub(self.event_bytes) {
            return Err(limit_error("HTTP SSE", self.limit_bytes));
        }
        self.event_bytes += wire_bytes;
        Ok(())
    }
}

fn append_bounded(
    destination: &mut Vec<u8>,
    bytes: &[u8],
    limit_bytes: usize,
    transport: &'static str,
) -> Result<(), IntegrationError> {
    if bytes.len() > limit_bytes.saturating_sub(destination.len()) {
        return Err(limit_error(transport, limit_bytes));
    }
    destination.extend_from_slice(bytes);
    Ok(())
}

fn limit_error(transport: &'static str, limit_bytes: usize) -> IntegrationError {
    IntegrationError::McpInboundMessageTooLarge {
        transport,
        limit_bytes,
    }
}

#[cfg(test)]
#[path = "mcp_transport_bounds_tests.rs"]
mod tests;
