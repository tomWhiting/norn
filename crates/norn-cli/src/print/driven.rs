//! Driven-mode (`--protocol jsonrpc`) run ownership: the duplex entry
//! point, the post-acceptance error funnel, and the mid-run intervene loop
//! wiring (`DRIVEN-PROTOCOL.md`).

use std::sync::Arc;

use uuid::Uuid;

use tokio_util::sync::CancellationToken;

use super::intervene::NornInterventionHandler;
use super::jsonrpc::{
    self, DrivenRun, EventEmitterError, InterventionHandler, PreRunOutcome, RunDriver,
    SharedRunDriver, TransportError, UnavailableInterventionHandler,
};
use super::orchestrator::{PrintError, assemble_print_agent, orchestrate, parse_output_schema};
use crate::cli::{Cli, ExitCode};

/// Driven-mode entry: own the stdin+stdout JSON-RPC 2.0 duplex.
///
/// Spawns the single serializing stdout writer, answers the `initialize`
/// handshake, and waits for one `run/execute` request. The prompt is
/// sourced from that request's params — stdin is the JSON-RPC channel, so
/// it is never read as a prompt. The agent then runs with a live `event/*`
/// notification emitter (in place of the stream renderer), and the final
/// result is returned as the id-matched `run/execute` response.
pub(super) async fn execute_driven(cli: &Cli) -> Result<ExitCode, PrintError> {
    let (writer, writer_task) = jsonrpc::spawn_writer();
    let mut reader = jsonrpc::stdin_reader();

    let outcome = match jsonrpc::drive_pre_run(&mut reader, &writer).await {
        Ok(outcome) => outcome,
        Err(err) => {
            // Transport failure before a run was accepted: there is no id
            // to answer, but any frames already queued (e.g. parse-error
            // responses) still get flushed before the process exits.
            drop(writer);
            finish_writer(writer_task).await?;
            return Err(PrintError::Io(err.to_string()));
        }
    };

    let (id, prompt) = match outcome {
        PreRunOutcome::Run { id, prompt } => (id, prompt),
        PreRunOutcome::Closed => {
            // The peer disconnected before requesting a run. Shut the
            // writer down cleanly and exit success — nothing to do.
            drop(writer);
            finish_writer(writer_task).await?;
            return Ok(ExitCode::Success);
        }
    };

    let driver: SharedRunDriver = Arc::new(RunDriver::new(writer, id));

    // EVERY failure after the run/execute request is accepted — schema
    // parse, runtime assembly, provider auth, the run itself — funnels
    // through run_accepted's Result and is answered as the id-matched
    // error response below, so the peer is never left with EOF in place
    // of a Response.
    let result = run_accepted(cli, prompt, reader, &driver).await;

    if let Err(ref err) = result
        && let Err(send_err) = driver.finish_with_error(err.to_string())
    {
        tracing::warn!("failed to send run/execute error response: {send_err}");
    }

    // Drop every writer handle so the single serializing writer task drains
    // its queue and exits; then join it so all frames are flushed to stdout
    // before the process exits (terminal-response shutdown handshake).
    drop(driver);
    finish_writer(writer_task).await?;

    result
}

/// Join the sole stdout writer and preserve both sink and task failures.
/// A failed writer means the JSON-RPC response stream is incomplete, so it
/// overrides an otherwise clean run result and maps to the existing I/O
/// failure exit class.
async fn finish_writer(
    task: tokio::task::JoinHandle<Result<(), TransportError>>,
) -> Result<(), PrintError> {
    match task.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(PrintError::Io(format!(
            "jsonrpc stdout writer failed: {error}"
        ))),
        Err(error) => Err(PrintError::Io(format!(
            "jsonrpc stdout writer task failed: {error}"
        ))),
    }
}

/// Everything that happens after a `run/execute` request has been accepted:
/// output-schema parsing, runtime assembly, and the orchestrated run. Kept
/// as one fallible unit so [`execute_driven`] can answer ANY failure as the
/// id-matched error response.
async fn run_accepted(
    cli: &Cli,
    prompt: String,
    reader: jsonrpc::StdinReader,
    driver: &SharedRunDriver,
) -> Result<ExitCode, PrintError> {
    let output_schema = parse_output_schema(cli.output_schema.as_deref())?;

    let assembly = assemble_print_agent(cli).await?;

    // The stdin reader that carried initialize/run/execute keeps being read
    // during the run so mid-run intervene/* requests reach the agent.
    let driven_run = DrivenRun {
        driver: Arc::clone(driver),
        reader,
    };

    orchestrate(cli, assembly, prompt, output_schema, Some(driven_run)).await
}

/// The join handle + stop signal for the mid-run intervene reader task.
pub(super) type InterveneTask = tokio::task::JoinHandle<Result<(), TransportError>>;

