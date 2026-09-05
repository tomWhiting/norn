//! Retained-screen preparation and frame caching; terminal paint never reads history or bodies.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

use norn::session_view::{BodyRef, DisplayText, ItemId, ViewSource};

use crate::TuiError;
use crate::input::editor::InputEditor;
use crate::render::frame::{Frame, PaintRow};
use crate::render::layout::{
    Layout, LayoutPolicy, LayoutRequest, Rect, SplitPreference, UpperLayout, UpperPane,
};
use crate::render::retained_markdown::{RenderedMarkdown, render_plain};
use crate::render::retained_text::{StyledText, TextLayout, TextRow};
use crate::terminal::setup::TerminalGuard;

use super::focus::{Focus, FocusAvailability, FocusState};
use super::state::AppState;
use super::viewport::{AnchorPosition, ViewAnchor, Viewport};

pub(in crate::app) mod changes;
mod composer;
pub(in crate::app) mod hit;
pub(in crate::app) mod transcript;
use composer::{paint_composer, popup};
use transcript::conversation;

/// Existing composer cap, shared with the pure rectangle owner.
pub(crate) const INPUT_AREA_MAX_ROWS: u16 = crate::render::layout::DEFAULT_MAX_COMPOSER_ROWS;
/// Declared terminal tab display width; input bytes stay unchanged.
const DISPLAY_TAB_WIDTH: usize = 4;

/// Geometry/cache state owned by one frontend, independent from the running agent.
pub struct ScreenState {
    pub(super) viewport: Viewport,
    pub(super) focus: FocusState,
    pub(super) changes_open: bool,
    pub(super) split: SplitPreference,
    pub(super) upper: UpperPane,
    pub(super) tool_overrides: HashMap<ItemId, bool>,
    pub(super) selection: Option<super::selection::Selection>,
    pub(super) selection_item: Option<ItemId>,
    pub(super) feedback: Option<String>,
    pub(super) request_copy: bool,
    pub(super) search: super::view_actions::reading::SearchState,
    /// Explicit visible body requests to be scheduled by the event owner.
    pub demands: Vec<(ItemId, BodyRef)>,
    /// Most recently rendered logical rows for keyboard/mouse hit testing.
    pub(super) visible: Vec<ViewAnchor>,
    pub(super) hit_rows: Vec<hit::HitRow>,
    pub(super) dragging_selection: bool,
    pub(super) dragging_divider: bool,
    pub(super) layout: Layout,
    pub(super) pane_switch: Option<Rect>,
    pub(super) navigation: Option<ScrollRequest>,
    pub(super) changes_row: usize,
    pub(in crate::app) changes: changes::ChangesState,
    pub(super) request_older: bool,
    pub(super) request_more: bool,
    /// A semantic update permits new visible-body demand; resize alone does not.
    pub allow_body_load: bool,
    displayed: HashMap<BodyRef, DisplayCache>,
    highlighter: crate::render::syntax::SyntaxHighlighter,
    last_frame: Vec<u8>,
    /// Input/navigation/body completion marks the next ready frame dirty.
    pub dirty: bool,
    last_revision: Option<u64>,
    last_indicator: Option<String>,
    ready_batch_remaining: usize,
}

/// An explicit logical-row movement, consumed before the next visible window is painted.
pub(super) struct ScrollRequest {
    pub backwards: bool,
    pub rows: usize,
}

/// One approved original revision, parsed once and laid out once per current width.
struct DisplayCache {
    original_len: usize,
    secondary_fields: bool,
    text: Arc<RenderedMarkdown>,
    columns: u16,
    rows: Arc<[TextRow]>,
}

impl ScreenState {
    /// Bind frontend navigation to the actual session/store identity.
    pub fn new(source: ViewSource) -> Self {
        Self {
            viewport: Viewport::new(source, true),
            focus: FocusState::new(),
            changes_open: false,
            split: SplitPreference::default(),
            upper: UpperPane::Conversation,
            tool_overrides: HashMap::new(),
            selection: None,
            selection_item: None,
            feedback: None,
            request_copy: false,
            search: super::view_actions::reading::SearchState::new(),
            demands: Vec::new(),
            visible: Vec::new(),
            hit_rows: Vec::new(),
            dragging_selection: false,
            dragging_divider: false,
            layout: Layout::NoPaint,
            pane_switch: None,
            navigation: None,
            changes_row: 0,
            changes: changes::ChangesState::new(),
            request_older: false,
            request_more: false,
            allow_body_load: true,
            displayed: HashMap::new(),
            highlighter: crate::render::syntax::SyntaxHighlighter::new(),
            last_frame: Vec::new(),
            dirty: true,
            last_revision: None,
            last_indicator: None,
            ready_batch_remaining: 0,
        }
    }

    /// Capture a finite frontier of already-ready terminal events. Later arrivals
    /// cannot extend this batch or postpone its completed frame indefinitely.
    pub(in crate::app) fn terminal_event(&mut self, already_queued: usize) {
        if self.ready_batch_remaining == 0 {
            self.ready_batch_remaining = already_queued;
        } else {
            self.ready_batch_remaining -= 1;
        }
    }

