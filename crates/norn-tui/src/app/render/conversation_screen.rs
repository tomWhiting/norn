//! Source-bound reading presentation; never owns a terminal frame, composer or runtime.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::hit;
use crate::app::viewport::{ViewAnchor, Viewport};
use crate::render::layout::Rect;
use crate::render::retained_markdown::RenderedMarkdown;
use crate::render::retained_text::TextRow;
use norn::session_view::{BodyRef, ItemId, ViewSource};

/// One conversation's retained reading position, selection and demanded display cache.
pub(in crate::app) struct ConversationScreen {
    pub(in crate::app) viewport: Viewport,
    pub(in crate::app) diagnostic_items: HashSet<ItemId>,
    pub(in crate::app) tool_overrides: HashMap<ItemId, bool>,
    pub(in crate::app) selection: Option<crate::app::selection::Selection>,
    pub(in crate::app) selection_item: Option<ItemId>,
    pub(in crate::app) reading_snapshot: Option<super::reading_snapshot::ReadingSnapshot>,
    pub(in crate::app) prepared_reading: Option<Rect>,
    pub(in crate::app) feedback: Option<String>,
    pub(in crate::app) request_copy: bool,
    pub(in crate::app) search: crate::app::view_actions::reading::SearchState,
    pub demands: Vec<(ItemId, BodyRef)>,
    pub(in crate::app) visible: Vec<ViewAnchor>,
    pub(in crate::app) hit_rows: Vec<hit::HitRow>,
    pub(in crate::app) navigation: Option<super::navigation::PendingNavigation>,
    pub(in crate::app) row_cursor: Option<super::navigation::RowCursor>,
    pub(in crate::app) request_older: bool,
    pub(in crate::app) request_more: bool,
    pub allow_body_load: bool,
    pub(super) displayed: HashMap<BodyRef, DisplayCache>,
    pub(super) highlighter: crate::render::syntax::SyntaxHighlighter,
}

/// One approved original revision, parsed once and laid out once per current width.
pub(super) struct DisplayCache {
    pub(super) original_len: usize,
    pub(super) secondary_fields: bool,
    pub(super) text: Arc<RenderedMarkdown>,
    pub(super) columns: u16,
    pub(super) rows: Arc<[TextRow]>,
}

impl ConversationScreen {
    pub fn new(source: ViewSource) -> Self {
        Self {
            viewport: Viewport::new(source, true),
            diagnostic_items: HashSet::new(),
            tool_overrides: HashMap::new(),
            selection: None,
            selection_item: None,
            reading_snapshot: None,
            prepared_reading: None,
            feedback: None,
            request_copy: false,
            search: crate::app::view_actions::reading::SearchState::new(),
            demands: Vec::new(),
            visible: Vec::new(),
            hit_rows: Vec::new(),
            navigation: None,
            row_cursor: None,
            request_older: false,
            request_more: false,
            allow_body_load: true,
            displayed: HashMap::new(),
            highlighter: crate::render::syntax::SyntaxHighlighter::new(),
        }
    }

    /// Source retirement cannot change physical terminal publication or global controls.
    pub fn replace_source(&mut self, source: &ViewSource) -> bool {
        if !self.viewport.replace_source(source.clone()) {
            return false;
        }
        self.viewport.follow_tail();
        self.tool_overrides.clear();
        self.selection = None;
        self.selection_item = None;
        self.reading_snapshot = None;
        self.prepared_reading = None;
        self.feedback = None;
        self.request_copy = false;
        self.search = crate::app::view_actions::reading::SearchState::new();
        self.demands.clear();
        self.visible.clear();
        self.hit_rows.clear();
        self.displayed.clear();
        self.navigation = None;
        self.row_cursor = None;
        self.request_older = false;
        self.request_more = false;
        self.diagnostic_items.clear();
        self.allow_body_load = true;
        true
    }
}
