//! Sealed source-bound read access; acquisition and filesystem reads stay off the terminal executor.

use std::sync::Arc;

use norn::session::store::{EventStore, HistoryPage, HistoryRead, SessionHistoryReader};
use norn::session_view::ViewError;

use super::{BodyDemand, LoadedBody, Transcript};
use crate::TuiError;

impl Transcript {
    /// Install read authority only for this exact projection, without changing its history.
    pub(in crate::app) fn attach_history_reader(
        &mut self,
        reader: SessionHistoryReader,
    ) -> Result<(), TuiError> {
        if reader.source() != self.projection.source() {
            return Err(ViewError::SourceMismatch {
                expected: Box::new(self.projection.source().clone()),
                actual: Box::new(reader.source().clone()),
            }
            .into());
        }
        self.history_reader = Some(reader);
        Ok(())
    }

    /// Clone only a sealed capability, never events or writable store authority.
    pub(in crate::app) fn history_reader(&self) -> Result<SessionHistoryReader, TuiError> {
        self.history_reader.clone().ok_or_else(|| {
            crate::app::render::interaction(std::io::Error::other(format!(
                "history read access has not been attached for source {:?}",
                self.projection.source()
            )))
        })
    }
}

/// Store/spool identity validation can touch disk, so source acquisition is blocking work.
pub(in crate::app) async fn open_history_reader(
    store: Arc<EventStore>,
) -> Result<SessionHistoryReader, TuiError> {
    tokio::task::spawn_blocking(move || store.history_reader())
        .await
        .map_err(|source| TuiError::ViewTask {
            operation: "history access",
            source,
        })?
        .map_err(TuiError::from)
}

/// Run only explicit history work off the terminal executor.
pub(in crate::app) async fn read_history(
    reader: SessionHistoryReader,
    request: HistoryRead,
) -> Result<HistoryPage, TuiError> {
    tokio::task::spawn_blocking(move || reader.history_page(&request))
        .await
        .map_err(|source| TuiError::ViewTask {
            operation: "history page",
            source,
        })?
        .map_err(TuiError::from)
}

/// Run only an approved committed body demand off the terminal executor.
pub(in crate::app) async fn read_committed_body(
    reader: SessionHistoryReader,
    demand: BodyDemand,
) -> Result<(BodyDemand, LoadedBody), TuiError> {
    tokio::task::spawn_blocking(move || {
        let page = reader.read_body(&demand.read)?;
        Ok((demand, LoadedBody::from(page)))
    })
    .await
    .map_err(|source| TuiError::ViewTask {
        operation: "body range",
        source,
    })?
}

#[cfg(test)]
#[path = "read_access_tests.rs"]
mod tests;