/// The accepted run's intervention channel ended without a clean join.
#[derive(Debug, thiserror::Error)]
pub(super) enum InterveneLoopError {
    /// Stdin framing, reading, or response transport failed.
    #[error("driven-mode intervention transport failed: {0}")]
    Transport(#[from] TransportError),
    /// The reader task panicked or was cancelled.
    #[error("driven-mode intervention reader task failed: {0}")]
    Task(#[source] tokio::task::JoinError),
}

/// Spawn the driven-mode `intervene/*` reader loop for the duration of the
/// run.
///
/// Returns the reader task's join handle and the `stop` sender used to
/// wind it down when the run finishes on its own. In non-driven mode both
/// are `None`: no reader task is spawned. The step is always driven by the
/// builder's root cancellation token (`root_cancel`), so an
/// `intervene/cancel` stops the run and cascades to spawned descendants
/// through the published `AgentCancellation`.
///
/// The reader is fed the Norn control adapter ([`NornInterventionHandler`])
/// built from the harness `MessageRouter` (looked up on the shared tool
/// context installed by the builder) and the root cancellation token, so
/// injects reach the root agent and a cancel trips the same token the step
/// observes. If the router cannot be resolved — an assembly invariant that
/// should never fail on the driven path — the loop STILL runs, degraded:
/// stdin keeps being read and every `intervene/*` request is answered with
/// the internal error (`-32603`) carrying the unavailability reason, so
/// peer requests never sit unread until EOF (`DRIVEN-PROTOCOL.md`
/// "Degraded intervention mode"). The condition is error-logged, never
/// silently dropped; `intervene/cancel` is then answered -32603 and the
/// step's own token is never tripped by the reader.
pub(super) fn spawn_intervene_loop<R>(
    run_driver: Option<&SharedRunDriver>,
    reader: Option<R>,
    registry: &Arc<norn::tool::registry::ToolRegistry>,
    root_id: Uuid,
    root_cancel: &CancellationToken,
) -> (
    Option<InterveneTask>,
    Option<tokio::sync::oneshot::Sender<()>>,
)
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    let (Some(run_driver), Some(mut reader)) = (run_driver, reader) else {
        return (None, None);
    };
    let handler: Box<dyn InterventionHandler> = if let Some(router) = resolve_router(registry) {
        Box::new(NornInterventionHandler::new(
            router,
            root_id,
            root_cancel.clone(),
        ))
    } else {
        tracing::error!(
            "driven mode: the harness MessageRouter is unavailable; \
             intervene/* requests will be answered -32603 this run",
        );
        Box::new(UnavailableInterventionHandler)
    };
    let writer = run_driver.writer();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        jsonrpc::drive_interventions(&mut reader, &writer, handler.as_ref(), stop_rx).await
    });
    (Some(task), Some(stop_tx))
}

/// Look up the harness `MessageRouter` on the registry's shared tool
/// context (published by `install_agent_tool_infra` as the
/// `AgentToolInfra` extension). `None` only when the shared context or the
/// infra extension is absent — never on the assembled driven path.
fn resolve_router(
    registry: &Arc<norn::tool::registry::ToolRegistry>,
) -> Option<Arc<norn::agent::message_router::MessageRouter>> {
    use norn::agent_loop::runner::ToolExecutor;
    let shared = registry.shared_context()?;
    let infra = shared.get_extension::<norn::tools::agent::AgentToolInfra>()?;
    Some(Arc::clone(&infra.router))
}

