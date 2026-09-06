//! Explicit follow intent, bounded accepted-history coverage and published Latest hit authority.

use std::sync::Arc;

use norn::session::store::{HistoryAnchor, HistoryDirection, HistoryPage, HistoryRead};
use norn::session_view::{HistoryPosition, ViewSource};
use uuid::Uuid;

use crate::TuiError;
use crate::app::render::{ScreenState, interaction};
use crate::app::state::AppState;
use crate::render::frame::Frame;
use crate::render::layout::Rect;

pub(in crate::app) const LABEL: &str = "↓ Latest";

#[derive(Debug, thiserror::Error)]
enum LatestError {
    #[error("latest history page belongs to {actual:?}, expected {expected:?}")]
    Source {
        expected: Box<ViewSource>,
        actual: Box<ViewSource>,
    },
    #[error("latest history cursor ordinal cannot represent its accepted event count")]
    Ordinal,
    #[error(
        "latest history read stopped before captured accepted event count {frontier}; covered {covered}"
    )]
    Coverage { frontier: usize, covered: usize },
}

struct Request {
    id: Uuid,
    source: ViewSource,
    frontier: Option<usize>,
}

/// One user intent and at most one existing supervised history job, not a new queue.
#[derive(Default)]
pub(in crate::app) struct LatestHistory {
    request: Option<Request>,
    in_flight: Option<Uuid>,
}

impl LatestHistory {
    pub(in crate::app) fn begin(&mut self, source: &ViewSource) {
        self.request = Some(Request {
            id: Uuid::new_v4(),
            source: source.clone(),
            frontier: None,
        });
    }

    pub(in crate::app) fn cancel(&mut self) {
        self.request = None;
    }

    pub(in crate::app) fn pending(&self) -> bool {
        self.request.is_some()
    }

    pub(in crate::app) fn start(&mut self) -> bool {
        if self.in_flight.is_some() {
            return false;
        }
        let Some(request) = self.request.as_ref() else {
            return false;
        };
        self.in_flight = Some(request.id);
        true
    }

    pub(in crate::app) fn failed(&mut self) {
        if let Some(id) = self.in_flight.take()
            && self
                .request
                .as_ref()
                .is_some_and(|request| request.id == id)
        {
            self.request = None;
        }
    }

    pub(in crate::app) fn observe(
        &mut self,
        read: &HistoryRead,
        page: &HistoryPage,
    ) -> Result<(), TuiError> {
        let Some(id) = self.in_flight.take() else {
            return Ok(());
        };
        let Some(intent) = self.request.as_mut().filter(|request| request.id == id) else {
            return Ok(());
        };
        if read.source != intent.source || page.source != intent.source {
            let expected = intent.source.clone();
            self.request = None;
            return Err(interaction(LatestError::Source {
                expected: Box::new(expected),
                actual: Box::new(page.source.clone()),
            }));
        }
        let frontier = *intent.frontier.get_or_insert(page.total_events);
        let covered = if let Some(last) = page.records.last() {
            position_count(last.cursor().position())?
        } else if read.direction == HistoryDirection::After {
            match &read.anchor {
                HistoryAnchor::At(cursor) => position_count(cursor.position())?,
                HistoryAnchor::Start => 0,
                HistoryAnchor::End => page.total_events,
            }
        } else if matches!(read.anchor, HistoryAnchor::End) && page.total_events == 0 {
            0
        } else {
            self.request = None;
            return Err(interaction(LatestError::Coverage {
                frontier,
                covered: 0,
            }));
        };
        if covered >= frontier {
            self.request = None;
        } else if page.records.is_empty() || !page.has_after {
            self.request = None;
            return Err(interaction(LatestError::Coverage { frontier, covered }));
        }
        Ok(())
    }
}

fn position_count(position: &HistoryPosition) -> Result<usize, TuiError> {
    match position {
        HistoryPosition::Empty => Ok(0),
        HistoryPosition::Event { ordinal, .. } => ordinal
            .checked_add(1)
            .ok_or_else(|| interaction(LatestError::Ordinal)),
    }
}

pub(in crate::app) fn follow_latest(state: &mut AppState) {
    state.screen.navigation = None;
    state.screen.row_cursor = None;
    state.screen.request_older = false;
    state.screen.viewport.follow_tail();
    state.transcript.request_latest();
    state.screen.dirty = true;
    state.screen.allow_body_load = true;
}

/// Hit regions belong to one successfully published surface, never prepared geometry alone.
pub(in crate::app) struct LatestHit {
    pub(in crate::app) area: Rect,
    source: ViewSource,
    frame: Arc<Frame>,
}

fn commit_hit(screen: &mut ScreenState, frame: &Arc<Frame>) {
    screen.latest_hit = screen.prepared_latest.take().map(|area| LatestHit {
        area,
        source: screen.viewport.source().clone(),
        frame: Arc::clone(frame),
    });
}

/// The caller passes the actual frame writer/flush result, after composer publication.
pub(in crate::app) fn finish_publication(
    screen: &mut ScreenState,
    frame: Arc<Frame>,
    publication: Result<(), TuiError>,
) -> Result<(), TuiError> {
    match publication {
        Ok(()) => {
            commit_hit(screen, &frame);
            screen.display_frame = Some(frame);
            Ok(())
        }
        Err(error) => {
            screen.prepared_latest = None;
            screen.latest_hit = None;
            screen.display_frame = None;
            Err(error)
        }
    }
}

pub(in crate::app) fn activate(state: &mut AppState, column: u16, row: u16) -> bool {
    let Some(hit) = state.screen.latest_hit.as_ref() else {
        return false;
    };
    if &hit.source != state.transcript.projection.source()
        || state
            .screen
            .display_frame
            .as_ref()
            .is_none_or(|frame| !Arc::ptr_eq(frame, &hit.frame))
    {
        state.screen.latest_hit = None;
        return false;
    }
    let area = hit.area;
    if column < area.column || column >= area.column.saturating_add(area.width) || row != area.row {
        return false;
    }
    follow_latest(state);
    true
}

#[cfg(test)]
#[path = "latest_tests.rs"]
mod tests;
