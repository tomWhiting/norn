//! The pre-run request loop, the one-shot `run/execute` acceptance, and the
//! terminal-result driver (`DRIVEN-PROTOCOL.md` "One-shot run lifecycle").

use std::sync::Arc;

use serde_json::Value;
use tokio::io::AsyncBufReadExt;

use norn::provider::AgentEvent;

use super::capabilities::initialize_capabilities;
use super::emitter::{EventEmitterHandle, spawn_event_emitter};
use super::frames::{
    CODE_INTERNAL_ERROR, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND, JsonRpcResponse,
    METHOD_INITIALIZE, METHOD_RUN_EXECUTE, TransportError, read_request,
};
use super::stdin::StdinReader;
use super::writer::OutboundWriter;

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
/// pre-run method (including `intervene/*`, which is only served mid-run)
/// with `-32601`; and returns on the first `run/execute` with its id +
/// prompt. Parse / invalid frames are answered and skipped, never fatal.
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
        // Notifications (no id) are not served inbound; ignore.
        let Some(id) = request.id.clone() else {
            tracing::debug!(method = %request.method, "ignoring inbound notification in driven mode");
            continue;
        };
        match request.method.as_str() {
            METHOD_INITIALIZE => {
                writer.send_response(&JsonRpcResponse::ok(id, initialize_capabilities()))?;
            }
            METHOD_RUN_EXECUTE => match prompt_from_params(&request.params) {
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
/// driver plus the stdin reader the mid-run
/// [`drive_interventions`](super::interventions::drive_interventions) loop
/// keeps consuming.
///
/// The same stdin reader that carried `initialize`/`run/execute` keeps
/// being read for in-band `intervene/*` requests while the run is in
/// flight — so the reader rides along here instead of being dropped after
/// the pre-run loop.
pub struct DrivenRun {
    /// The shared result/event driver.
    pub driver: SharedRunDriver,
    /// The stdin JSON-RPC reader, moved in so the intervene loop owns it
    /// for the duration of the run.
    pub reader: StdinReader,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::frames::CODE_METHOD_NOT_FOUND;
    use super::*;
    use norn::provider::events::{ProviderEvent, StopReason};
    use norn::provider::usage::Usage;
    use norn::provider::{AgentEvent, AgentEventKind};
    use serde_json::json;
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
        let (writer, mut rx) = OutboundWriter::test_channel();
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
        let (writer, mut rx) = OutboundWriter::test_channel();
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
    async fn unknown_pre_run_method_gets_method_not_found() {
        let (writer, mut rx) = OutboundWriter::test_channel();
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

    /// The mandatory round-trip / negative control, composed over one
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
        let (writer, mut out_rx) = OutboundWriter::test_channel();

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
        let (writer, mut rx) = OutboundWriter::test_channel();
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
}
