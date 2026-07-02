//! JSON-RPC 2.0 frame types, error codes, and inbound-line parsing for the
//! driven channel (`DRIVEN-PROTOCOL.md` "Transport and framing").
//!
//! The envelope types, newline-delimited framing, and the standard error
//! codes (`-32700` parse, `-32600` invalid request, `-32601` method not
//! found, `-32603` internal) mirror
//! [`norn::integration::mcp_server`](../../../../norn/src/integration/mcp_server.rs).
//! `-32000` is the driven channel's own invalid-state code (a `run/execute`
//! while a run is already in flight — the one-shot lifecycle). stderr stays
//! human logs (the tracing subscriber already targets it), so library noise
//! can never corrupt the structured stream.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

/// JSON-RPC parse-error code (invalid JSON was received).
pub(crate) const CODE_PARSE_ERROR: i64 = -32700;
/// JSON-RPC invalid-request code (well-formed JSON, invalid Request object).
pub(crate) const CODE_INVALID_REQUEST: i64 = -32600;
/// JSON-RPC method-not-found code.
pub(crate) const CODE_METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC internal-error code.
pub(crate) const CODE_INTERNAL_ERROR: i64 = -32603;
/// Driven-channel invalid-state code (implementation-defined server-error
/// range): a `run/execute` arrived while a run is already in flight. The
/// channel serves exactly one run per process (`DRIVEN-PROTOCOL.md`
/// "One-shot run lifecycle").
pub(crate) const CODE_RUN_BUSY: i64 = -32000;

/// The JSON-RPC protocol version every frame carries.
pub(crate) const JSONRPC_VERSION: &str = "2.0";

/// The `initialize` handshake method.
pub(crate) const METHOD_INITIALIZE: &str = "initialize";
/// The one-shot run method.
pub(crate) const METHOD_RUN_EXECUTE: &str = "run/execute";

/// A parsed inbound JSON-RPC message.
///
/// A message with no `id` is a Notification; with an `id` it is a Request.
/// The driven channel only serves Requests inbound (`initialize`,
/// `run/execute`, and mid-run `intervene/*`).
#[derive(Deserialize, Debug)]
pub struct JsonRpcRequest {
    /// Protocol tag; validated to be exactly `"2.0"` when present.
    #[serde(default)]
    pub jsonrpc: Option<String>,
    /// Correlation id. Absent for notifications.
    #[serde(default)]
    pub id: Option<Value>,
    /// The method name (namespace `/` member).
    pub method: String,
    /// Method parameters. Defaults to `null` when omitted.
    #[serde(default)]
    pub params: Value,
}

/// An outbound JSON-RPC response (result XOR error), id-matched to a
/// request.
#[derive(Serialize, Debug)]
pub struct JsonRpcResponse {
    /// Always `"2.0"`.
    pub jsonrpc: &'static str,
    /// The id of the request being answered (may be `null`).
    pub id: Value,
    /// The success payload. Mutually exclusive with `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The failure payload. Mutually exclusive with `result`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// An outbound JSON-RPC notification (no `id` — never a response, never
/// enters result capture).
#[derive(Serialize, Debug)]
pub struct JsonRpcNotification {
    /// Always `"2.0"`.
    pub jsonrpc: &'static str,
    /// The `event/*` method for this notification.
    pub method: &'static str,
    /// The notification payload.
    pub params: Value,
}

/// A JSON-RPC error object.
#[derive(Serialize, Debug)]
pub struct JsonRpcError {
    /// One of the codes in the `DRIVEN-PROTOCOL.md` error-code table.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
}

impl JsonRpcResponse {
    /// A success response id-matched to `id`.
    #[must_use]
    pub(crate) fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    /// An error response id-matched to `id`.
    #[must_use]
    pub(crate) fn err(id: Value, code: i64, message: String) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

/// Errors from the driven-mode transport itself (framing / IO), distinct
/// from agent-run errors which are reported as a `run/execute` error
/// response, not a transport failure.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// An IO error on stdin or stdout.
    #[error("jsonrpc transport I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A frame could not be serialised for transmission.
    #[error("jsonrpc frame serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The single serializing writer task has stopped (stdout closed), so no
    /// further frame can be delivered. Carries the underlying channel send
    /// error as its source rather than discarding it.
    #[error("jsonrpc outbound writer task has stopped")]
    WriterStopped(#[from] mpsc::error::SendError<String>),
}

/// Read the next inbound request line, skipping blanks, until EOF.
///
/// Returns `Ok(None)` at EOF, `Ok(Some(Ok(req)))` for a parseable request,
/// and `Ok(Some(Err(resp)))` when the line is invalid JSON / not a valid
/// Request (the caller emits `resp` and continues reading — a parse error
/// is not fatal to the channel).
pub(crate) async fn read_request<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    line: &mut String,
) -> Result<Option<Result<JsonRpcRequest, Box<JsonRpcResponse>>>, TransportError> {
    loop {
        line.clear();
        let n = reader.read_line(line).await?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return Ok(Some(parse_request(trimmed)));
    }
}

/// Parse and validate one inbound line into a [`JsonRpcRequest`], or a
/// boxed error response to emit. The error is boxed to keep the `Result`'s
/// `Err` variant small (a bare [`JsonRpcResponse`] is large).
pub(crate) fn parse_request(body: &str) -> Result<JsonRpcRequest, Box<JsonRpcResponse>> {
    let request: JsonRpcRequest = match serde_json::from_str(body) {
        Ok(req) => req,
        Err(e) => {
            return Err(Box::new(JsonRpcResponse::err(
                Value::Null,
                CODE_PARSE_ERROR,
                format!("parse error: {e}"),
            )));
        }
    };
    if let Some(version) = request.jsonrpc.as_deref()
        && version != JSONRPC_VERSION
    {
        return Err(Box::new(JsonRpcResponse::err(
            request.id.clone().unwrap_or(Value::Null),
            CODE_INVALID_REQUEST,
            format!("unsupported jsonrpc version: {version}"),
        )));
    }
    Ok(request)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_rejects_bad_json() {
        let resp = parse_request("not json").unwrap_err();
        assert_eq!(resp.error.unwrap().code, CODE_PARSE_ERROR);
    }

    #[test]
    fn parse_request_rejects_wrong_version() {
        let resp = parse_request(r#"{"jsonrpc":"1.0","id":1,"method":"initialize"}"#).unwrap_err();
        let err = resp.error.unwrap();
        assert_eq!(err.code, CODE_INVALID_REQUEST);
    }

    #[test]
    fn parse_request_accepts_missing_version() {
        // A missing jsonrpc tag is tolerated (only a wrong one is rejected).
        let req = parse_request(r#"{"id":1,"method":"initialize"}"#).unwrap();
        assert_eq!(req.method, "initialize");
    }
}