    /// Retire source-bound caches and anchors while preserving frontend preferences.
    pub fn replace_source(&mut self, source: &ViewSource) {
        if self.viewport.replace_source(source.clone()) {
            self.viewport.follow_tail();
            self.tool_overrides.clear();
            self.selection = None;
            self.selection_item = None;
            self.feedback = None;
            self.request_copy = false;
            self.search = super::view_actions::reading::SearchState::new();
            self.demands.clear();
            self.visible.clear();
            self.hit_rows.clear();
            self.dragging_selection = false;
            self.dragging_divider = false;
            self.displayed.clear();
            self.last_frame.clear();
            self.navigation = None;
            self.changes_row = 0;
            self.changes.clear();
            self.request_older = false;
            self.request_more = false;
        }
        self.allow_body_load = true;
        self.dirty = true;
    }

    /// Visible focus regions are derived only from the last calculated rectangles.
    pub(super) fn availability(&self) -> FocusAvailability {
        let mut availability = FocusAvailability {
            composer: false,
            conversation: false,
            changes: false,
            divider: false,
        };
        if let Layout::Ready { upper, .. } = self.layout {
            availability.composer = true;
            match upper {
                UpperLayout::Split { .. } => {
                    availability.conversation = true;
                    availability.changes = true;
                    availability.divider = true;
                }
                UpperLayout::Single { pane, .. } => match pane {
                    UpperPane::Conversation => availability.conversation = true,
                    UpperPane::Changes => availability.changes = true,
                },
            }
        }
        availability
    }
}

/// Legacy editor wrapping is used only for its editing viewport, never to clip paint.
#[must_use]
pub(crate) fn capped_input_height(editor: &InputEditor, cols: u16, terminal_rows: u16) -> u16 {
    let chrome = if terminal_rows >= 6 {
        crate::render::layout::COMPOSER_CHROME_ROWS
    } else {
        0
    };
    editor
        .visual_height(cols)
        .min(((terminal_rows - chrome) / 2).clamp(1, INPUT_AREA_MAX_ROWS))
        .max(1)
}

/// Keep editor navigation's internal viewport in step with its requested size.
pub(crate) fn sync_input_area(editor: &mut InputEditor, cols: u16, terminal_rows: u16) -> u16 {
    let height = capped_input_height(editor, cols, terminal_rows);
    editor.scroll_to_cursor(cols, height);
    height
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
    let frame = prepare(state, guard.terminal_columns(), guard.terminal_rows())?;
    let output = frame.encode(&state.terminal_caps)?;
    state.screen.dirty = false;
    state.screen.last_revision = Some(revision);
    state.screen.last_indicator = indicator;
    if output == state.screen.last_frame {
        return Ok(());
    }
    let caps = state.terminal_caps.clone();
    super::helpers::sync_with_guard(&caps, guard, |guard| {
        guard.terminal_mut().write_all(&output)?;
        guard.terminal_mut().flush()?;
        Ok(())
    })?;
    state.screen.last_frame = output;
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
    let prefix = 0;
    let original = state.input_editor.text();
    let cursor = original
        .char_indices()
        .nth(state.input_editor.cursor_char_index())
        .map_or(original.len(), |(offset, _)| offset);
    // The prefix is a separate rectangle: a leading combining mark cannot join its space.
    let draft = safe_text(&format!("{original} "))?;
    let draft_rows = layout_rows(&draft.styled, columns.saturating_sub(prefix))?;
    let layout = Layout::calculate(
        LayoutRequest {
            columns,
            rows,
            requested_composer_rows: u16::try_from(draft_rows.len()).unwrap_or(u16::MAX),
            changes_open: state.screen.changes_open,
            split: state.screen.split,
            active_upper_pane: state.screen.upper,
        },
        LayoutPolicy::default(),
    )?;
    state.screen.layout = layout;
    state.screen.pane_switch = None;
    state.screen.visible.clear();
    state.screen.hit_rows.clear();
    state.screen.demands.clear();
    let mut frame = Frame {
        layout,
        rows: Vec::new(),
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
                            match pane {
                                UpperPane::Conversation => "Conversation  [F2 · Changes]",
                                UpperPane::Changes => "Changes  [F2 · Conversation]",
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
                        UpperPane::Changes => changes::paint(state, &mut frame, area)?,
                    }
                }
                UpperLayout::Split {
                    conversation: left,
                    changes: right,
                    ..
                } => {
                    conversation(state, &mut frame, left)?;
                    changes::paint(state, &mut frame, right)?;
                }
            }
            composer::paint_chrome(state, &mut frame, composer)?;
            let input_area = crate::render::layout::composer_input_area(composer);
            paint_composer(
                state,
                &mut frame,
                input_area,
                prefix,
                &draft,
                &draft_rows,
                cursor,
            )?;
            popup(state, &mut frame, composer)?;
        }
    }
    Ok(frame)
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
    if std::mem::take(&mut state.screen.request_older) {
        state.transcript.load_older(store)?;
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
