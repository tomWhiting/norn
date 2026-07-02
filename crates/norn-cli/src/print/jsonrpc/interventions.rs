//! Mid-run `intervene/*` requests: the neutral control seam, param parsing,
//! dispatch, and the in-flight request loop (`DRIVEN-PROTOCOL.md`
//! "Interventions").

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;

use super::capabilities::initialize_capabilities;
use super::frames::{
    CODE_INTERNAL_ERROR, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND, CODE_RUN_BUSY,
    JsonRpcRequest, JsonRpcResponse, METHOD_INITIALIZE, METHOD_RUN_EXECUTE, TransportError,
    read_request,
};
use super::writer::OutboundWriter;

/// Priority of an injected out-of-band user turn.
///
/// `Interrupt` is steer-now (drains at the next tool boundary), `Normal` is
/// a queued turn that batches to stop-time (`DRIVEN-PROTOCOL.md`
/// "Interventions"). Deserialised from the `intervene/injectMessage`
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
/// requests onto.
///
/// This trait is the ONE place the transport touches the running agent's
/// control channel. The transport stays harness-blind: it parses the
/// neutral `intervene/*` methods and calls these two operations, never
/// naming a Norn type. The Norn mapping (inject → `ChannelMessage`,
/// cancel → `CancellationToken`) lives behind this trait in the
/// orchestrator (`DRIVEN-PROTOCOL.md` "Interventions"), and a test double
/// implements it to drive the negative controls without a live agent.
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

/// The degraded-mode handler: the run's control channel could not be
/// assembled (the harness message router failed to resolve — an assembly
/// invariant that should never fail on the driven path), so no intervention
/// can be applied. The stdin loop still runs with this handler installed,
/// so every `intervene/*` request the peer sends is READ and ANSWERED with
/// the internal error (`-32603`) instead of sitting unread until EOF
/// (`DRIVEN-PROTOCOL.md` "Degraded intervention mode").
pub struct UnavailableInterventionHandler;

impl UnavailableInterventionHandler {
    const REASON: &'static str =
        "the intervention control channel is unavailable this run (message router unresolved)";
}

impl InterventionHandler for UnavailableInterventionHandler {
    fn inject_message(&self, _text: &str, _priority: InjectPriority) -> Result<(), String> {
        Err(Self::REASON.to_owned())
    }

    fn cancel(&self, _reason: &str) -> Result<(), String> {
        Err(Self::REASON.to_owned())
    }
}

/// The `intervene/*` methods this driven channel serves inbound, matching
/// the `interventions` capability set advertised at `initialize`
/// (`inject_message` + `cancel`). Every other `intervene/*` method is a
/// primitive Norn does NOT advertise and is answered `-32601` — the
/// capability gate (`DRIVEN-PROTOCOL.md` "Interventions").
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

