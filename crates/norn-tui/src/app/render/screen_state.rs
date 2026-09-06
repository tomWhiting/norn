//! Frontend-owned geometry, publication baselines and bounded current display caches.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use super::{changes, hit};
use crate::app::focus::{FocusAvailability, FocusState};
use crate::app::viewport::{ViewAnchor, Viewport};
use crate::render::frame::{Frame, PreparedFrame};
use crate::render::layout::{Layout, Rect, SplitPreference, UpperLayout, UpperPane};
use crate::render::retained_markdown::RenderedMarkdown;
use crate::render::retained_text::TextRow;
use norn::session_view::{BodyRef, ItemId, ViewSource};

/// Content selected for the auxiliary pane during this frontend session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum AuxiliaryPane {
    Diff,
    Agents,
}

/// Geometry/cache state owned by one frontend, independent from the running agent.
pub struct ScreenState {
    pub(in crate::app) viewport: Viewport,
    pub(in crate::app) focus: FocusState,
    pub(in crate::app) changes_open: bool,
    pub(in crate::app) auxiliary: AuxiliaryPane,
    pub(in crate::app) split: SplitPreference,
    pub(in crate::app) upper: UpperPane,
    pub(in crate::app) tool_overrides: HashMap<ItemId, bool>,
    pub(in crate::app) selection: Option<crate::app::selection::Selection>,
    pub(in crate::app) selection_item: Option<ItemId>,
    pub(in crate::app) display_frame: Option<Arc<Frame>>,
    pub(in crate::app) display_selection: Option<crate::app::display_selection::DisplaySelection>,
    pub(in crate::app) feedback: Option<String>,
    pub(in crate::app) request_copy: bool,
    pub(in crate::app) search: crate::app::view_actions::reading::SearchState,
    /// Explicit visible body requests to be scheduled by the event owner.
    pub demands: Vec<(ItemId, BodyRef)>,
    /// Most recently rendered logical rows for keyboard/mouse hit testing.
    pub(in crate::app) visible: Vec<ViewAnchor>,
    pub(in crate::app) hit_rows: Vec<hit::HitRow>,
    pub(in crate::app) dragging_selection: bool,
    pub(in crate::app) dragging_composer: bool,
    pub(in crate::app) dragging_divider: bool,
    pub(in crate::app) layout: Layout,
    pub(in crate::app) pane_switch: Option<Rect>,
    pub(in crate::app) composer_send_key_area: Option<Rect>,
    pub(in crate::app) prepared_latest: Option<Rect>,
    pub(in crate::app) latest_hit: Option<crate::app::view_actions::latest::LatestHit>,
    pub(in crate::app) navigation: Option<super::navigation::PendingNavigation>,
    pub(in crate::app) row_cursor: Option<super::navigation::RowCursor>,
    pub(in crate::app) changes_row: usize,
    pub(in crate::app) changes: changes::ChangesState,
    pub(in crate::app) request_older: bool,
    pub(in crate::app) request_more: bool,
    /// A semantic update permits new visible-body demand; resize alone does not.
    pub allow_body_load: bool,
    pub(super) displayed: HashMap<BodyRef, DisplayCache>,
    pub(super) highlighter: crate::render::syntax::SyntaxHighlighter,
    pub(super) last_frame: Option<PreparedFrame>,
    /// Input/navigation/body completion marks the next ready frame dirty.
    pub dirty: bool,
    pub(super) last_revision: Option<u64>,
    pub(super) last_indicator: Option<String>,
    pub(super) next_agent_refresh: Option<Instant>,
    pub(super) ready_batch_remaining: usize,
}

/// One approved original revision, parsed once and laid out once per current width.
pub(super) struct DisplayCache {
    pub(super) original_len: usize,
    pub(super) secondary_fields: bool,
    pub(super) text: Arc<RenderedMarkdown>,
    pub(super) columns: u16,
    pub(super) rows: Arc<[TextRow]>,
}

impl ScreenState {
    /// Bind frontend navigation to the actual session/store identity.
    pub fn new(source: ViewSource) -> Self {
        Self {
            viewport: Viewport::new(source, true),
            focus: FocusState::new(),
            changes_open: false,
            auxiliary: AuxiliaryPane::Diff,
            split: SplitPreference::default(),
            upper: UpperPane::Conversation,
            tool_overrides: HashMap::new(),
            selection: None,
            selection_item: None,
            display_frame: None,
            display_selection: None,
            feedback: None,
            request_copy: false,
            search: crate::app::view_actions::reading::SearchState::new(),
            demands: Vec::new(),
            visible: Vec::new(),
            hit_rows: Vec::new(),
            dragging_selection: false,
            dragging_composer: false,
            dragging_divider: false,
            layout: Layout::NoPaint,
            pane_switch: None,
            composer_send_key_area: None,
            prepared_latest: None,
            latest_hit: None,
            navigation: None,
            row_cursor: None,
            changes_row: 0,
            changes: changes::ChangesState::new(),
            request_older: false,
            request_more: false,
            allow_body_load: true,
            displayed: HashMap::new(),
            highlighter: crate::render::syntax::SyntaxHighlighter::new(),
            last_frame: None,
            dirty: true,
            last_revision: None,
            last_indicator: None,
            next_agent_refresh: None,
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
            self.display_frame = None;
            self.display_selection = None;
            self.feedback = None;
            self.request_copy = false;
            self.search = crate::app::view_actions::reading::SearchState::new();
            self.demands.clear();
            self.visible.clear();
            self.hit_rows.clear();
            self.dragging_selection = false;
            self.dragging_composer = false;
            self.dragging_divider = false;
            self.displayed.clear();
            self.last_frame = None;
            self.prepared_latest = None;
            self.latest_hit = None;
            self.navigation = None;
            self.row_cursor = None;
            self.changes_row = 0;
            self.changes.clear();
            self.request_older = false;
            self.request_more = false;
        }
        self.allow_body_load = true;
        self.dirty = true;
    }

    /// Visible focus regions are derived only from the last calculated rectangles.
    pub(in crate::app) fn availability(&self) -> FocusAvailability {
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
