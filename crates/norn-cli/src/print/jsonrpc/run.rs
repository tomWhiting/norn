//! Request-bound event emission and terminal responses on a persistent connection.

use std::sync::Arc;

use norn::provider::AgentEvent;
use serde_json::Value;

use super::emitter::{EventEmitterHandle, spawn_event_emitter};
use super::frames::{CODE_INTERNAL_ERROR, CODE_INVALID_REQUEST, JsonRpcResponse, TransportError};
use super::interventions::InterventionHandler;
use super::session::SessionControl;
use super::writer::OutboundWriter;

/// Extract the prompt, accepting `input` as its documented alias.
///
/// # Errors
/// Returns an invalid-request response description when no string is supplied.
pub fn prompt_from_params(params: &Value) -> Result<String, (i64, String)> {
    params
        .get("prompt")
        .or_else(|| params.get("input"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            (
                CODE_INVALID_REQUEST,
                "run/execute params must carry a string `prompt` (or `input`)".to_owned(),
            )
        })
}

/// One accepted run, correlated with its request and persistent connection.
pub struct RunDriver {
    writer: OutboundWriter,
    id: Value,
    session: SessionControl,
    persistent: bool,
}

impl RunDriver {
    /// Bind a request to the connection's admission owner.
    #[must_use]
    pub fn for_session(
        writer: OutboundWriter,
        id: Value,
        session: SessionControl,
        persistent: bool,
    ) -> Self {
        Self {
            writer,
            id,
            session,
            persistent,
        }
    }

    /// Whether the caller opted into a persistent conversation.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// Install the current run's controls.
    ///
    /// # Errors
    /// Returns a transport error if the input owner has ended.
    pub fn bind(&self, handler: Arc<dyn InterventionHandler>) -> Result<(), TransportError> {
        self.session.bind(handler)
    }

    /// Subscribe to this run's live events through the connection's writer.
    #[must_use]
    pub fn attach_emitter(
        &self,
        tx: &tokio::sync::broadcast::Sender<AgentEvent>,
    ) -> EventEmitterHandle {
        spawn_event_emitter(tx, self.writer.clone())
    }

    /// Publish a result after all run events have drained.
    ///
    /// # Errors
    /// Returns a transport error if terminal publication fails.
    pub async fn finish_with_result(&self, result: Value) -> Result<(), TransportError> {
        self.session
            .finish(JsonRpcResponse::ok(self.id.clone(), result))
            .await
    }

    /// Publish a failed accepted request without a second output envelope.
    ///
    /// # Errors
    /// Returns a transport error if terminal publication fails.
    pub async fn finish_with_error(&self, message: String) -> Result<(), TransportError> {
        self.session
            .finish_and_close(JsonRpcResponse::err(
                self.id.clone(),
                CODE_INTERNAL_ERROR,
                message,
            ))
            .await
    }

    /// Complete an explicit /exit without admitting another request.
    ///
    /// # Errors
    /// Returns a transport error when the terminal response cannot be published.
    pub async fn finish_and_close(&self, result: Value) -> Result<(), TransportError> {
        self.session
            .finish_and_close(JsonRpcResponse::ok(self.id.clone(), result))
            .await
    }
}

/// Shared result and event context for an accepted request.
pub type SharedRunDriver = Arc<RunDriver>;
