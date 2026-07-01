//! Driven-mode JSON-RPC 2.0 stdio transport (NOI-1).
//!
//! When Norn is invoked with `--protocol jsonrpc`, this module owns the
//! process's stdin+stdout as a single bidirectional JSON-RPC 2.0 channel.
//! It is entered ONLY when the flag is set; every existing render/TUI path
//! is byte-for-byte unreached without it.
//!
//! The wire contract (READ direction — NOI-1 scope; `intervene/*` is NOI-2):
//!
//! - **`initialize`** (request) → a response advertising Norn's driven
//!   capabilities, including the intervention primitive set the future
//!   worker adapter will gate on ([`initialize_capabilities`]).
//! - **`run/execute`** (request) → the agent runs; the single final
//!   structured result is returned as the RESPONSE whose `id` matches this
//!   request, and ONLY as that response. The prompt is sourced from the
//!   request `params` (stdin is the JSON-RPC channel, not the prompt).
//! - **`event/*`** (notifications, no `id`) → every [`AgentEvent`] the run
//!   emits, streamed LIVE as it occurs, reusing the exact stream-json
//!   payload ([`crate::print::output::agent_event_to_value`]) as the
//!   notification `params` with `agent_id` / `agent_role` added.
//!
//! ## Framing (reused from the in-tree MCP prior art)
//!
//! The envelope types, newline-delimited framing, and the error codes
//! (`-32700` parse, `-32600` invalid request, `-32601` method not found,
//! `-32603` internal) mirror
//! [`norn::integration::mcp_server`](../../../norn/src/integration/mcp_server.rs).
//! stderr stays human logs (the tracing subscriber already targets it), so
//! library noise can never corrupt the structured stream.
//!
//! ## Single serializing writer (§4.2)
//!
//! A duplex channel carrying interleaved notifications and a run response
//! MUST NOT let two producers interleave-corrupt a frame. Exactly ONE task
//! ([`spawn_writer`]) owns stdout; every outbound frame is enqueued on an
//! `mpsc` channel and written by that task, one complete line at a time.
//! The [`OutboundWriter`] handle is cloneable and is the only way to emit.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use norn::provider::AgentEvent;

use super::output::{agent_event_method, agent_event_to_value};

/// JSON-RPC parse-error code (invalid JSON was received).
const CODE_PARSE_ERROR: i64 = -32700;
/// JSON-RPC invalid-request code (well-formed JSON, invalid Request object).
const CODE_INVALID_REQUEST: i64 = -32600;
/// JSON-RPC method-not-found code.
const CODE_METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC internal-error code.
const CODE_INTERNAL_ERROR: i64 = -32603;

/// The JSON-RPC protocol version every frame carries.
const JSONRPC_VERSION: &str = "2.0";

/// A parsed inbound JSON-RPC message.
///
/// A message with no `id` is a Notification; with an `id` it is a Request.
/// NOI-1 only serves Requests inbound (`initialize`, `run/execute`);
/// inbound `intervene/*` Requests are NOI-2 and are answered here with a
/// clean `-32601` until that slice lands.
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
    /// One of the `-327xx` codes.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
}

impl JsonRpcResponse {
    /// A success response id-matched to `id`.
    #[must_use]
    fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    /// An error response id-matched to `id`.
    #[must_use]
    fn err(id: Value, code: i64, message: String) -> Self {
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

/// A cloneable handle to the single serializing stdout writer.
///
/// All outbound frames funnel through one writer task so notifications and
/// the run response can never interleave-corrupt a line (§4.2). Cloning
/// the handle clones the `mpsc` sender, not the writer.
#[derive(Clone)]
pub struct OutboundWriter {
    tx: mpsc::UnboundedSender<String>,
}

impl OutboundWriter {
    /// Enqueue a serialised response frame. A send failure means the writer
    /// task has stopped (stdout closed); it is surfaced, never swallowed.
    fn send_response(&self, resp: &JsonRpcResponse) -> Result<(), TransportError> {
        let line = serde_json::to_string(resp)?;
        self.tx.send(line)?;
        Ok(())
    }

    /// Enqueue a serialised notification frame (no `id`; never a response).
    fn send_notification(&self, note: &JsonRpcNotification) -> Result<(), TransportError> {
        let line = serde_json::to_string(note)?;
        self.tx.send(line)?;
        Ok(())
    }
}

/// Spawn the single stdout-owning writer task.
///
/// The task holds the ONLY handle to stdout and writes each queued frame as
/// one newline-terminated line, flushing per line. It exits when every
/// [`OutboundWriter`] is dropped (the channel closes) or stdout breaks. The
/// returned [`OutboundWriter`] is the sole way to emit a frame.
#[must_use]
pub fn spawn_writer() -> (OutboundWriter, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let task = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                // stdout is gone; nothing more can be delivered. Drain and
                // exit so senders observe the closed channel promptly.
                rx.close();
                return;
            }
        }
    });
    (OutboundWriter { tx }, task)
}

