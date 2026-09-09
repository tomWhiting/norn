//! Single input owner, explicit lifecycle negotiation and ordered run admission.

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::capabilities::initialize_capabilities;
use super::frames::{
    CODE_INTERNAL_ERROR, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND, CODE_RUN_BUSY,
    JsonRpcRequest, JsonRpcResponse, METHOD_INITIALIZE, METHOD_RUN_EXECUTE, TransportError,
    parse_request,
};
use super::interventions::{InterventionHandler, dispatch_intervention};
use super::run::prompt_from_params;
use super::writer::OutboundWriter;

/// One validated request delivered to the runtime owner.
pub struct AcceptedRun {
    /// Caller correlation identifier.
    pub id: Value,
    /// Validated prompt.
    pub prompt: String,
    /// Lifecycle selected before accepting this request.
    pub persistent: bool,
}

enum Command {
    Bind(Arc<dyn InterventionHandler>),
    Finish(JsonRpcResponse, bool, oneshot::Sender<()>),
    Shutdown,
}

struct Admission {
    active: Option<Value>,
    handler: Option<Arc<dyn InterventionHandler>>,
    pending: Vec<JsonRpcRequest>,
    controls_ready: bool,
    persistent: bool,
    started: bool,
}

impl Drop for Admission {
    fn drop(&mut self) {
        if let Some(handler) = self.handler.take()
            && let Err(error) =
                handler.cancel("driven input owner stopped before the run completed")
        {
            tracing::error!(%error, "failed to cancel run after driven input failure");
        }
    }
}

impl Admission {
    fn initialize(
        &mut self,
        id: Value,
        params: &Value,
        writer: &OutboundWriter,
    ) -> Result<(), TransportError> {
        if let Some(requested) = params.get("runLifecycle") {
            let persistent = match requested.as_str() {
                Some("persistent") => true,
                Some("one_shot") => false,
                _ => {
                    return writer.send_response(&JsonRpcResponse::err(
                        id,
                        CODE_INVALID_REQUEST,
                        "initialize runLifecycle must be one_shot or persistent".to_owned(),
                    ));
                }
            };
            if self.started && persistent != self.persistent {
                return writer.send_response(&JsonRpcResponse::err(
                    id,
                    CODE_RUN_BUSY,
                    "runLifecycle cannot change after execution has started".to_owned(),
                ));
            }
            self.persistent = persistent;
        }
        let mut capabilities = initialize_capabilities();
        capabilities["capabilities"]["runLifecycle"] = Value::String(
            if self.persistent {
                "persistent"
            } else {
                "one_shot"
            }
            .to_owned(),
        );
        writer.send_response(&JsonRpcResponse::ok(id, capabilities))
    }

    fn admit(
        &mut self,
        request: JsonRpcRequest,
        writer: &OutboundWriter,
        runs: &mpsc::UnboundedSender<AcceptedRun>,
    ) -> Result<(), TransportError> {
        let Some(id) = request.id.clone() else {
            tracing::debug!(method = %request.method, "ignoring driven inbound notification");
            return Ok(());
        };
        match request.method.as_str() {
            METHOD_INITIALIZE => self.initialize(id, &request.params, writer),
            METHOD_RUN_EXECUTE => {
                if self.active.is_some() {
                    return writer.send_response(&JsonRpcResponse::err(
                        id,
                        CODE_RUN_BUSY,
                        "run already active; wait for its terminal response".to_owned(),
                    ));
                }
                match prompt_from_params(&request.params) {
                    Ok(prompt) => {
                        self.started = true;
                        self.active = Some(id.clone());
                        runs.send(AcceptedRun {
                            id,
                            prompt,
                            persistent: self.persistent,
                        })
                        .map_err(|error| TransportError::Session(error.to_string()))
                    }
                    Err((code, message)) => {
                        writer.send_response(&JsonRpcResponse::err(id, code, message))
                    }
                }
            }
            "intervene/injectMessage" | "intervene/cancel" if self.active.is_some() => {
                if !self.controls_ready {
                    self.pending.push(request);
                    return Ok(());
                }
                if let Some(handler) = self.handler.as_ref() {
                    if dispatch_intervention(&request, id, handler.as_ref(), writer)? {
                        self.handler = None;
                    }
                    return Ok(());
                }
                writer.send_response(&JsonRpcResponse::err(
                    id,
                    CODE_INTERNAL_ERROR,
                    "run controls are unavailable while stopping".to_owned(),
                ))
            }
            other => writer.send_response(&JsonRpcResponse::err(
                id,
                CODE_METHOD_NOT_FOUND,
                format!("method not found in the current state: {other}"),
            )),
        }
    }

    fn finish(
        &mut self,
        response: &JsonRpcResponse,
        writer: &OutboundWriter,
    ) -> Result<(), TransportError> {
        if self.active.as_ref() != Some(&response.id) {
            return Err(TransportError::Session(
                "terminal response does not match the active request".to_owned(),
            ));
        }
        for request in self.pending.drain(..) {
            if let Some(id) = request.id {
                writer.send_response(&JsonRpcResponse::err(
                    id,
                    CODE_INTERNAL_ERROR,
                    "run ended before its controls became available".to_owned(),
                ))?;
            }
        }
        writer.send_response(response)?;
        self.active = None;
        self.handler = None;
        self.controls_ready = false;
        Ok(())
    }
}

