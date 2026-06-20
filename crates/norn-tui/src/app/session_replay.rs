//! Startup replay of persisted session events into the visible transcript.
//!
//! The provider prompt projection is rebuilt from [`EventStore`] inside the
//! agent loop. This module handles the separate TUI projection: when the app
//! starts with a non-empty store, render the persisted events back into the
//! scroll region so a resumed session does not appear visually empty.

use std::io::Write as _;

use termina::Terminal as _;
use termina::escape::csi::{Csi, Sgr};
use termina::style::Intensity;

use norn::session::store::EventStore;

use crate::TuiError;
use crate::agents::tabs::DEFAULT_REPLAY_COUNT;
use crate::render::scroll_region::write_to_scroll;
use crate::terminal::setup::TerminalGuard;

use super::state::AppState;

const STARTUP_REPLAY_COUNT: usize = DEFAULT_REPLAY_COUNT;

/// Replay the current session store into the scroll region.
///
/// This intentionally renders the append-only event log rather than the
/// provider prompt view. A live TUI cannot erase old terminal scrollback when
/// `/compact` records a summary, so resume should show the same audit-grade
/// transcript plus the compaction marker instead of pretending visible rows
/// disappeared. Startup only paints the recent replay window already used by
/// agent-tab switching, so large sessions become interactive without waiting
/// for the whole audit log to format.
pub(super) fn replay_visible_session_history(
    state: &AppState,
    store: &EventStore,
    guard: &mut TerminalGuard,
) -> Result<(), TuiError> {
    let events = store.last_events(STARTUP_REPLAY_COUNT);
    if events.is_empty() {
        return Ok(());
    }

    let total_events = store.len().max(events.len());
    if let Some(notice) = replay_window_notice(total_events, events.len()) {
        write_to_scroll(&notice, guard.terminal_mut())?;
        guard.note_scroll_newlines(&notice)?;
    }

    let terminal_width = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    for event in &events {
        let rendered = crate::events::render_event(
            event,
            &state.terminal_caps,
            state.display_toggles,
            terminal_width,
        );
        if rendered.is_empty() {
            continue;
        }
        write_to_scroll(&rendered, guard.terminal_mut())?;
        guard.note_scroll_newlines(&rendered)?;
    }
    guard.terminal_mut().flush()?;
    Ok(())
}

fn replay_window_notice(total_events: usize, rendered_events: usize) -> Option<String> {
    if total_events <= rendered_events {
        return None;
    }
    Some(format!(
        "{}showing last {rendered_events} of {total_events} session events{}\n",
        Csi::Sgr(Sgr::Intensity(Intensity::Dim)),
        Csi::Sgr(Sgr::Intensity(Intensity::Normal)),
    ))
}

#[cfg(test)]
mod tests {
    use super::replay_window_notice;

    #[test]
    fn replay_window_notice_is_absent_when_every_event_is_rendered() {
        assert!(replay_window_notice(20, 20).is_none());
    }

    #[test]
    fn replay_window_notice_reports_bounded_replay_window() {
        let notice = replay_window_notice(25, 20);

        assert!(
            notice
                .as_deref()
                .is_some_and(|text| text.contains("showing last 20 of 25 session events")),
            "unexpected notice: {notice:?}",
        );
    }
}