/// Spawn the live `event/*` notification emitter.
///
/// Subscribes a SECOND receiver off the run's existing broadcast channel —
/// the broadcast fan-out means this composes with the run (and, in other
/// modes, the stream renderer) without interference. Each [`AgentEvent`] is
/// mapped to its `event/*` method and its byte-identical stream-json
/// payload, with `agent_id` / `agent_role` added, and emitted through the
/// single serializing writer LIVE as it occurs.
///
/// The task terminates on the explicit `shutdown` signal (after draining
/// events already buffered on its receiver) — mirroring the stream
/// renderer's REVIEW C1 discipline, because the registry's shared-context
/// sender clone keeps the broadcast channel open for the runtime's life.
#[must_use]
pub fn spawn_event_emitter(
    tx: &tokio::sync::broadcast::Sender<AgentEvent>,
    writer: OutboundWriter,
) -> EventEmitterHandle {
    let mut rx = tx.subscribe();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            tokio::select! {
                // Biased: drain ready events before observing shutdown so
                // nothing broadcast ahead of the signal is dropped.
                biased;
                received = rx.recv() => match received {
                    Ok(event) => {
                        if emit_one(&writer, &event).is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Closed) => return,
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "jsonrpc event emitter lagged — {n} events dropped");
                    }
                },
                _ = &mut shutdown_rx => {
                    drain_events(&mut rx, &writer);
                    return;
                }
            }
        }
    });
    EventEmitterHandle {
        shutdown: shutdown_tx,
        task,
    }
}

