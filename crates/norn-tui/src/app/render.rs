//! Retained-screen preparation and frame caching; terminal paint never reads history or bodies.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

use norn::session_view::BodyRef;

use crate::TuiError;
use crate::render::frame::{Frame, PaintRow};
use crate::render::layout::{Layout, LayoutPolicy, LayoutRequest, Rect, UpperLayout, UpperPane};
use crate::render::retained_markdown::{RenderedMarkdown, render_plain};
use crate::render::retained_text::{StyledText, TextLayout, TextRow};
use crate::terminal::setup::TerminalGuard;

use super::focus::Focus;
use super::state::AppState;
use super::viewport::{AnchorPosition, ViewAnchor};

mod agents;
pub(in crate::app) mod navigation;
mod screen_state;
pub(super) use screen_state::AuxiliaryPane;
use screen_state::DisplayCache;
pub use screen_state::ScreenState;
pub(in crate::app) mod changes;
mod composer;
pub(in crate::app) mod hit;
pub(in crate::app) mod transcript;
mod transcript_items;
use composer::popup;
use transcript::conversation;

/// Declared terminal tab display width; input bytes stay unchanged.
const DISPLAY_TAB_WIDTH: usize = 4;

/// Keep cell input and parent height synchronized without mutating the editor document.
pub(crate) fn sync_input_area(
    state: &mut AppState,
    cols: u16,
    terminal_rows: u16,
) -> Result<u16, TuiError> {
    let geometry_changed = match state.screen.layout {
        Layout::Ready { composer, .. } => {
            composer.width != cols || composer.row + composer.height != terminal_rows
        }
        Layout::NoPaint => cols != 0 && terminal_rows != 0,
        Layout::ResizeRequired { area } => area.width != cols || area.height != terminal_rows,
    };
    if geometry_changed {
        navigation::apply(state)?;
        state.screen.row_cursor = None;
    }
    super::display_selection::sync_geometry(&mut state.screen, cols, terminal_rows);
    if state.screen.display_frame.is_none() {
        state.screen.latest_hit = None;
        state.screen.prepared_latest = None;
    }
    let height = state
        .composer_geometry
        .measure(&state.input_editor, cols, terminal_rows)?;
    state.fixed_panel.set_input_area(height);
    if geometry_changed {
        // Input routing needs the new geometry even when this finite batch has not
        // published yet. Pointer/copy authority remains revoked until publication.
        state.screen.layout = Layout::calculate(
            LayoutRequest {
                columns: cols,
                rows: terminal_rows,
                requested_composer_rows: height,
                changes_open: state.screen.changes_open,
                split: state.screen.split,
                active_upper_pane: state.screen.upper,
            },
            LayoutPolicy::default(),
        )?;
    }
    Ok(height)
}

/// Retain a locally admitted human input; the committed record remains independently identified.
pub(crate) fn write_user_message(
    text: String,
    state: &mut AppState,
) -> Result<super::transcript::publication::SubmittedInput, TuiError> {
    let local = super::notices::input(state, "You · submitted", &text)?;
    state.screen.allow_body_load = true;
    Ok(super::transcript::publication::SubmittedInput { text, local })
}

/// Prepare cached data into a coherent full-screen frame, then publish it once.
pub fn redraw_all(state: &mut AppState, guard: &mut TerminalGuard) -> Result<(), TuiError> {
    if state.screen.ready_batch_remaining > 0 {
        return Ok(());
    }
    super::view_actions::flush_copy(state, guard)?;
    if state
        .screen
        .next_agent_refresh
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        state.screen.dirty = true;
    }
    let revision = state.transcript.projection.revision();
    let indicator = state
        .streaming_indicator
        .repaint_key(guard.terminal_columns());
    if !state.screen.dirty
        && state.screen.last_revision == Some(revision)
        && state.screen.last_indicator == indicator
    {
        return Ok(());
    }
    let mut frame = prepare(state, guard.terminal_columns(), guard.terminal_rows())?;
    super::display_selection::paint(&mut state.screen, &mut frame).map_err(interaction)?;
    let prepared = frame.prepare(&state.terminal_caps)?;
    let publication = super::helpers::sync_with_guard(
        &state.terminal_caps,
        guard,
        &mut state.screen.last_frame,
        prepared,
    );
    if publication.is_err() {
        state.screen.composer_send_key_area = None;
        state.screen.dragging_composer = false;
        state.screen.display_frame = None;
        state.screen.latest_hit = None;
        state.screen.prepared_latest = None;
        state.screen.dragging_selection = false;
    }
    let publication = state.composer_geometry.finish_publication(publication);
    super::view_actions::latest::finish_publication(
        &mut state.screen,
        Arc::new(frame),
        publication,
    )?;
    // Publication and flush must succeed before either baseline is advanced.
    state.screen.dirty = false;
    state.screen.last_revision = Some(revision);
    state.screen.last_indicator = indicator;
    Ok(())
}

