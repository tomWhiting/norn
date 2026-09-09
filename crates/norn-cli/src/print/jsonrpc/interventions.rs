//! Validated operator interventions; input ownership belongs to the session actor.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::frames::{
    CODE_INTERNAL_ERROR, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND, JsonRpcRequest,
    JsonRpcResponse, TransportError,
};
use super::writer::OutboundWriter;

/// Injection delivery priority, independent of audio playback control.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum InjectPriority {
    /// Queue until a stop boundary.
    #[default]
    Normal,
    /// Steer at the next tool boundary.
    Interrupt,
}

/// Nonblocking runtime operations exposed by the driven transport.
pub trait InterventionHandler: Send + Sync {
    /// Enqueue an operator message.
    ///
    /// # Errors
    /// Returns the delivery failure, including full or closed inbound queues.
    fn inject_message(&self, text: &str, priority: InjectPriority) -> Result<(), String>;

    /// Request cancellation of this run.
    ///
    /// # Errors
    /// Returns an explicit failure when the cancellation cannot be applied.
    fn cancel(&self, reason: &str) -> Result<(), String>;
}

fn inject_params(params: &Value) -> Result<(&str, InjectPriority), (i64, String)> {
    let text = params.get("text").and_then(Value::as_str).ok_or_else(|| {
        (
            CODE_INVALID_REQUEST,
            "intervene/injectMessage params must carry a string `text`".to_owned(),
        )
    })?;
    let priority = match params.get("priority") {
        None | Some(Value::Null) => InjectPriority::Normal,
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            (
                CODE_INVALID_REQUEST,
                format!("intervene/injectMessage `priority` is invalid: {error}"),
            )
        })?,
    };
    Ok((text, priority))
}

/// Answer one intervention. A true result means cancellation was applied;
/// the actual terminal outcome is still owned by the runtime.
pub(super) fn dispatch_intervention(
    request: &JsonRpcRequest,
    id: Value,
    handler: &dyn InterventionHandler,
    writer: &OutboundWriter,
) -> Result<bool, TransportError> {
    match request.method.as_str() {
        "intervene/injectMessage" => {
            let response = match inject_params(&request.params) {
                Ok((text, priority)) => match handler.inject_message(text, priority) {
                    Ok(()) => {
                        JsonRpcResponse::ok(id, json!({"status":"injected", "priority":priority}))
                    }
                    Err(error) => JsonRpcResponse::err(
                        id,
                        CODE_INTERNAL_ERROR,
                        format!("intervene/injectMessage failed: {error}"),
                    ),
                },
                Err((code, message)) => JsonRpcResponse::err(id, code, message),
            };
            writer.send_response(&response)?;
            Ok(false)
        }
        "intervene/cancel" => {
            let reason = request
                .params
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("cancelled by operator");
            match handler.cancel(reason) {
                Ok(()) => {
                    writer.send_response(&JsonRpcResponse::ok(
                        id,
                        json!({"status":"cancel_requested", "reason":reason}),
                    ))?;
                    Ok(true)
                }
                Err(error) => {
                    writer.send_response(&JsonRpcResponse::err(
                        id,
                        CODE_INTERNAL_ERROR,
                        format!("intervene/cancel failed: {error}"),
                    ))?;
                    Ok(false)
                }
            }
        }
        other => {
            writer.send_response(&JsonRpcResponse::err(
                id,
                CODE_METHOD_NOT_FOUND,
                format!("method not found: {other}"),
            ))?;
            Ok(false)
        }
    }
}
