//! Startup replay of persisted session events into the visible transcript.
//!
//! The provider prompt projection is rebuilt from [`EventStore`] inside the
//! agent loop. This module handles the separate TUI projection: when the app
//! starts with a non-empty store, render the persisted events back into the
//! scroll region so a resumed session does not appear visually empty.

use std::io::Write as _;

use termina::Terminal as _;

use norn::session::store::EventStore;

use crate::TuiError;
use crate::render::scroll_region::write_to_scroll;
use crate::terminal::setup::TerminalGuard;

use super::state::AppState;

/// Replay the current session store into the scroll region.
///
/// This intentionally renders the append-only event log rather than the
/// provider prompt view. A live TUI cannot erase old terminal scrollback when
/// `/compact` records a summary, so resume should show the same audit-grade
/// transcript plus the compaction marker instead of pretending older visible
/// rows disappeared.
pub(super) fn replay_visible_session_history(
    state: &AppState,
    store: &EventStore,
    guard: &mut TerminalGuard,
) -> Result<(), TuiError> {
    let events = store.events();
    if events.is_empty() {
        return Ok(());
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
