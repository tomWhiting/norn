//! Persistent driven duplex ownership and explicit transport failure mapping.

use super::error::{PrintError, preserve_primary_failure};
use super::jsonrpc::{self, EventEmitterError, TransportError};
use crate::cli::{Cli, ExitCode};
use tokio_util::sync::CancellationToken;

/// Run sequential requests with one input owner, output writer and runtime.
pub(super) async fn execute_driven(cli: &Cli) -> Result<ExitCode, PrintError> {
    let (writer, writer_task) = jsonrpc::spawn_writer();
    let input = jsonrpc::session::spawn_session_input(jsonrpc::stdin_reader(), writer.clone());
    let cancel = input.cancel.clone();
    // The retained assembly makes this a large future; allocate it once for
    // the connection instead of growing each supervising task's stack.
    let session = Box::pin(super::driven_session::run_session(
        cli,
        input,
        writer.clone(),
    ));
    supervise_session(session, writer, writer_task, cancel).await
}

async fn supervise_session<F>(
    session: F,
    writer: jsonrpc::OutboundWriter,
    mut writer_task: tokio::task::JoinHandle<Result<(), TransportError>>,
    cancel: CancellationToken,
) -> Result<ExitCode, PrintError>
where
    F: std::future::Future<Output = Result<ExitCode, PrintError>>,
{
    tokio::pin!(session);
    tokio::select! {
        outcome = &mut session => {
            drop(writer);
            preserve_primary_failure(outcome, finish_writer(writer_task).await)
        }
        output = &mut writer_task => {
            // Enqueue success does not establish writer health. Wake idle
            // admission or cancel active work, then await the owned cleanup.
            cancel.cancel();
            drop(writer);
            preserve_primary_failure(session.await, writer_outcome(output))
        }
    }
}

async fn finish_writer(
    task: tokio::task::JoinHandle<Result<(), TransportError>>,
) -> Result<(), PrintError> {
    writer_outcome(task.await)
}

fn writer_outcome(
    outcome: Result<Result<(), TransportError>, tokio::task::JoinError>,
) -> Result<(), PrintError> {
    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(PrintError::Io(format!(
            "jsonrpc stdout writer failed: {error}"
        ))),
        Err(error) => Err(PrintError::Io(format!(
            "jsonrpc stdout writer task failed: {error}"
        ))),
    }
}

/// A failed emitter cannot be followed by a clean terminal run result.
pub(super) fn emitter_failure(error: &EventEmitterError) -> PrintError {
    match error {
        EventEmitterError::Transport(error) => {
            PrintError::Io(format!("{error}; the live event stream is incomplete"))
        }
        EventEmitterError::EventsLost { .. } => {
            PrintError::Agent(format!("{error}; the live event stream is incomplete"))
        }
        EventEmitterError::Task(error) => PrintError::Agent(format!(
            "jsonrpc event emitter task failed: {error}; the live event stream is incomplete"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_writer_wakes_an_idle_session_with_stdin_still_open()
    -> Result<(), Box<dyn std::error::Error>> {
        let (stdin, reader) = jsonrpc::stdin::StdinReader::test_channel();
        let (writer, mut outbound) = jsonrpc::OutboundWriter::test_channel();
        let input = jsonrpc::session::spawn_session_input(reader, writer.clone());
        let cancel = input.cancel.clone();
        let output = tokio::spawn(async move {
            let frame = outbound.recv().await.ok_or_else(|| {
                TransportError::Session("initialize response was not enqueued".to_owned())
            })?;
            let parsed: serde_json::Value = serde_json::from_str(&frame)?;
            assert_eq!(parsed["id"], "init");
            Err(TransportError::Io(std::io::ErrorKind::BrokenPipe.into()))
        });
        stdin.send(Ok(
            b"{\"jsonrpc\":\"2.0\",\"id\":\"init\",\"method\":\"initialize\"}\n".to_vec(),
        ))?;
        let session = async move {
            input
                .task
                .await
                .map_err(|error| PrintError::Io(error.to_string()))?
                .map_err(|error| PrintError::Io(error.to_string()))?;
            Ok(ExitCode::Success)
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            supervise_session(session, writer, output, cancel),
        )
        .await?;
        assert!(
            matches!(result, Err(PrintError::Io(ref error)) if error.contains("stdout writer failed"))
        );
        assert!(stdin.is_closed());
        Ok(())
    }

    #[tokio::test]
    async fn writer_failure_and_cancellation_are_not_clean_exits() {
        let failed =
            tokio::spawn(async { Err(TransportError::Io(std::io::ErrorKind::BrokenPipe.into())) });
        assert!(matches!(
            finish_writer(failed).await,
            Err(PrintError::Io(_))
        ));
        let cancelled = tokio::spawn(std::future::pending::<Result<(), TransportError>>());
        cancelled.abort();
        assert!(matches!(
            finish_writer(cancelled).await,
            Err(PrintError::Io(_))
        ));
    }

    #[test]
    fn event_loss_and_transport_failures_preserve_their_classes() {
        let lost = emitter_failure(&EventEmitterError::EventsLost { missed: 7 });
        assert!(matches!(lost, PrintError::Agent(_)));
        assert!(lost.to_string().contains("lost 7 events"));
        let transport = emitter_failure(&EventEmitterError::Transport(TransportError::Io(
            std::io::ErrorKind::BrokenPipe.into(),
        )));
        assert!(matches!(transport, PrintError::Io(_)));
    }
}
