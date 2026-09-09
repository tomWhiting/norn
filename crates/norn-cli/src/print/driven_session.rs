//! Persistent driven runtime ownership and per-request cancellation boundaries.

use std::sync::Arc;

use norn::agent::AgentParts;
use norn::agent_loop::config::ToolExecutor;
use tokio_util::sync::CancellationToken;

use super::assembly::{PrintAssembly, assemble_print_agent};
use super::error::{PrintError, preserve_primary_failure};
use super::intervene::NornInterventionHandler;
use super::jsonrpc::session::SessionInput;
use super::jsonrpc::{RunDriver, SharedRunDriver};
use super::orchestrator::{orchestrate, parse_output_schema};
use super::signals::SignalWatch;
use crate::cli::{Cli, ExitCode};

/// Keep one runtime and conversation alive until EOF, /exit or a fatal error.
pub(super) async fn run_session(
    cli: &Cli,
    mut input: SessionInput,
    writer: super::jsonrpc::OutboundWriter,
) -> Result<ExitCode, PrintError> {
    let signal_watch = match SignalWatch::install_with_activity(
        input.cancel.clone(),
        Arc::clone(&input.run_active),
    ) {
        Ok(watch) => watch,
        Err(error) => {
            input.control.shutdown().map_err(|transport| {
                PrintError::Io(format!("{error}; input shutdown failed: {transport}"))
            })?;
            input
                .task
                .await
                .map_err(|join| PrintError::Io(format!("{error}; input task failed: {join}")))?
                .map_err(|transport| {
                    PrintError::Io(format!("{error}; input failed: {transport}"))
                })?;
            return Err(PrintError::Agent(error.to_string()));
        }
    };
    let mut assembly = None;
    let mut session_cancel = None;
    let mut outcome = Ok(ExitCode::Success);
    while let Some(request) = input.runs.recv().await {
        let driver = Arc::new(RunDriver::for_session(
            writer.clone(),
            request.id,
            input.control.clone(),
            request.persistent,
        ));
        let result = execute_request(
            cli,
            &mut assembly,
            &mut session_cancel,
            &input.cancel,
            request.prompt,
            &driver,
        )
        .await;
        match result {
            Ok(code) => outcome = Ok(code),
            Err(error) => {
                let response = driver.finish_with_error(error.to_string()).await;
                outcome = Err(match response {
                    Ok(()) => error,
                    Err(transport) => {
                        PrintError::Io(format!("{error}; terminal response failed: {transport}"))
                    }
                });
                break;
            }
        }
        if assembly
            .as_ref()
            .and_then(|runtime| runtime.slash.as_ref())
            .is_some_and(|(state, _)| {
                state
                    .exit_requested
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
        {
            break;
        }
    }
    if let Some(cancel) = session_cancel {
        cancel.cancel();
    }
    input.cancel.cancel();
    if let Some(runtime) = assembly.as_ref()
        && runtime.session_started
    {
        runtime.parts.fire_session_end().await;
    }
    // EOF can already have finished the input owner. Its join result, rather
    // than an expected closed command queue, determines transport success.
    if !input.task.is_finished()
        && let Err(error) = input.control.shutdown()
    {
        tracing::debug!(%error, "driven input owner ended during shutdown");
    }
    let input_result = match input.task.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(PrintError::Io(format!("driven input failed: {error}"))),
        Err(error) => Err(PrintError::Io(format!("driven input task failed: {error}"))),
    };
    drop(signal_watch);
    preserve_primary_failure(outcome, input_result)
}

async fn execute_request(
    cli: &Cli,
    assembly: &mut Option<PrintAssembly>,
    session_cancel: &mut Option<CancellationToken>,
    process_cancel: &CancellationToken,
    prompt: String,
    driver: &SharedRunDriver,
) -> Result<ExitCode, PrintError> {
    let schema = if assembly.is_none() {
        parse_output_schema(cli.output_schema.as_deref())?
    } else {
        None
    };
    if assembly.is_none() {
        let assembled = assemble_print_agent(cli).await?;
        *session_cancel = Some(assembled.parts.cancel.clone());
        *assembly = Some(assembled);
    }
    let Some(runtime) = assembly.as_mut() else {
        return Err(PrintError::Agent(
            "persistent runtime missing after assembly".to_owned(),
        ));
    };
    runtime.parts.cancel = process_cancel.child_token();
    let shared = runtime.parts.registry.shared_context().ok_or_else(|| {
        PrintError::Agent("persistent runtime has no shared tool context".to_owned())
    })?;
    shared.insert_extension(Arc::new(norn::tools::agent::AgentCancellation(
        runtime.parts.cancel.clone(),
    )));
    orchestrate(cli, runtime, prompt, schema, Some(Arc::clone(driver))).await
}

/// Bind controls to the current request; old descendants retain old tokens.
pub(super) fn bind_controls(
    driver: &SharedRunDriver,
    parts: &AgentParts,
) -> Result<(), PrintError> {
    let shared = parts
        .registry
        .shared_context()
        .ok_or_else(|| PrintError::Agent("driven shared tool context missing".to_owned()))?;
    let infra = shared
        .get_extension::<norn::tools::agent::AgentToolInfra>()
        .ok_or_else(|| PrintError::Agent("driven agent message router missing".to_owned()))?;
    driver
        .bind(Arc::new(NornInterventionHandler::new(
            Arc::clone(&infra.router),
            parts.id,
            parts.cancel.clone(),
        )))
        .map_err(|error| PrintError::Io(error.to_string()))
}
