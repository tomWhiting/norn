//! Persistent driven duplex ownership and explicit transport failure mapping.

use super::error::{PrintError, preserve_primary_failure};
use super::jsonrpc::{self, EventEmitterError, TransportError};
use crate::cli::{Cli, ExitCode};

/// Run sequential requests with one input owner, output writer and runtime.
pub(super) async fn execute_driven(cli: &Cli) -> Result<ExitCode, PrintError> {
    let (writer, writer_task) = jsonrpc::spawn_writer();
    let input = jsonrpc::session::spawn_session_input(jsonrpc::stdin_reader(), writer.clone());
    let result = super::driven_session::run_session(cli, input, writer.clone()).await;
    drop(writer);
    preserve_primary_failure(result, finish_writer(writer_task).await)
}

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
