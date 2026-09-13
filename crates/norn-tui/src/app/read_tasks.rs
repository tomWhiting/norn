//! One frontend supervisor for explicitly demanded reads; jobs outlive conversation selection.

use norn::session::store::{HistoryPage, HistoryRead, SessionHistoryReader};
use norn::session_view::ViewSource;
use tokio::task::{JoinError, JoinSet};

use super::transcript::{BodyDemand, LoadedBody};
use crate::TuiError;

pub(in crate::app) type HistoryResult =
    Result<(HistoryRead, Result<HistoryPage, TuiError>), JoinError>;
pub(in crate::app) type BodyResult =
    Result<(ViewSource, BodyDemand, Result<LoadedBody, TuiError>), JoinError>;

/// The frontend alone owns these queues. Conversations retain demand identity, not tasks.
/// No read starts until bounded demand is explicitly submitted; completion wakes the event loop.
#[derive(Default)]
pub(in crate::app) struct ReadTasks {
    pub history: JoinSet<(HistoryRead, Result<HistoryPage, TuiError>)>,
    pub bodies: JoinSet<(ViewSource, BodyDemand, Result<LoadedBody, TuiError>)>,
}

impl ReadTasks {
    pub fn history(&mut self, reader: SessionHistoryReader, request: HistoryRead) {
        self.history.spawn(async move {
            let result = super::transcript::read_history(reader, request.clone()).await;
            (request, result)
        });
    }

    pub fn body(&mut self, reader: SessionHistoryReader, demand: BodyDemand) {
        let source = reader.source().clone();
        self.bodies.spawn(async move {
            let result =
                super::transcript::read_access::read_committed_body(reader, demand.clone())
                    .await
                    .map(|(_, page)| page);
            (source, demand, result)
        });
    }
}

/// Retired-source completion cannot dirty or alter the new conversation's selection/search.
pub(in crate::app) fn finish_body(
    state: &mut super::state::AppState,
    result: BodyResult,
) -> Result<(), TuiError> {
    if matches!(&result, Ok((source, _, _)) if source != state.transcript.projection.source()) {
        return Ok(());
    }
    state.transcript.finish_body(result)?;
    state.screen.allow_body_load = true;
    state.screen.dirty = true;
    Ok(())
}

#[cfg(test)]
#[path = "read_tasks_tests.rs"]
mod tests;