/// Dispatch one inbound mid-run request against `handler` and emit its ack
/// (or error) as the id-matched response.
///
/// The capability gate lives here: `intervene/injectMessage` and
/// `intervene/cancel` are the two primitives Norn advertises and are
/// mapped onto `handler`; every other `intervene/*` method (`pauseResume`,
/// `updateBudget`, `respondToApproval`, …) is answered `-32601 Method not
/// found`, the honest "unsupported primitive" signal. Two advertised
/// non-intervention methods are also answered mid-run: `initialize` is
/// idempotent and read-only, so it is re-served with the capabilities; a
/// second `run/execute` violates the one-shot lifecycle and is answered
/// with the typed invalid-state error (`-32000`), never `-32601` — the
/// method exists, the channel is busy. Anything else is `-32601`.
///
/// Returns `true` when the dispatched intervention was a cancel that was
/// successfully applied, so the reader loop can stop reading further
/// requests (the run is terminating). Every other outcome returns `false`
/// — the channel keeps serving.
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
                    // "cancel_requested", not "cancelled": the ack only
                    // guarantees the cancellation signal was applied. If the
                    // run reached its own terminal outcome in the same
                    // instant, the run/execute response reflects that actual
                    // outcome — the terminal stop reason is authoritative
                    // (`DRIVEN-PROTOCOL.md` "Cancel acknowledgement
                    // semantics").
                    writer.send_response(&JsonRpcResponse::ok(
                        id,
                        json!({ "status": "cancel_requested", "reason": reason }),
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
        // initialize is idempotent and read-only: re-serve the capabilities
        // so a consumer can re-inspect the contract mid-run.
        METHOD_INITIALIZE => {
            writer.send_response(&JsonRpcResponse::ok(id, initialize_capabilities()))?;
            Ok(false)
        }
        // The one-shot lifecycle: a second run/execute while a run is in
        // flight is an invalid-state error, typed distinctly from
        // method-not-found because the method IS advertised.
        METHOD_RUN_EXECUTE => {
            writer.send_response(&JsonRpcResponse::err(
                id,
                CODE_RUN_BUSY,
                "run already active: the driven channel serves exactly one run/execute per \
                 process (runLifecycle: one_shot)"
                    .to_owned(),
            ))?;
            Ok(false)
        }
        // The capability gate: an intervene/* primitive Norn does not
        // advertise (pauseResume / updateBudget / respondToApproval), or
        // any other unknown method arriving mid-run, is Method Not Found.
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
/// closes, a cancel is applied, or `stop` is signalled.
///
/// This is the WRITE-direction half of the driven channel: while the run
/// is in flight, it concurrently reads inbound JSON-RPC requests off the
/// same stdin reader and dispatches each request through
/// [`dispatch_intervention`], acking on the single serializing writer so
/// acks, `event/*` notifications, and the terminal run response never
/// interleave-corrupt. It returns when:
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::emitter::spawn_event_emitter;
    use super::super::frames::JSONRPC_VERSION;
    use super::*;
    use norn::provider::events::ProviderEvent;
    use norn::provider::{AgentEvent, AgentEventKind};
    use std::sync::Arc;
    use tokio::io::BufReader;
    use uuid::Uuid;

    fn provider_event(kind: ProviderEvent) -> AgentEvent {
        AgentEvent {
            agent_id: Uuid::nil(),
            agent_role: Arc::from("root"),
            event: AgentEventKind::Provider(kind),
        }
    }

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
        let (writer, mut rx) = OutboundWriter::test_channel();
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

    /// (b) An intervene/cancel mid-run reaches the handler, is acked with
    /// the signal-applied status, and makes the loop STOP reading (the run
    /// is terminating).
    #[tokio::test]
    async fn drive_interventions_cancel_acks_and_stops() {
        let (writer, mut rx) = OutboundWriter::test_channel();
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
        // The ack acknowledges the SIGNAL, not the outcome — the terminal
        // run response's stop reason stays authoritative.
        assert_eq!(parsed["result"]["status"], json!("cancel_requested"));
        // Exactly one response — the cancel ack; the second request was not read.
        assert!(rx.recv().await.is_none());
    }

    /// (c) An unsupported intervene/* primitive returns -32601 Method not
    /// found (the capability gate), and the loop keeps serving.
    #[tokio::test]
    async fn drive_interventions_unsupported_primitive_is_method_not_found() {
        let (writer, mut rx) = OutboundWriter::test_channel();
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

    /// A second run/execute while the run is in flight gets the typed
    /// invalid-state error (-32000), never -32601 — the method IS
    /// advertised — and the loop keeps serving interventions after it.
    #[tokio::test]
    async fn drive_interventions_second_run_execute_is_busy_not_method_not_found() {
        let (writer, mut rx) = OutboundWriter::test_channel();
        let handler = RecordingHandler::default();
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"run/execute\",\"params\":{\"prompt\":\"again\"}}\n\
              {\"jsonrpc\":\"2.0\",\"id\":12,\"method\":\"intervene/injectMessage\",\"params\":{\"text\":\"still served\"}}\n";
        let mut reader = BufReader::new(&input[..]);
        drive_interventions(&mut reader, &writer, &handler, stop_rx)
            .await
            .unwrap();
        drop(writer);

        let busy = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&busy).unwrap();
        assert_eq!(parsed["id"], json!(11));
        assert_eq!(parsed["error"]["code"], json!(CODE_RUN_BUSY));
        assert!(
            parsed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("one_shot"),
            "the busy error names the one-shot lifecycle: {parsed}"
        );

        // The channel kept serving: the follow-up injection was dispatched
        // and acked.
        let ack = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&ack).unwrap();
        assert_eq!(parsed["id"], json!(12));
        assert_eq!(parsed["result"]["status"], json!("injected"));
        assert_eq!(handler.injections.lock().unwrap().len(), 1);
    }

    /// A mid-run initialize is idempotent: re-answered with the
    /// capabilities, and the loop keeps serving.
    #[tokio::test]
    async fn drive_interventions_reserves_initialize_midrun() {
        let (writer, mut rx) = OutboundWriter::test_channel();
        let handler = RecordingHandler::default();
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":21,\"method\":\"initialize\"}\n";
        let mut reader = BufReader::new(&input[..]);
        drive_interventions(&mut reader, &writer, &handler, stop_rx)
            .await
            .unwrap();
        drop(writer);

        let frame = rx.recv().await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["id"], json!(21));
        assert!(parsed["result"]["capabilities"].is_object());
    }

    /// A handler-side injection failure surfaces as an id-matched error
    /// response (never swallowed), and the loop keeps serving.
    #[tokio::test]
    async fn drive_interventions_inject_failure_surfaces_error() {
        let (writer, mut rx) = OutboundWriter::test_channel();
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

    /// Degraded mode (router unresolved): every advertised intervention is
    /// still READ and answered -32603 with the unavailability reason — the
    /// peer's requests never sit unread, and the loop keeps serving until
    /// EOF.
    #[tokio::test]
    async fn drive_interventions_degraded_mode_answers_each_request() {
        let (writer, mut rx) = OutboundWriter::test_channel();
        let handler = UnavailableInterventionHandler;
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":31,\"method\":\"intervene/injectMessage\",\"params\":{\"text\":\"x\"}}\n\
              {\"jsonrpc\":\"2.0\",\"id\":32,\"method\":\"intervene/cancel\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":33,\"method\":\"intervene/injectMessage\",\"params\":{\"text\":\"y\"}}\n";
        let mut reader = BufReader::new(&input[..]);
        drive_interventions(&mut reader, &writer, &handler, stop_rx)
            .await
            .unwrap();
        drop(writer);

        for expected_id in [31, 32, 33] {
            let frame = rx.recv().await.unwrap();
            let parsed: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(parsed["id"], json!(expected_id));
            assert_eq!(parsed["error"]["code"], json!(CODE_INTERNAL_ERROR));
            assert!(
                parsed["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("unavailable"),
                "the degraded reason must reach the peer: {parsed}"
            );
        }
        assert!(rx.recv().await.is_none());
    }

    /// The `stop` signal winds the reader down cleanly even with no input
    /// pending — the run-finished path.
    #[tokio::test]
    async fn drive_interventions_stops_on_signal() {
        let (writer, _rx) = OutboundWriter::test_channel();
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

    /// Single-serializing-writer serialization under concurrency: a BURST
    /// of event/* notifications streams WHILE an intervene/* ack is
    /// emitted, and every outbound line is a complete, parseable JSON-RPC
    /// frame — proving acks and notifications never interleave-corrupt.
    #[tokio::test]
    async fn intervene_ack_and_event_burst_never_interleave_corrupt() {
        let (writer, mut out_rx) = OutboundWriter::test_channel();

        // The live event emitter shares the ONE writer with the intervene
        // dispatch — the exact production topology.
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
