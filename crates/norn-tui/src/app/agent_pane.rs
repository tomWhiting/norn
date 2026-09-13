//! Agent row gestures belong to published frames; labels never resolve a recipient.

use std::sync::Arc;

use norn::session_view::ViewSource;
use termina::event::{MouseButton, MouseEvent, MouseEventKind};
use uuid::Uuid;

use crate::render::frame::Frame;
use crate::render::layout::Rect;

use super::render::{AuxiliaryPane, ScreenState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) struct AgentHit {
    pub id: Uuid,
    pub area: Rect,
}

struct Published {
    frame: Arc<Frame>,
    source: ViewSource,
    rows: Vec<AgentHit>,
}

struct Press {
    hit: AgentHit,
    source: ViewSource,
    column: u16,
    row: u16,
}

#[derive(Default)]
pub(in crate::app) struct AgentPane {
    pub prepared: Vec<AgentHit>,
    published: Option<Published>,
    press: Option<Press>,
}

impl AgentPane {
    pub fn revoke(&mut self) {
        self.prepared.clear();
        self.published = None;
        self.press = None;
    }
}

pub(in crate::app) fn published(screen: &mut ScreenState, frame: &Arc<Frame>) {
    screen.agent_pane.published = Some(Published {
        frame: Arc::clone(frame),
        source: screen.conversation.viewport.source().clone(),
        rows: std::mem::take(&mut screen.agent_pane.prepared),
    });
}

/// Observe alongside ordinary text selection. Only an unmoved left release activates.
pub(in crate::app) fn gesture(screen: &mut ScreenState, event: MouseEvent) -> Option<Uuid> {
    let Some(published) = screen.agent_pane.published.as_ref().filter(|published| {
        screen.auxiliary == AuxiliaryPane::Agents
            && screen.conversation.viewport.source() == &published.source
            && screen
                .display_frame
                .as_ref()
                .is_some_and(|frame| Arc::ptr_eq(frame, &published.frame))
    }) else {
        screen.agent_pane.press = None;
        return None;
    };
    let hit = published.rows.iter().copied().find(|hit| {
        event.column >= hit.area.column
            && event.column < hit.area.column.saturating_add(hit.area.width)
            && event.row == hit.area.row
    });
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if screen.dragging_selection {
                screen.agent_pane.press = None;
                return None;
            }
            screen.agent_pane.press = hit.map(|hit| Press {
                hit,
                source: published.source.clone(),
                column: event.column,
                row: event.row,
            });
        }
        MouseEventKind::Up(MouseButton::Left) => {
            return screen.agent_pane.press.take().and_then(|press| {
                (hit == Some(press.hit)
                    && press.source == published.source
                    && press.column == event.column
                    && press.row == event.row)
                    .then_some(press.hit.id)
            });
        }
        MouseEventKind::Moved => {
            if screen
                .agent_pane
                .press
                .as_ref()
                .is_some_and(|press| press.column != event.column || press.row != event.row)
            {
                screen.agent_pane.press = None;
            }
        }
        _ => screen.agent_pane.press = None,
    }
    None
}

#[cfg(test)]
#[path = "agent_pane_tests.rs"]
mod tests;