/// Wind down the intervene reader after the run: signal `stop` and join the
/// task. A `None` task (non-driven mode) is a no-op.
///
/// # Errors
///
/// Returns a typed failure when stdin or an intervention response tears, or
/// when the reader task panics or is cancelled. An accepted `run/execute`
/// must not report success after its concurrent control channel failed.
pub(super) async fn finish_intervene_loop(
    task: Option<InterveneTask>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<(), InterveneLoopError> {
    if let Some(stop) = stop {
        // A failed send means the reader already returned (EOF or an applied
        // cancel) — not an error; the join below still completes.
        let _ = stop.send(());
    }
    let Some(task) = task else { return Ok(()) };
    match task.await {
        Ok(result) => result.map_err(InterveneLoopError::Transport),
        Err(error) => Err(InterveneLoopError::Task(error)),
    }
}

/// Map intervention-channel failure onto the same nonzero classes used by
/// the other driven transport tasks.
pub(super) fn intervene_failure(error: &InterveneLoopError) -> PrintError {
    match error {
        InterveneLoopError::Transport(_) => PrintError::Io(format!(
            "{error}; the accepted run's intervention channel is incomplete"
        )),
        InterveneLoopError::Task(_) => PrintError::Agent(format!(
            "{error}; the accepted run's intervention channel is incomplete"
        )),
    }
}

/// Select one terminal failure after both driven background tasks have been
/// joined. When both channels fail, preserve the intervention failure's class
/// while retaining the emitter failure in the rendered diagnostic.
pub(super) fn driven_background_failure(
    intervene: Option<&InterveneLoopError>,
    emitter: Option<&EventEmitterError>,
) -> Option<PrintError> {
    match (intervene, emitter) {
        (Some(intervene @ InterveneLoopError::Transport(_)), Some(emitter)) => {
            Some(PrintError::Io(format!(
                "{intervene}; the accepted run's intervention channel is incomplete; \
                 additionally, {}",
                emitter_failure(emitter)
            )))
        }
        (Some(intervene @ InterveneLoopError::Task(_)), Some(emitter)) => {
            Some(PrintError::Agent(format!(
                "{intervene}; the accepted run's intervention channel is incomplete; \
                 additionally, {}",
                emitter_failure(emitter)
            )))
        }
        (Some(intervene), None) => Some(intervene_failure(intervene)),
        (None, Some(emitter)) => Some(emitter_failure(emitter)),
        (None, None) => None,
    }
}

/// Map a driven-mode event-emitter failure onto the existing print error
/// classes. Transport failures remain I/O errors; lag and task failure are
/// agent errors. Every variant exits nonzero because the live transcript is
/// incomplete and must never be followed by a clean terminal result.
pub(super) fn emitter_failure(error: &EventEmitterError) -> PrintError {
    match error {
        EventEmitterError::Transport(error) => {
            PrintError::Io(format!("{error}; the live event stream is incomplete"))
        }
        EventEmitterError::EventsLost { .. } => {
            PrintError::Agent(format!("{error}; the live event stream is incomplete"))
        }
        EventEmitterError::Task(join_error) => PrintError::Agent(format!(
            "jsonrpc event emitter task failed ({kind}): {join_error}; \
             the live event stream is incomplete",
            kind = if join_error.is_panic() {
                "panic"
            } else {
                "cancelled"
            },
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// Regression for the degraded intervene mode: with NO resolvable
    /// `MessageRouter` on the registry (bare registry, no shared context),
    /// the intervene loop must STILL be spawned — reading stdin and
    /// answering each advertised intervention with -32603 — instead of
    /// being skipped while the peer's requests sit unread forever.
    #[tokio::test]
    async fn intervene_loop_runs_degraded_when_router_unresolvable()
    -> Result<(), Box<dyn std::error::Error>> {
        let (writer, mut out_rx) = super::super::jsonrpc::writer::OutboundWriter::test_channel();
        let driver: SharedRunDriver = Arc::new(RunDriver::new(writer, json!("run-1")));
        let registry = Arc::new(norn::tool::registry::ToolRegistry::new());

        let input = b"{\"jsonrpc\":\"2.0\",\"id\":51,\"method\":\"intervene/injectMessage\",\"params\":{\"text\":\"hi\"}}\n\
              {\"jsonrpc\":\"2.0\",\"id\":52,\"method\":\"intervene/cancel\"}\n";
        let reader = tokio::io::BufReader::new(&input[..]);

        let root_cancel = CancellationToken::new();
        let (task, stop) = spawn_intervene_loop(
            Some(&driver),
            Some(reader),
            &registry,
            Uuid::new_v4(),
            &root_cancel,
        );
        assert!(task.is_some(), "the reader loop MUST run in degraded mode");

        // Observe both answers BEFORE winding down: the loop's select is
        // biased toward the stop signal, so signalling first would race the
        // reads this test exists to prove.
        for expected_id in [51, 52] {
            let frame = out_rx
                .recv()
                .await
                .ok_or_else(|| std::io::Error::other("missing intervention response"))?;
            let parsed: Value = serde_json::from_str(&frame)?;
            assert_eq!(parsed["id"], json!(expected_id));
            assert_eq!(parsed["error"]["code"], json!(-32603));
        }

        // EOF has ended the loop; the wind-down path joins it cleanly.
        finish_intervene_loop(task, stop).await?;
        drop(driver);
        assert!(out_rx.recv().await.is_none(), "no further frames");
        Ok(())
    }

    /// Non-driven mode spawns nothing: no token, no task, no stop.
    #[tokio::test]
    async fn intervene_loop_absent_outside_driven_mode() {
        let registry = Arc::new(norn::tool::registry::ToolRegistry::new());
        let root_cancel = CancellationToken::new();
        let (task, stop) = spawn_intervene_loop::<super::jsonrpc::StdinReader>(
            None,
            None,
            &registry,
            Uuid::new_v4(),
            &root_cancel,
        );
        assert!(task.is_none());
        assert!(stop.is_none());
    }

    /// A cancelled emitter must degrade to the agent-error exit path with
    /// the tear named — never a clean exit 0.
    #[tokio::test]
    async fn emitter_cancellation_maps_to_agent_error_exit() {
        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        let join_result = task.await;
        assert!(
            join_result.is_err(),
            "aborted task must not complete cleanly"
        );
        let Err(join_error) = join_result else {
            return;
        };
        let failure = EventEmitterError::Task(join_error);
        let err = emitter_failure(&failure);
        assert!(matches!(&err, PrintError::Agent(_)), "error: {err:?}");
        if let PrintError::Agent(message) = &err {
            assert!(message.contains("cancelled"), "message: {message}");
            assert!(message.contains("incomplete"), "message: {message}");
        }
        assert_eq!(err.exit_code(), ExitCode::AgentError);
    }

    #[tokio::test]
    async fn writer_transport_failure_maps_to_io_error_exit() {
        let task =
            tokio::spawn(async { Err(TransportError::Io(std::io::ErrorKind::BrokenPipe.into())) });
        let outcome = finish_writer(task).await;
        assert!(outcome.is_err(), "broken stdout must fail driven execution");
        let Err(error) = outcome else { return };
        assert!(matches!(&error, PrintError::Io(_)));
        assert_eq!(error.exit_code(), ExitCode::AgentError);
    }

    #[tokio::test]
    async fn writer_task_cancellation_maps_to_io_error_exit() {
        let task = tokio::spawn(std::future::pending::<Result<(), TransportError>>());
        task.abort();
        let outcome = finish_writer(task).await;
        assert!(
            outcome.is_err(),
            "cancelled stdout writer must fail driven execution"
        );
        let Err(error) = outcome else { return };
        assert!(matches!(&error, PrintError::Io(_)));
        assert!(error.to_string().contains("writer task failed"));
        assert_eq!(error.exit_code(), ExitCode::AgentError);
    }

    #[test]
    fn emitter_transport_failure_maps_to_io_error_exit() {
        let failure =
            EventEmitterError::Transport(TransportError::Io(std::io::ErrorKind::BrokenPipe.into()));
        let error = emitter_failure(&failure);
        assert!(matches!(&error, PrintError::Io(_)));
        assert_eq!(error.exit_code(), ExitCode::AgentError);
    }

    #[test]
    fn emitter_event_loss_maps_to_agent_error_exit() {
        let failure = EventEmitterError::EventsLost { missed: 3 };
        let error = emitter_failure(&failure);
        assert!(matches!(&error, PrintError::Agent(_)));
        assert!(error.to_string().contains("lost 3 events"));
        assert_eq!(error.exit_code(), ExitCode::AgentError);
    }

    #[tokio::test]
    async fn intervention_transport_failure_is_not_swallowed() {
        let task =
            tokio::spawn(async { Err(TransportError::Io(std::io::ErrorKind::BrokenPipe.into())) });
        let outcome = finish_intervene_loop(Some(task), None).await;
        assert!(
            matches!(outcome, Err(InterveneLoopError::Transport(_))),
            "outcome: {outcome:?}"
        );
        let Err(error) = outcome else { return };
        let mapped = intervene_failure(&error);
        assert!(matches!(&mapped, PrintError::Io(_)));
        assert!(mapped.to_string().contains("incomplete"));
        assert_eq!(mapped.exit_code(), ExitCode::AgentError);
    }

    #[tokio::test]
    async fn intervention_task_cancellation_is_not_swallowed() {
        let task = tokio::spawn(std::future::pending::<Result<(), TransportError>>());
        task.abort();
        let outcome = finish_intervene_loop(Some(task), None).await;
        assert!(
            matches!(outcome, Err(InterveneLoopError::Task(_))),
            "outcome: {outcome:?}"
        );
        let Err(error) = outcome else { return };
        let mapped = intervene_failure(&error);
        assert!(matches!(&mapped, PrintError::Agent(_)));
        assert!(mapped.to_string().contains("incomplete"));
        assert_eq!(mapped.exit_code(), ExitCode::AgentError);
    }

    #[test]
    fn dual_background_failure_preserves_both_diagnostics() {
        let intervene = InterveneLoopError::Transport(TransportError::Io(std::io::Error::other(
            "stdin control torn",
        )));
        let emitter = EventEmitterError::EventsLost { missed: 7 };
        let selected = driven_background_failure(Some(&intervene), Some(&emitter));
        assert!(matches!(&selected, Some(PrintError::Io(_))));
        let Some(error) = selected else { return };
        let rendered = error.to_string();
        assert!(rendered.contains("stdin control torn"), "error: {rendered}");
        assert!(rendered.contains("lost 7 events"), "error: {rendered}");
        assert_eq!(error.exit_code(), ExitCode::AgentError);
    }
}