/// A status timer changes retained state; an unchanged frame writes nothing.
pub fn redraw_streaming_tick(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    now: Instant,
) -> Result<(), TuiError> {
    state.tick(now);
    redraw_all(state, guard)
}

fn prepare(state: &mut AppState, columns: u16, rows: u16) -> Result<Frame, TuiError> {
    state.composer_geometry.begin_frame();
    let input_height = sync_input_area(state, columns, rows)?;
    let layout = Layout::calculate(
        LayoutRequest {
            columns,
            rows,
            requested_composer_rows: input_height,
            changes_open: state.screen.changes_open,
            split: state.screen.split,
            active_upper_pane: state.screen.upper,
        },
        LayoutPolicy::default(),
    )?;
    let agent_frame = agents::prepare(
        &mut state.agent_panel,
        layout,
        Instant::now(),
        chrono::Utc::now(),
    )?;
    let layout = agent_frame.layout;
    state.screen.next_agent_refresh =
        agent_frame.refresh_deadline(state.screen.auxiliary == AuxiliaryPane::Agents);
    state.screen.layout = layout;
    state.screen.pane_switch = None;
    state.screen.prepared_latest = None;
    state.screen.composer_send_key_area = None;
    state.screen.visible.clear();
    state.screen.hit_rows.clear();
    state.screen.demands.clear();
    let mut frame = Frame {
        layout,
        rows: Vec::new(),
        composer: None,
        cursor: None,
    };
    match layout {
        Layout::NoPaint => {}
        Layout::ResizeRequired { area } => {
            push_text(&mut frame, "Resize to continue", area, false, false)?;
        }
        Layout::Ready { upper, composer } => {
            match upper {
                UpperLayout::Single { pane, area } => {
                    let area = if state.screen.changes_open && area.height > 1 {
                        let switch = Rect { height: 1, ..area };
                        state.screen.pane_switch = Some(switch);
                        push_text(
                            &mut frame,
                            match (pane, state.screen.auxiliary) {
                                (UpperPane::Conversation, AuxiliaryPane::Diff) => {
                                    "Conversation  [F2 · Changes]"
                                }
                                (UpperPane::Conversation, AuxiliaryPane::Agents) => {
                                    "Conversation  [F2 · Agents]"
                                }
                                (UpperPane::Changes, AuxiliaryPane::Diff) => {
                                    "Changes  [F2 · Conversation]"
                                }
                                (UpperPane::Changes, AuxiliaryPane::Agents) => {
                                    "Agents  [F2 · Conversation]"
                                }
                            },
                            switch,
                            false,
                            false,
                        )?;
                        Rect {
                            row: area.row + 1,
                            height: area.height - 1,
                            ..area
                        }
                    } else {
                        area
                    };
                    match pane {
                        UpperPane::Conversation => conversation(state, &mut frame, area)?,
                        UpperPane::Changes => {
                            paint_auxiliary(state, &agent_frame, &mut frame, area)?;
                        }
                    }
                }
                UpperLayout::Split {
                    conversation: left,
                    changes: right,
                    ..
                } => {
                    conversation(state, &mut frame, left)?;
                    paint_auxiliary(state, &agent_frame, &mut frame, right)?;
                }
            }
            agents::paint(&agent_frame, &mut frame)?;
            composer::paint_chrome(state, &mut frame, composer)?;
            let input_area = crate::render::layout::composer_input_area(composer);
            let (cells, cursor) = state
                .composer_geometry
                .prepare(&state.input_editor, input_area)?;
            frame.composer = Some(cells);
            if state
                .screen
                .focus
                .visible(state.screen.availability())
                .map_err(interaction)?
                == Focus::Composer
            {
                frame.cursor = cursor;
            }
            popup(state, &mut frame, composer)?;
        }
    }
    Ok(frame)
}

