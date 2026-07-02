//! Driven-mode (`--protocol jsonrpc`) run ownership: the duplex entry
//! point, the post-acceptance error funnel, and the mid-run intervene loop
//! wiring (`DRIVEN-PROTOCOL.md`).

use std::sync::Arc;

use uuid::Uuid;

use tokio_util::sync::CancellationToken;

use super::intervene::NornInterventionHandler;
use super::jsonrpc::{
    self, DrivenRun, InterventionHandler, PreRunOutcome, RunDriver, SharedRunDriver,
    TransportError, UnavailableInterventionHandler,
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
            if let Err(join_err) = writer_task.await {
                tracing::warn!("jsonrpc writer task did not exit cleanly: {join_err}");
            }
            return Err(PrintError::Io(err.to_string()));
        }
    };

    let (id, prompt) = match outcome {
        PreRunOutcome::Run { id, prompt } => (id, prompt),
        PreRunOutcome::Closed => {
            // The peer disconnected before requesting a run. Shut the
            // writer down cleanly and exit success — nothing to do.
            drop(writer);
            if let Err(join_err) = writer_task.await {
                tracing::warn!("jsonrpc writer task did not exit cleanly: {join_err}");
            }
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
    if let Err(join_err) = writer_task.await {
        tracing::warn!("jsonrpc writer task did not exit cleanly: {join_err}");
    }

    result
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

    let parts = assemble_print_agent(cli).await?;

    // The stdin reader that carried initialize/run/execute keeps being read
    // during the run so mid-run intervene/* requests reach the agent.
    let driven_run = DrivenRun {
        driver: Arc::clone(driver),
        reader,
    };

    orchestrate(cli, parts, prompt, output_schema, Some(driven_run)).await
}

/// The join handle + stop signal for the mid-run intervene reader task.
pub(super) type InterveneTask = tokio::task::JoinHandle<Result<(), TransportError>>;

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
/// task, logging (never swallowing) a join panic or a transport error it
/// surfaced. A `None` task (non-driven mode) is a no-op.
pub(super) async fn finish_intervene_loop(
    task: Option<InterveneTask>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
) {
    if let Some(stop) = stop {
        // A failed send means the reader already returned (EOF or an applied
        // cancel) — not an error; the join below still completes.
        let _ = stop.send(());
    }
    let Some(task) = task else { return };
    match task.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!("driven-mode intervene reader ended with a transport error: {err}");
        }
        Err(join_err) => {
            tracing::warn!("driven-mode intervene reader task did not exit cleanly: {join_err}");
        }
    }
}

/// Map a driven-mode event-emitter [`tokio::task::JoinError`] onto the
/// agent-error path: a panicked/cancelled emitter means the live `event/*`
/// transcript is torn, so the run must surface the failure rather than send
/// a clean terminal result over an incomplete stream.
pub(super) fn emitter_failure(err: &tokio::task::JoinError) -> PrintError {
    PrintError::Agent(format!(
        "jsonrpc event emitter task failed ({kind}): {err}; the live event stream is incomplete",
        kind = if err.is_panic() { "panic" } else { "cancelled" },
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// Regression for the degraded intervene mode: with NO resolvable
    /// `MessageRouter` on the registry (bare registry, no shared context),
    /// the intervene loop must STILL be spawned — reading stdin and
    /// answering each advertised intervention with -32603 — instead of
    /// being skipped while the peer's requests sit unread forever.
    #[tokio::test]
    async fn intervene_loop_runs_degraded_when_router_unresolvable() {
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
            let frame = out_rx.recv().await.expect("an answer per request");
            let parsed: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(parsed["id"], json!(expected_id));
            assert_eq!(parsed["error"]["code"], json!(-32603));
        }

        // EOF has ended the loop; the wind-down path joins it cleanly.
        finish_intervene_loop(task, stop).await;
        drop(driver);
        assert!(out_rx.recv().await.is_none(), "no further frames");
    }

    /// Non-driven mode spawns nothing: no token, no task, no stop.
    #[tokio::test]
    async fn intervene_loop_absent_outside_driven_mode() {
        let registry = Arc::new(norn::tool::registry::ToolRegistry::new());
        let root_cancel = CancellationToken::new();
        let (task, stop) = spawn_intervene_loop::<tokio::io::BufReader<tokio::io::Stdin>>(
            None,
            None,
            &registry,
            Uuid::new_v4(),
            &root_cancel,
        );
        assert!(task.is_none());
        assert!(stop.is_none());
    }

    /// An emitter `JoinError` (panic) must degrade to the agent-error exit
    /// path with the tear named — never a clean exit 0.
    #[tokio::test]
    async fn emitter_panic_maps_to_agent_error_exit() {
        let task = tokio::spawn(async {
            panic!("emitter blew up");
        });
        let join_err = task.await.expect_err("task must panic");
        let err = emitter_failure(&join_err);
        match &err {
            PrintError::Agent(message) => {
                assert!(message.contains("panic"), "message: {message}");
                assert!(message.contains("incomplete"), "message: {message}");
            }
            other => panic!("expected Agent, got {other:?}"),
        }
        assert_eq!(err.exit_code(), ExitCode::AgentError);
    }
}
