//! Frontend-owned geometry, publication baselines and bounded current display caches.

use std::sync::Arc;
use std::time::Instant;

use super::changes;
use crate::app::focus::{FocusAvailability, FocusState};
use crate::render::frame::{Frame, PreparedFrame};
use crate::render::layout::{Layout, Rect, SplitPreference, UpperLayout, UpperPane};
use norn::session_view::ViewSource;

/// Content selected for the auxiliary pane during this frontend session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum AuxiliaryPane {
    Diff,
    Agents,
}

/// Geometry/cache state owned by one frontend, independent from the running agent.
pub struct ScreenState {
    pub(in crate::app) agent_pane: crate::app::agent_pane::AgentPane,
    pub(in crate::app) conversation: super::ConversationScreen,
    pub(in crate::app) focus: FocusState,
    pub(in crate::app) changes_open: bool,
    pub(in crate::app) auxiliary: AuxiliaryPane,
    pub(in crate::app) split: SplitPreference,
    pub(in crate::app) upper: UpperPane,
    pub(in crate::app) display_frame: Option<Arc<Frame>>,
    pub(in crate::app) display_selection: Option<crate::app::display_selection::DisplaySelection>,
    pub(in crate::app) dragging_selection: bool,
    pub(in crate::app) dragging_composer: bool,
    pub(in crate::app) dragging_divider: bool,
    pub(in crate::app) layout: Layout,
    pub(in crate::app) pane_switch: Option<Rect>,
    pub(in crate::app) composer_send_key_area: Option<Rect>,
    pub(in crate::app) prepared_recovery: Option<crate::app::composer_recovery::PreparedRecovery>,
    pub(in crate::app) recovery_hit: Option<crate::app::composer_recovery::RecoveryHit>,
    pub(in crate::app) prepared_latest: Option<Rect>,
    pub(in crate::app) latest_hit: Option<crate::app::view_actions::latest::LatestHit>,
    pub(in crate::app) changes_row: usize,
    pub(in crate::app) changes: changes::ChangesState,
    pub(super) last_frame: Option<PreparedFrame>,
    /// Input/navigation/body completion marks the next ready frame dirty.
    pub dirty: bool,
    pub(super) last_revision: Option<u64>,
    pub(super) last_indicator: Option<String>,
    pub(super) next_agent_refresh: Option<Instant>,
    pub(super) ready_batch_remaining: usize,
}

impl ScreenState {
    /// Bind frontend navigation to the actual session/store identity.
    pub fn new(source: ViewSource) -> Self {
        Self {
            agent_pane: crate::app::agent_pane::AgentPane::default(),
            conversation: super::ConversationScreen::new(source),
            focus: FocusState::new(),
            changes_open: false,
            auxiliary: AuxiliaryPane::Diff,
            split: SplitPreference::default(),
            upper: UpperPane::Conversation,
            display_frame: None,
            display_selection: None,
            dragging_selection: false,
            dragging_composer: false,
            dragging_divider: false,
            layout: Layout::NoPaint,
            pane_switch: None,
            composer_send_key_area: None,
            prepared_latest: None,
            prepared_recovery: None,
            recovery_hit: None,
            latest_hit: None,
            changes_row: 0,
            changes: changes::ChangesState::new(),
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
        if self.conversation.replace_source(source) {
            self.agent_pane.revoke();
            self.display_frame = None;
            self.display_selection = None;
            self.dragging_selection = false;
            self.dragging_composer = false;
            self.dragging_divider = false;
            self.last_frame = None;
            self.prepared_latest = None;
            self.latest_hit = None;
            self.prepared_recovery = None;
            self.recovery_hit = None;
            self.changes_row = 0;
            self.changes.clear();
        }
        self.conversation.allow_body_load = true;
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