/// Runtime-side control of the connection's single input owner.
#[derive(Clone)]
pub struct SessionControl {
    tx: mpsc::UnboundedSender<Command>,
}

impl SessionControl {
    /// Install the current run's controls.
    ///
    /// # Errors
    /// Returns a transport error when the input owner has ended.
    pub fn bind(&self, handler: Arc<dyn InterventionHandler>) -> Result<(), TransportError> {
        self.send(Command::Bind(handler))
    }

    /// Publish a terminal response and advance the negotiated lifecycle.
    ///
    /// # Errors
    /// Returns a transport error when publication cannot complete.
    pub async fn finish(&self, response: JsonRpcResponse) -> Result<(), TransportError> {
        self.publish(response, false).await
    }

    /// Publish the last response and close admission atomically.
    ///
    /// # Errors
    /// Returns a transport error when publication cannot complete.
    pub async fn finish_and_close(&self, response: JsonRpcResponse) -> Result<(), TransportError> {
        self.publish(response, true).await
    }

    async fn publish(&self, response: JsonRpcResponse, close: bool) -> Result<(), TransportError> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Finish(response, close, tx))?;
        rx.await
            .map_err(|error| TransportError::Session(error.to_string()))
    }

    /// End admission after teardown or an unrecoverable failure.
    ///
    /// # Errors
    /// Returns a transport error when the input owner has already ended.
    pub fn shutdown(&self) -> Result<(), TransportError> {
        self.send(Command::Shutdown)
    }

    fn send(&self, command: Command) -> Result<(), TransportError> {
        self.tx
            .send(command)
            .map_err(|error| TransportError::Session(error.to_string()))
    }
}

/// Input owner handles and process-lifetime cancellation.
pub struct SessionInput {
    /// Runtime control handle.
    pub control: SessionControl,
    /// Accepted requests; no overlap is admitted.
    pub runs: mpsc::UnboundedReceiver<AcceptedRun>,
    /// Process-lifetime cancellation, distinct from each run's child token.
    pub cancel: CancellationToken,
    /// Input owner's terminal result.
    pub task: tokio::task::JoinHandle<Result<(), TransportError>>,
}

/// Start the input owner with one-shot behavior until explicit negotiation.
#[must_use]
pub fn spawn_session_input<R>(reader: R, writer: OutboundWriter) -> SessionInput
where
    R: AsyncBufRead + Unpin + Send + 'static,
{
    let (tx, commands) = mpsc::unbounded_channel();
    let (runs_tx, runs) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let task = tokio::spawn(drive_session(
        reader,
        writer,
        commands,
        runs_tx,
        cancel.clone(),
    ));
    SessionInput {
        control: SessionControl { tx },
        runs,
        cancel,
        task,
    }
}

async fn drive_session<R: AsyncBufRead + Unpin>(
    reader: R,
    writer: OutboundWriter,
    mut commands: mpsc::UnboundedReceiver<Command>,
    runs: mpsc::UnboundedSender<AcceptedRun>,
    cancel: CancellationToken,
) -> Result<(), TransportError> {
    let mut lines = reader.lines();
    let mut admission = Admission {
        active: None,
        handler: None,
        pending: Vec::new(),
        controls_ready: false,
        persistent: false,
        started: false,
    };
    let mut eof = false;
    loop {
        if eof && admission.active.is_none() {
            return Ok(());
        }
        tokio::select! {
            biased;
            () = cancel.cancelled(), if admission.active.is_none() => return Ok(()),
            command = commands.recv() => match command {
                Some(Command::Bind(handler)) => {
                    admission.handler = Some(handler);
                    admission.controls_ready = true;
                    for request in std::mem::take(&mut admission.pending) {
                        admission.admit(request, &writer, &runs)?;
                    }
                }
                Some(Command::Finish(response, close, acknowledged)) => {
                    admission.finish(&response, &writer)?;
                    acknowledged.send(()).map_err(|()| TransportError::Session("runtime dropped terminal-response acknowledgment".to_owned()))?;
                    if close || !admission.persistent || cancel.is_cancelled() { return Ok(()); }
                }
                Some(Command::Shutdown) | None => {
                    if let Some(id) = admission.active.clone() {
                        admission.finish(&JsonRpcResponse::err(id, CODE_INTERNAL_ERROR, "connection closed before execution completed".to_owned()), &writer)?;
                    }
                    return Ok(());
                }
            },
            // next_line preserves incomplete prefixes when a command wins.
            line = lines.next_line(), if !eof => match line? {
                Some(line) if line.trim().is_empty() => {},
                Some(line) => match parse_request(&line) {
                    Ok(request) => admission.admit(request, &writer, &runs)?,
                    Err(response) => writer.send_response(&response)?,
                },
                None => eof = true,
            },
        }
    }
}