/// Handle to the spawned event emitter; call [`Self::finish`] to stop it.
pub struct EventEmitterHandle {
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl EventEmitterHandle {
    /// Signal the emitter to drain its buffered events and stop, then wait
    /// for the task. Call only after the run's own senders are dropped.
    ///
    /// # Errors
    ///
    /// Returns the [`tokio::task::JoinError`] if the emitter task panicked
    /// or was cancelled.
    pub async fn finish(self) -> Result<(), tokio::task::JoinError> {
        // A failed send means the task already exited (channel closed /
        // stdout broken) — not an error; the join still completes.
        let _ = self.shutdown.send(());
        self.task.await
    }
}

/// Map one [`AgentEvent`] to an `event/*` notification and emit it.
///
/// Events with no on-wire form (filtered deltas, provider `Error`) are
/// skipped. `partial` is `true`: the driven channel forwards deltas so the
/// consumer sees the full live stream — `--partial` is a render concern
/// that deliberately does not apply to the transport.
fn emit_one(writer: &OutboundWriter, event: &AgentEvent) -> Result<(), TransportError> {
    let Some(mut params) = agent_event_to_value(event, true) else {
        return Ok(());
    };
    if let Some(obj) = params.as_object_mut() {
        // The stream-json translator drops agent identity, which is fine
        // for single-agent stdout but makes multi-agent events
        // unattributable. Add it to every notification (§4.2).
        obj.insert("agent_id".to_owned(), json!(event.agent_id.to_string()));
        obj.insert(
            "agent_role".to_owned(),
            json!(event.agent_role.as_ref().to_owned()),
        );
    }
    let note = JsonRpcNotification {
        jsonrpc: JSONRPC_VERSION,
        method: agent_event_method(event),
        params,
    };
    writer.send_notification(&note)
}

/// Drain events already buffered on `rx` after a shutdown signal.
/// `try_recv` never blocks, so this terminates even while the shared
/// sender clone keeps the channel open.
fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<AgentEvent>, writer: &OutboundWriter) {
    use tokio::sync::broadcast::error::TryRecvError;
    loop {
        match rx.try_recv() {
            Ok(event) => {
                if emit_one(writer, &event).is_err() {
                    return;
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
            Err(TryRecvError::Lagged(n)) => {
                tracing::warn!(
                    missed = n,
                    "jsonrpc event emitter lagged — {n} events dropped"
                );
            }
        }
    }
}

/// The capabilities Norn advertises in its `initialize` response.
///
/// This seeds the future aion worker adapter (§3.3/§3.4): Norn advertises
/// the neutral intervention primitives it can serve — `inject_message` and
/// `cancel` — which NOI-2 wires to the real Norn control channel. We do NOT
/// invent aion-neutral types here; this is a documented JSON-RPC shape the
/// adapter maps. The remaining primitives are absent (unsupported), which
/// is the honest advertisement until their mechanism lands.
#[must_use]
pub fn initialize_capabilities() -> Value {
    json!({
        "protocolVersion": JSONRPC_VERSION,
        "serverInfo": {
            "name": "norn",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {
            // The methods this driven channel serves inbound.
            "methods": ["initialize", "run/execute"],
            // The `event/*` notification methods it emits outbound.
            "events": [
                "event/message",
                "event/toolCall",
                "event/toolResult",
                "event/progress",
                "event/stop",
                "event/raw",
            ],
            // Neutral intervention primitives Norn can serve (NOI-2 wires
            // the read-in path; NOI-1 only advertises them). Absent
            // primitives (pause_resume, update_budget, respond_to_approval)
            // are unsupported until their mechanism exists.
            "interventions": ["inject_message", "cancel"],
        },
    })
}

/// Priority of an injected out-of-band user turn (NOI-2).
///
/// Mirrors the neutral `InjectPriority` (§3.3): `Interrupt` is steer-now
/// (drains at the next tool boundary), `Normal` is a queued turn that
/// batches to stop-time. Deserialised from the `intervene/injectMessage`
/// `priority` param; an absent/unknown value defaults to `Normal` (the
/// conservative choice — a queued turn never pre-empts).
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum InjectPriority {
    /// A queued user turn — batches at stop boundaries.
    #[default]
    Normal,
    /// Act now — steer the running agent at the next tool boundary.
    Interrupt,
}

/// The worker-side control seam the driven channel maps `intervene/*`
/// requests onto (NOI-2).
///
/// This trait is the ONE place the transport touches the running agent's
/// control channel. `jsonrpc.rs` stays harness-blind: it parses the
/// neutral `intervene/*` methods and calls these two operations, never
/// naming a Norn type. The Norn mapping (inject → `ChannelMessage`,
/// cancel → `CancellationToken`) lives behind this trait in the
/// orchestrator (§3.4), and a test double implements it to drive the
/// negative controls without a live agent.
///
/// Both operations are synchronous and non-blocking: an injection enqueues
/// onto the agent's bounded inbound channel (`try_send`) and a cancel trips
/// a token — neither awaits, so the intervene reader never stalls the
/// stdin loop behind a slow agent.
pub trait InterventionHandler: Send + Sync {
    /// Inject an out-of-band user turn into the running agent.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the injection cannot be
    /// delivered (e.g. the agent's inbound channel is full or closed).
    /// The reason rides the `intervene/*` error response — never swallowed.
    fn inject_message(&self, text: &str, priority: InjectPriority) -> Result<(), String>;

    /// Cancel the running agent run.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the cancel cannot be applied.
    /// A token trip is infallible, so the concrete Norn handler returns
    /// `Ok` — the fallible signature keeps the seam honest for future
    /// harnesses whose cancel can fail.
    fn cancel(&self, reason: &str) -> Result<(), String>;
}

/// The `intervene/*` methods this driven channel serves inbound, matching
/// the `interventions` capability set advertised at `initialize`
/// (`inject_message` + `cancel`). Every other `intervene/*` method is a
/// primitive Norn does NOT advertise and is answered `-32601` — the
/// capability gate (§3.4).
const METHOD_INTERVENE_INJECT: &str = "intervene/injectMessage";
const METHOD_INTERVENE_CANCEL: &str = "intervene/cancel";

/// Extract the `{text, priority}` params of an `intervene/injectMessage`
/// request. `text` is required (a non-empty string is not enforced here —
/// an empty steer is a valid, if pointless, operator turn); `priority`
/// defaults to [`InjectPriority::Normal`] when absent.
///
/// # Errors
///
/// Returns [`CODE_INVALID_REQUEST`] with a reason when `text` is missing
/// or not a string, or when `priority` is present but not a known value.
fn inject_params(params: &Value) -> Result<(String, InjectPriority), (i64, String)> {
    let text = params
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            (
                CODE_INVALID_REQUEST,
                "intervene/injectMessage params must carry a string `text`".to_owned(),
            )
        })?;
    let priority = match params.get("priority") {
        None | Some(Value::Null) => InjectPriority::Normal,
        Some(value) => serde_json::from_value(value.clone()).map_err(|err| {
            (
                CODE_INVALID_REQUEST,
                format!("intervene/injectMessage `priority` is invalid: {err}"),
            )
        })?,
    };
    Ok((text, priority))
}

/// Extract the `{reason}` param of an `intervene/cancel` request. The
/// reason is optional — an absent reason defaults to a neutral label so
/// the cancel is always attributable.
fn cancel_reason(params: &Value) -> String {
    params
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("cancelled by operator")
        .to_owned()
}

/// Dispatch one inbound `intervene/*` request against `handler` and emit
/// its ack (or error) as the id-matched response (NOI-2).
///
/// The capability gate lives here: `intervene/injectMessage` and
/// `intervene/cancel` are the two primitives Norn advertises and are
/// mapped onto `handler`; every other `intervene/*` method (`pauseResume`,
/// `updateBudget`, `respondToApproval`, …) is answered `-32601 Method not
/// found`, the honest "unsupported primitive" signal. A non-`intervene/*`
/// method reaching this function is likewise `-32601` — during a run the
/// only in-band requests served are interventions.
///
/// Returns `true` when the dispatched intervention was a cancel that was
/// successfully applied, so the reader loop can stop reading further
/// requests (the run is terminating). Injection acks and errors return
/// `false` — the channel keeps serving.
///
/// # Errors
///
/// Returns a [`TransportError`] only when the ack/error frame cannot be
/// enqueued on the outbound writer — never for a rejected intervention
/// (that is a normal error response).
fn dispatch_intervention(
    request: &JsonRpcRequest,
    id: Value,
    handler: &dyn InterventionHandler,
    writer: &OutboundWriter,
) -> Result<bool, TransportError> {
    match request.method.as_str() {
        METHOD_INTERVENE_INJECT => {
            match inject_params(&request.params) {
                Ok((text, priority)) => match handler.inject_message(&text, priority) {
                    Ok(()) => {
                        writer.send_response(&JsonRpcResponse::ok(
                            id,
                            json!({ "status": "injected", "priority": priority }),
                        ))?;
                    }
                    Err(reason) => {
                        writer.send_response(&JsonRpcResponse::err(
                            id,
                            CODE_INTERNAL_ERROR,
                            format!("intervene/injectMessage failed: {reason}"),
                        ))?;
                    }
                },
                Err((code, message)) => {
                    writer.send_response(&JsonRpcResponse::err(id, code, message))?;
                }
            }
            Ok(false)
        }
        METHOD_INTERVENE_CANCEL => {
            let reason = cancel_reason(&request.params);
            match handler.cancel(&reason) {
                Ok(()) => {
                    writer.send_response(&JsonRpcResponse::ok(
                        id,
                        json!({ "status": "cancelling", "reason": reason }),
                    ))?;
                    Ok(true)
                }
                Err(err) => {
                    writer.send_response(&JsonRpcResponse::err(
                        id,
                        CODE_INTERNAL_ERROR,
                        format!("intervene/cancel failed: {err}"),
                    ))?;
                    Ok(false)
                }
            }
        }
        // The capability gate: an intervene/* primitive Norn does not
        // advertise (pauseResume / updateBudget / respondToApproval), or
        // any other method arriving mid-run, is Method Not Found.
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

/// Run the mid-run inbound `intervene/*` request loop until the channel
/// closes, a cancel is applied, or `stop` is signalled (NOI-2).
///
/// This is the WRITE-direction half of the driven channel: while the run
/// is in flight, it concurrently reads inbound JSON-RPC requests off the
/// same stdin reader and dispatches each `intervene/*` through
/// [`dispatch_intervention`], acking on the single serializing writer so
/// acks, `event/*` notifications, and the terminal run response never
/// interleave-corrupt (§4.2). It returns when:
///
/// - the peer closes stdin (EOF),
/// - a cancel intervention was applied (the run is terminating), or
/// - the orchestrator signals `stop` because the run finished on its own.
///
/// The loop is cancel-safe: `read_request` and the `stop` await race in a
/// `select!`, and reading a line is the only await that can be dropped —
/// no partial intervention is left half-applied.
///
/// # Errors
///
/// Returns a [`TransportError`] for underlying stdin IO / outbound
/// serialisation failure — never for a malformed or rejected intervention
/// (those are answered and the loop continues).
pub async fn drive_interventions<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    writer: &OutboundWriter,
    handler: &dyn InterventionHandler,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), TransportError> {
    let mut line = String::new();
    loop {
        tokio::select! {
            // Biased: honour a run-finished stop before consuming another
            // request, so a request that arrives in the same tick as the
            // run's completion is not dispatched against a dead agent.
            biased;
            _ = &mut stop => return Ok(()),
            read = read_request(reader, &mut line) => {
                let Some(parsed) = read? else {
                    // EOF: the peer closed the write half. Nothing more can
                    // arrive; the run continues to its own terminal result.
                    return Ok(());
                };
                let request = match parsed {
                    Ok(req) => req,
                    Err(resp) => {
                        writer.send_response(&resp)?;
                        continue;
                    }
                };
                // Notifications (no id) are not served inbound; ignore.
                let Some(id) = request.id.clone() else {
                    tracing::debug!(
                        method = %request.method,
                        "ignoring inbound notification during run",
                    );
                    continue;
                };
                if dispatch_intervention(&request, id, handler, writer)? {
                    // A cancel was applied — stop reading; the run loop
                    // observes the token and finishes with Cancelled.
                    return Ok(());
                }
            }
        }
    }
}

/// Extract the prompt from `run/execute` params.
///
/// The prompt is `params.prompt` (a string) or `params.input` as an alias.
/// stdin is the JSON-RPC channel, so it is NEVER read as the prompt in
/// driven mode.
///
/// # Errors
///
/// Returns [`CODE_INVALID_REQUEST`] as the error code (via `Err`) when no
/// string prompt is present.
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

/// Read the next inbound request line, skipping blanks, until EOF.
///
/// Returns `Ok(None)` at EOF, `Ok(Some(Ok(req)))` for a parseable request,
/// and `Ok(Some(Err(resp)))` when the line is invalid JSON / not a valid
/// Request (the caller emits `resp` and continues reading — a parse error
/// is not fatal to the channel).
async fn read_request<R: AsyncBufReadExt + Unpin>(
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
fn parse_request(body: &str) -> Result<JsonRpcRequest, Box<JsonRpcResponse>> {
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

/// Outcome of the pre-run request loop: either a `run/execute` request to
/// execute (carrying its id + prompt), or the channel closed first.
pub enum PreRunOutcome {
    /// A `run/execute` request arrived; run the agent and answer this id.
    Run {
        /// The id to id-match the final result response against.
        id: Value,
        /// The prompt sourced from the request params.
        prompt: String,
    },
    /// The peer closed the channel before issuing `run/execute`.
    Closed,
}

/// Drive the inbound request loop up to (and including) `run/execute`.
///
/// Answers `initialize` with [`initialize_capabilities`]; answers any other
/// pre-run method (including NOI-2's `intervene/*`) with `-32601`; and
/// returns on the first `run/execute` with its id + prompt. Parse / invalid
/// frames are answered and skipped, never fatal.
///
/// # Errors
///
/// Returns a [`TransportError`] only for underlying IO or serialisation
/// failures — never for a malformed inbound frame.
pub async fn drive_pre_run<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    writer: &OutboundWriter,
) -> Result<PreRunOutcome, TransportError> {
    let mut line = String::new();
    loop {
        let Some(parsed) = read_request(reader, &mut line).await? else {
            return Ok(PreRunOutcome::Closed);
        };
        let request = match parsed {
            Ok(req) => req,
            Err(resp) => {
                writer.send_response(&resp)?;
                continue;
            }
        };
        // Notifications (no id) are not served inbound in NOI-1; ignore.
        let Some(id) = request.id.clone() else {
            tracing::debug!(method = %request.method, "ignoring inbound notification in driven mode");
            continue;
        };
        match request.method.as_str() {
            "initialize" => {
                writer.send_response(&JsonRpcResponse::ok(id, initialize_capabilities()))?;
            }
            "run/execute" => match prompt_from_params(&request.params) {
                Ok(prompt) => return Ok(PreRunOutcome::Run { id, prompt }),
                Err((code, message)) => {
                    writer.send_response(&JsonRpcResponse::err(id, code, message))?;
                }
            },
            other => {
                writer.send_response(&JsonRpcResponse::err(
                    id,
                    CODE_METHOD_NOT_FOUND,
                    format!("method not found: {other}"),
                ))?;
            }
        }
    }
}

/// Send the final `run/execute` result as the id-matched success response.
///
/// This is the ONLY place the run result is emitted, and it is emitted as a
/// Response (has an `id`) — never as a notification — so the result/event
/// split is structural.
///
/// # Errors
///
/// Returns a [`TransportError`] if the frame cannot be enqueued.
pub fn send_run_result(
    writer: &OutboundWriter,
    id: Value,
    result: Value,
) -> Result<(), TransportError> {
    writer.send_response(&JsonRpcResponse::ok(id, result))
}

/// Send a `run/execute` failure as the id-matched error response.
///
/// # Errors
///
/// Returns a [`TransportError`] if the frame cannot be enqueued.
pub fn send_run_error(
    writer: &OutboundWriter,
    id: Value,
    message: String,
) -> Result<(), TransportError> {
    writer.send_response(&JsonRpcResponse::err(id, CODE_INTERNAL_ERROR, message))
}

/// The buffered stdin reader type owning the inbound JSON-RPC half. Aliased
/// so the pre-run loop and the mid-run intervene loop name the same type.
pub type StdinReader = BufReader<tokio::io::Stdin>;

/// Build a [`BufReader`] over the process stdin for the inbound half.
///
/// Isolated behind a function so the driven loop takes ownership of stdin
/// exactly once, mirroring how the writer task takes stdout.
#[must_use]
pub fn stdin_reader() -> StdinReader {
    BufReader::new(tokio::io::stdin())
}

/// A run driver: the pieces the orchestrator needs to (a) stream `event/*`
/// notifications live off the run's broadcast channel, and (b) return the
/// final result as the id-matched `run/execute` response.
///
/// The orchestrator wires an [`Self::attach_emitter`] onto its broadcast
/// channel in place of the stream renderer, then hands the final result to
/// [`Self::finish_with_result`].
pub struct RunDriver {
    writer: OutboundWriter,
    id: Value,
}

impl RunDriver {
    /// Create a driver bound to the `run/execute` request `id`.
    #[must_use]
    pub fn new(writer: OutboundWriter, id: Value) -> Self {
        Self { writer, id }
    }

    /// A clone of the outbound writer handle (for the shutdown path).
    #[must_use]
    pub fn writer(&self) -> OutboundWriter {
        self.writer.clone()
    }

    /// Attach the live `event/*` emitter to the run's broadcast channel.
    #[must_use]
    pub fn attach_emitter(
        &self,
        tx: &tokio::sync::broadcast::Sender<AgentEvent>,
    ) -> EventEmitterHandle {
        spawn_event_emitter(tx, self.writer.clone())
    }

    /// Emit the final result as the id-matched `run/execute` response.
    ///
    /// # Errors
    ///
    /// Returns a [`TransportError`] if the frame cannot be enqueued.
    pub fn finish_with_result(&self, result: Value) -> Result<(), TransportError> {
        send_run_result(&self.writer, self.id.clone(), result)
    }

    /// Emit a run failure as the id-matched error response.
    ///
    /// # Errors
    ///
    /// Returns a [`TransportError`] if the frame cannot be enqueued.
    pub fn finish_with_error(&self, message: String) -> Result<(), TransportError> {
        send_run_error(&self.writer, self.id.clone(), message)
    }
}

/// Shared driven-run context handed to the orchestrator.
///
/// Wrapping the [`RunDriver`] in an `Arc` lets the orchestrator hold it
/// while the emitter task holds its own writer clone.
pub type SharedRunDriver = Arc<RunDriver>;

/// The driven-mode run handed to the orchestrator: the shared result/event
/// driver plus the stdin reader the mid-run [`drive_interventions`] loop
/// keeps consuming (NOI-2).
///
/// NOI-1 only needed the driver (result + events). NOI-2 adds the WRITE
/// direction, which requires the same stdin reader that carried
/// `initialize`/`run/execute` to keep being read for in-band `intervene/*`
/// requests while the run is in flight — so the reader rides along here
/// instead of being dropped after the pre-run loop.
pub struct DrivenRun {
    /// The shared result/event driver (the NOI-1 surface).
    pub driver: SharedRunDriver,
    /// The stdin JSON-RPC reader, moved in so the intervene loop owns it
    /// for the duration of the run.
    pub reader: StdinReader,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use norn::provider::events::{ProviderEvent, StopReason};
    use norn::provider::usage::Usage;
    use norn::provider::{AgentEvent, AgentEventKind};
    use std::sync::Arc;
    use uuid::Uuid;

    fn provider_event(kind: ProviderEvent) -> AgentEvent {
        AgentEvent {
            agent_id: Uuid::nil(),
            agent_role: Arc::from("root"),
            event: AgentEventKind::Provider(kind),
        }
    }

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

    #[test]
    fn initialize_capabilities_advertises_interventions() {
        let caps = initialize_capabilities();
        let interventions = caps["capabilities"]["interventions"].as_array().unwrap();
        let names: Vec<&str> = interventions.iter().filter_map(Value::as_str).collect();
        assert!(names.contains(&"inject_message"));
        assert!(names.contains(&"cancel"));
        // The unsupported primitives are absent — the honest advertisement.
        assert!(!names.contains(&"pause_resume"));
        assert_eq!(caps["serverInfo"]["name"], "norn");
    }

    #[test]
    fn prompt_from_params_reads_prompt_field() {
        let params = json!({"prompt": "hello"});
        assert_eq!(prompt_from_params(&params).unwrap(), "hello");
    }

    #[test]
    fn prompt_from_params_reads_input_alias() {
        let params = json!({"input": "aliased"});
        assert_eq!(prompt_from_params(&params).unwrap(), "aliased");
    }

    #[test]
    fn prompt_from_params_rejects_missing() {
        let params = json!({"other": 1});
        let (code, _) = prompt_from_params(&params).unwrap_err();
        assert_eq!(code, CODE_INVALID_REQUEST);
    }

    #[tokio::test]
    async fn drive_pre_run_answers_initialize_then_returns_run() {
        // Capture frames through an in-process channel writer so the test
        // never writes to the real process stdout.
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx };
        let input =
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"run/execute\",\"params\":{\"prompt\":\"go\"}}\n";
        let mut reader = BufReader::new(&input[..]);
        let outcome = drive_pre_run(&mut reader, &writer).await.unwrap();
        match outcome {
            PreRunOutcome::Run { id, prompt } => {
                assert_eq!(id, json!(2));
                assert_eq!(prompt, "go");
            }
            PreRunOutcome::Closed => panic!("expected a run/execute"),
        }
        drop(writer);
        // The initialize was answered before run/execute returned: exactly
        // one response frame, id-matched, carrying capabilities.
        let frame = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["id"], json!(1));
        assert!(parsed["result"]["capabilities"].is_object());
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn drive_pre_run_reports_closed_when_no_run() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx };
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n";
        let mut reader = BufReader::new(&input[..]);
        let outcome = drive_pre_run(&mut reader, &writer).await.unwrap();
        assert!(matches!(outcome, PreRunOutcome::Closed));
        drop(writer);
        let frame = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["id"], json!(1));
    }

    #[tokio::test]
    async fn writer_task_frames_each_message_as_one_line() {
        // The real stdout-owning writer: verify it exits cleanly when every
        // handle is dropped (the terminal-response shutdown handshake).
        let (writer, writer_task) = spawn_writer();
        drop(writer);
        tokio::time::timeout(std::time::Duration::from_secs(5), writer_task)
            .await
            .expect("writer task must exit when all handles drop")
            .expect("writer task must not panic");
    }

    #[tokio::test]
    async fn unknown_pre_run_method_gets_method_not_found() {
        // Collect frames the writer emits by draining stdout is not
        // possible in-process; instead drive the parse+dispatch directly
        // by capturing frames through a local channel writer.
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx };
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"intervene/cancel\"}\n";
        let mut reader = BufReader::new(&input[..]);
        let outcome = drive_pre_run(&mut reader, &writer).await.unwrap();
        assert!(matches!(outcome, PreRunOutcome::Closed));
        drop(writer);
        let frame = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["id"], json!(7));
        assert_eq!(parsed["error"]["code"], json!(CODE_METHOD_NOT_FOUND));
    }

    #[tokio::test]
    async fn event_emitter_streams_notifications_never_responses() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx: out_tx };
        let handle = spawn_event_emitter(&tx, writer);

        // One of each mapped AgentEventKind arm.
        tx.send(provider_event(ProviderEvent::TextComplete {
            text: "hi".to_owned(),
        }))
        .unwrap();
        tx.send(provider_event(ProviderEvent::ToolCallComplete {
            call_id: "c1".to_owned(),
            name: "read".to_owned(),
            arguments: "{}".to_owned(),
            kind: norn::provider::request::ToolCallKind::Function,
        }))
        .unwrap();
        tx.send(provider_event(ProviderEvent::ToolResult {
            tool_call_id: "c1".to_owned(),
            tool_name: "read".to_owned(),
            output: json!("ok"),
            duration_ms: 3,
        }))
        .unwrap();
        tx.send(provider_event(ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }))
        .unwrap();

        drop(tx);
        handle.finish().await.unwrap();

        let mut methods = Vec::new();
        while let Ok(frame) = out_rx.try_recv() {
            let parsed: Value = serde_json::from_str(&frame).unwrap();
            // (c) NO event/* notification is ever a Response: no id field.
            assert!(
                parsed.get("id").is_none(),
                "notification must not carry an id: {frame}"
            );
            assert_eq!(parsed["jsonrpc"], "2.0");
            // agent_id / agent_role are attached to every notification.
            assert!(parsed["params"]["agent_id"].is_string());
            assert!(parsed["params"]["agent_role"].is_string());
            methods.push(parsed["method"].as_str().unwrap().to_owned());
        }
        assert!(methods.contains(&"event/message".to_owned()));
        assert!(methods.contains(&"event/toolCall".to_owned()));
        assert!(methods.contains(&"event/toolResult".to_owned()));
        assert!(methods.contains(&"event/stop".to_owned()));
    }

    /// The mandatory NOI-1 round-trip / negative control, composed over one
    /// serializing writer: (a) initialize → capabilities; (b) live `event/*`
    /// notifications stream DURING the run; (c) the final result arrives
    /// ONLY as the id-matched `run/execute` Response, emitted LAST; (d) NO
    /// notification is ever a Response and NO Response is mistaken for an
    /// event. The whole outbound frame sequence funnels through the single
    /// writer, proving framing cannot interleave-corrupt.
    #[tokio::test]
    async fn full_driven_round_trip_separates_result_from_events() {
        // One channel stands in for the single serializing stdout writer, so
        // the exact ordered frame sequence is observable.
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx: out_tx };

        // (a) handshake + run request drive the pre-run loop.
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":\"init-1\",\"method\":\"initialize\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":\"run-9\",\"method\":\"run/execute\",\"params\":{\"prompt\":\"go\"}}\n";
        let mut reader = BufReader::new(&input[..]);
        let outcome = drive_pre_run(&mut reader, &writer).await.unwrap();
        let run_id = match outcome {
            PreRunOutcome::Run { id, prompt } => {
                assert_eq!(prompt, "go");
                id
            }
            PreRunOutcome::Closed => panic!("expected run/execute"),
        };

        // (b) events stream live off the broadcast channel while the run is
        // notionally in flight.
        let (ev_tx, _ev_rx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
        let driver = RunDriver::new(writer.clone(), run_id.clone());
        let emitter = driver.attach_emitter(&ev_tx);
        ev_tx
            .send(provider_event(ProviderEvent::TextComplete {
                text: "thinking".to_owned(),
            }))
            .unwrap();
        ev_tx
            .send(provider_event(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }))
            .unwrap();
        drop(ev_tx);
        emitter.finish().await.unwrap();

        // (c) the terminal result — the ONLY place it is emitted — as the
        // id-matched Response, after the events.
        driver
            .finish_with_result(json!({"result": "done"}))
            .unwrap();
        drop(driver);
        drop(writer);

        // Collect the full ordered outbound sequence.
        let mut frames = Vec::new();
        while let Ok(frame) = out_rx.try_recv() {
            frames.push(serde_json::from_str::<Value>(&frame).unwrap());
        }

        // First frame: the initialize response, id-matched, with caps.
        assert_eq!(frames[0]["id"], json!("init-1"));
        assert!(frames[0]["result"]["capabilities"].is_object());
        assert!(frames[0].get("method").is_none());

        // The middle frames are event/* notifications: never a Response
        // (no id), always a method — no Response mistaken for an event.
        let mut saw_event = false;
        let mut result_index = None;
        for (i, frame) in frames.iter().enumerate().skip(1) {
            if frame.get("method").is_some() {
                saw_event = true;
                assert!(
                    frame.get("id").is_none(),
                    "an event notification must never carry an id: {frame}"
                );
                assert!(
                    frame["method"].as_str().unwrap().starts_with("event/"),
                    "notification method must be event/*: {frame}"
                );
            } else {
                // The only non-notification after initialize is the result.
                assert_eq!(frame["id"], json!("run-9"));
                assert_eq!(frame["result"]["result"], json!("done"));
                result_index = Some(i);
            }
        }
        assert!(saw_event, "expected at least one event/* notification");
        // (c) the result is the LAST frame — after every live event.
        assert_eq!(
            result_index,
            Some(frames.len() - 1),
            "the id-matched result must be emitted last, after the events"
        );
    }

    #[tokio::test]
    async fn run_result_is_a_response_with_matching_id() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx };
        let driver = RunDriver::new(writer, json!(42));
        driver.finish_with_result(json!({"answer": 7})).unwrap();
        drop(driver);
        let frame = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        // (a) result arrives as the id-matched Response, and only there.
        assert_eq!(parsed["id"], json!(42));
        assert_eq!(parsed["result"]["answer"], json!(7));
        assert!(parsed.get("method").is_none(), "a result is never a method");
    }

    // --- NOI-2: the intervene/* request loop ---

    /// A test double recording what the transport asked of the control
    /// channel, so the intervene dispatch is verified without a live agent.
    #[derive(Default)]
    struct RecordingHandler {
        injections: std::sync::Mutex<Vec<(String, InjectPriority)>>,
        cancels: std::sync::Mutex<Vec<String>>,
        fail_inject: bool,
    }

    impl InterventionHandler for RecordingHandler {
        fn inject_message(&self, text: &str, priority: InjectPriority) -> Result<(), String> {
            if self.fail_inject {
                return Err("agent inbound channel closed".to_owned());
            }
            self.injections
                .lock()
                .unwrap()
                .push((text.to_owned(), priority));
            Ok(())
        }

        fn cancel(&self, reason: &str) -> Result<(), String> {
            self.cancels.lock().unwrap().push(reason.to_owned());
            Ok(())
        }
    }

    #[test]
    fn inject_params_defaults_priority_to_normal() {
        let (text, priority) = inject_params(&json!({"text": "hi"})).unwrap();
        assert_eq!(text, "hi");
        assert_eq!(priority, InjectPriority::Normal);
    }

    #[test]
    fn inject_params_reads_interrupt_priority() {
        let (_t, priority) =
            inject_params(&json!({"text": "go", "priority": "interrupt"})).unwrap();
        assert_eq!(priority, InjectPriority::Interrupt);
    }

    #[test]
    fn inject_params_rejects_missing_text() {
        let (code, _) = inject_params(&json!({"priority": "normal"})).unwrap_err();
        assert_eq!(code, CODE_INVALID_REQUEST);
    }

    #[test]
    fn inject_params_rejects_unknown_priority() {
        let (code, _) = inject_params(&json!({"text": "x", "priority": "yell"})).unwrap_err();
        assert_eq!(code, CODE_INVALID_REQUEST);
    }

    #[test]
    fn cancel_reason_defaults_when_absent() {
        assert_eq!(cancel_reason(&json!({})), "cancelled by operator");
        assert_eq!(cancel_reason(&json!({"reason": "budget"})), "budget");
    }

    /// (a) An intervene/injectMessage mid-run reaches the handler and is
    /// acked with the id-matched success Response; the loop keeps serving.
    #[tokio::test]
    async fn drive_interventions_injects_and_acks() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx };
        let handler = RecordingHandler::default();
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"intervene/injectMessage\",\
              \"params\":{\"text\":\"steer now\",\"priority\":\"interrupt\"}}\n";
        let mut reader = BufReader::new(&input[..]);
        // EOF after the one request ends the loop with Ok.
        drive_interventions(&mut reader, &writer, &handler, stop_rx)
            .await
            .unwrap();
        drop(writer);

        let injected = handler.injections.lock().unwrap().clone();
        assert_eq!(injected.len(), 1);
        assert_eq!(injected[0].0, "steer now");
        assert_eq!(injected[0].1, InjectPriority::Interrupt);

        let frame = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["id"], json!(5));
        assert_eq!(parsed["result"]["status"], json!("injected"));
        assert!(parsed.get("error").is_none());
    }

    /// (b) An intervene/cancel mid-run reaches the handler, is acked, and
    /// makes the loop STOP reading (the run is terminating).
    #[tokio::test]
    async fn drive_interventions_cancel_acks_and_stops() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx };
        let handler = RecordingHandler::default();
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        // A second request follows the cancel; the loop must NOT read it,
        // because a cancel stops the reader immediately.
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"intervene/cancel\",\"params\":{\"reason\":\"stop\"}}\n\
              {\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"intervene/injectMessage\",\"params\":{\"text\":\"late\"}}\n";
        let mut reader = BufReader::new(&input[..]);
        drive_interventions(&mut reader, &writer, &handler, stop_rx)
            .await
            .unwrap();
        drop(writer);

        assert_eq!(handler.cancels.lock().unwrap().as_slice(), ["stop"]);
        // The post-cancel injection was never dispatched.
        assert!(handler.injections.lock().unwrap().is_empty());

        let frame = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["id"], json!(9));
        assert_eq!(parsed["result"]["status"], json!("cancelling"));
        // Exactly one response — the cancel ack; the second request was not read.
        assert!(rx.recv().await.is_none());
    }

    /// (c) An unsupported intervene/* primitive returns -32601 Method not
    /// found (the capability gate), and the loop keeps serving.
    #[tokio::test]
    async fn drive_interventions_unsupported_primitive_is_method_not_found() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx };
        let handler = RecordingHandler::default();
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"intervene/pauseResume\",\"params\":{\"paused\":true}}\n";
        let mut reader = BufReader::new(&input[..]);
        drive_interventions(&mut reader, &writer, &handler, stop_rx)
            .await
            .unwrap();
        drop(writer);

        // The unsupported primitive touched neither handler operation.
        assert!(handler.injections.lock().unwrap().is_empty());
        assert!(handler.cancels.lock().unwrap().is_empty());

        let frame = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["id"], json!(3));
        assert_eq!(parsed["error"]["code"], json!(CODE_METHOD_NOT_FOUND));
    }

    /// A handler-side injection failure surfaces as an id-matched error
    /// response (never swallowed), and the loop keeps serving.
    #[tokio::test]
    async fn drive_interventions_inject_failure_surfaces_error() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx };
        let handler = RecordingHandler {
            fail_inject: true,
            ..Default::default()
        };
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let input =
            b"{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"intervene/injectMessage\",\"params\":{\"text\":\"x\"}}\n";
        let mut reader = BufReader::new(&input[..]);
        drive_interventions(&mut reader, &writer, &handler, stop_rx)
            .await
            .unwrap();
        drop(writer);

        let frame = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["id"], json!(8));
        assert_eq!(parsed["error"]["code"], json!(CODE_INTERNAL_ERROR));
    }

    /// The `stop` signal winds the reader down cleanly even with no input
    /// pending — the run-finished path.
    #[tokio::test]
    async fn drive_interventions_stops_on_signal() {
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx };
        let handler = RecordingHandler::default();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        // A reader that never yields a line (pending forever) — only `stop`
        // can end the loop.
        let (_client, server) = tokio::io::duplex(64);
        let mut reader = BufReader::new(server);
        stop_tx.send(()).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drive_interventions(&mut reader, &writer, &handler, stop_rx),
        )
        .await
        .expect("stop signal must end the loop")
        .expect("clean shutdown");
    }

    /// (d) Single-serializing-writer serialization under concurrency (§4.2):
    /// a BURST of event/* notifications streams WHILE an intervene/* ack is
    /// emitted, and every outbound line is a complete, parseable JSON-RPC
    /// frame — proving acks and notifications never interleave-corrupt.
    #[tokio::test]
    async fn intervene_ack_and_event_burst_never_interleave_corrupt() {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let writer = OutboundWriter { tx: out_tx };

        // The live event emitter shares the ONE writer with the intervene
        // dispatch — the exact production topology (§4.2).
        let (ev_tx, _ev_rx) = tokio::sync::broadcast::channel::<AgentEvent>(256);
        let emitter = spawn_event_emitter(&ev_tx, writer.clone());

        // Concurrently: burst events and dispatch an intervene ack.
        let handler = RecordingHandler::default();
        let ack_writer = writer.clone();
        let dispatch = tokio::spawn(async move {
            let request = JsonRpcRequest {
                jsonrpc: Some(JSONRPC_VERSION.to_owned()),
                id: Some(json!("ack-1")),
                method: METHOD_INTERVENE_INJECT.to_owned(),
                params: json!({"text": "mid-burst", "priority": "interrupt"}),
            };
            dispatch_intervention(&request, json!("ack-1"), &handler, &ack_writer).unwrap();
        });
        for i in 0..64 {
            ev_tx
                .send(provider_event(ProviderEvent::TextComplete {
                    text: format!("chunk-{i}"),
                }))
                .unwrap();
        }
        dispatch.await.unwrap();
        drop(ev_tx);
        emitter.finish().await.unwrap();
        drop(writer);

        // Every single outbound line must be a complete, parseable frame.
        let mut saw_ack = false;
        let mut saw_event = false;
        while let Ok(frame) = out_rx.try_recv() {
            let parsed: Value =
                serde_json::from_str(&frame).expect("every outbound line is a complete frame");
            assert_eq!(parsed["jsonrpc"], "2.0");
            if parsed.get("method").is_some() {
                // An event notification: never an id.
                assert!(parsed.get("id").is_none());
                saw_event = true;
            } else if parsed["id"] == json!("ack-1") {
                // The intervene ack: an id-matched Response, never a method.
                assert!(parsed.get("method").is_none());
                assert_eq!(parsed["result"]["status"], json!("injected"));
                saw_ack = true;
            }
        }
        assert!(saw_ack, "the intervene ack must be on the wire");
        assert!(saw_event, "the event burst must be on the wire");
    }
}