fn paint_auxiliary(
    state: &mut AppState,
    agents: &agents::AgentFrame,
    frame: &mut Frame,
    area: Rect,
) -> Result<(), TuiError> {
    match state.screen.auxiliary {
        AuxiliaryPane::Diff => changes::paint(state, frame, area),
        AuxiliaryPane::Agents => agents::paint_pane(agents, frame, area, state.screen.changes_row),
    }
}

fn push_text(
    frame: &mut Frame,
    text: &str,
    area: Rect,
    selected: bool,
    composer: bool,
) -> Result<(), TuiError> {
    let text = safe_text(text)?;
    for (index, geometry) in layout_rows(&text.styled, area.width)?
        .into_iter()
        .take(usize::from(area.height))
        .enumerate()
    {
        frame.rows.push(PaintRow {
            area,
            row: u16::try_from(index).map_err(|source| TuiError::FrameCoordinate {
                value: index,
                source,
            })?,
            text: Arc::clone(&text),
            geometry,
            selected,
            selection: Vec::new(),
            composer,
        });
    }
    Ok(())
}

fn safe_text(text: &str) -> Result<Arc<RenderedMarkdown>, TuiError> {
    Ok(Arc::new(render_plain(text)?))
}

fn layout_rows(text: &StyledText, columns: u16) -> Result<Vec<TextRow>, TuiError> {
    Ok(
        match text.layout(
            usize::from(columns),
            NonZeroUsize::new(DISPLAY_TAB_WIDTH).ok_or(TuiError::InvalidViewDemand {
                name: "display tab width",
                value: DISPLAY_TAB_WIDTH,
            })?,
        )? {
            TextLayout::NoPaint => Vec::new(),
            TextLayout::Rows(rows) => rows,
        },
    )
}

pub(super) fn interaction(error: impl std::error::Error + Send + Sync + 'static) -> TuiError {
    TuiError::ViewInteraction {
        source: Box::new(error),
    }
}

/// Schedule only previously identified visible demands, never called by frame encoding.
pub(super) fn load_visible(
    state: &mut AppState,
    store: &Arc<norn::session::EventStore>,
) -> Result<(), TuiError> {
    // A deferred frame still describes the previous selection/geometry. Keep
    // every demand and permission pending until this finite input batch paints.
    if state.screen.ready_batch_remaining > 0 {
        return Ok(());
    }
    if !state.screen.viewport.follows_tail() {
        state.transcript.cancel_latest();
    }
    state.transcript.load_latest(store)?;
    if state.screen.request_older
        && (!state.transcript.has_older || state.transcript.load_older(store)?)
    {
        state.screen.request_older = false;
    }
    if std::mem::take(&mut state.screen.request_more)
        && let Some(item) = state
            .screen
            .viewport
            .selected()
            .and_then(|id| state.transcript.projection.item(id))
    {
        let id = item.id.clone();
        let bodies = item.bodies.clone();
        for body in bodies {
            state.transcript.load_body(store, &id, &body, true)?;
        }
    }
    if !state.screen.allow_body_load {
        return Ok(());
    }
    state.screen.allow_body_load = false;
    let mut demands = std::mem::take(&mut state.screen.demands);
    if state.screen.changes_open
        && state.screen.auxiliary == AuxiliaryPane::Diff
        && let Some(item) = state
            .screen
            .viewport
            .selected()
            .and_then(|id| state.transcript.projection.item(id))
    {
        demands.extend(
            item.bodies
                .iter()
                .cloned()
                .map(|body| (item.id.clone(), body)),
        );
    }
    let mut pinned: HashSet<BodyRef> = demands
        .iter()
        .map(|(_, reference)| reference.clone())
        .collect();
    if let Some(ViewAnchor {
        position: AnchorPosition::Body { reference, .. },
        ..
    }) = state.screen.viewport.anchor()
    {
        pinned.insert(reference.clone());
    }
    if let Some(selection) = &state.screen.selection {
        pinned.insert(selection.reference().clone());
    }
    super::view_actions::reading::load_requests(state, store, &mut pinned)?;
    let had_demands = !demands.is_empty();
    for (item, reference) in demands {
        state
            .transcript
            .load_body(store, &item, &reference, false)?;
    }
    state.screen.dirty |= had_demands;
    changes::demand(state);
    state.transcript.retain_bodies(&pinned);
    state
        .screen
        .displayed
        .retain(|reference, _| pinned.contains(reference));
    Ok(())
}
