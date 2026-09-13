//! Explicit recovery exchanges complete draft owners; no admission, resend or second recall store.

use std::collections::VecDeque;
use std::sync::Arc;

use norn::session_view::ViewSource;
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

use super::render::ScreenState;
use super::state::AppState;
use crate::input::DetachedComposerDraft;
use crate::render::frame::Frame;
use crate::render::layout::Rect;

struct SavedDraft {
    id: Uuid,
    draft: DetachedComposerDraft,
    rejected: bool,
}

/// Only explicit failed submissions or user-displaced drafts allocate an entry.
#[derive(Default)]
pub(super) struct ComposerRecovery {
    drafts: VecDeque<SavedDraft>,
}

impl ComposerRecovery {
    pub(super) fn retain_rejected(&mut self, draft: DetachedComposerDraft) {
        self.drafts.push_back(SavedDraft {
            id: Uuid::new_v4(),
            draft,
            rejected: true,
        });
    }
}

/// Geometry carries the saved owner identity before and after publication.
pub(super) struct PreparedRecovery {
    area: Rect,
    id: Uuid,
}

/// A recovery click is valid only on the successfully flushed, still-current frame.
pub(super) struct RecoveryHit {
    prepared: PreparedRecovery,
    source: ViewSource,
    frame: Arc<Frame>,
}

pub(super) fn prepare(
    state: &mut AppState,
    area: Rect,
) -> Result<Option<(String, Rect)>, crate::TuiError> {
    let Some(saved) = state.composer_recovery.drafts.front() else {
        return Ok(None);
    };
    let action = if saved.rejected {
        "Recover rejected message"
    } else {
        "Switch saved draft"
    };
    let label = if state.composer_recovery.drafts.len() > 1 {
        format!(
            "[{action} · {} saved]",
            state.composer_recovery.drafts.len()
        )
    } else {
        format!("[{action}]")
    };
    let width = u16::try_from(label.width().min(usize::from(area.width))).map_err(|source| {
        crate::TuiError::FrameCoordinate {
            value: label.width(),
            source,
        }
    })?;
    if width == 0 {
        return Ok(None);
    }
    let area = Rect {
        column: area.column + area.width - width,
        width,
        ..area
    };
    state.screen.prepared_recovery = Some(PreparedRecovery { area, id: saved.id });
    Ok(Some((label, area)))
}

pub(super) fn published(screen: &mut ScreenState, frame: &Arc<Frame>) {
    screen.recovery_hit = screen.prepared_recovery.take().map(|prepared| RecoveryHit {
        prepared,
        source: screen.conversation.viewport.source().clone(),
        frame: Arc::clone(frame),
    });
}

pub(super) fn activate(state: &mut AppState, column: u16, row: u16) -> bool {
    let Some(hit) = state.screen.recovery_hit.as_ref() else {
        return false;
    };
    if &hit.source != state.transcript.projection.source()
        || state
            .screen
            .display_frame
            .as_ref()
            .is_none_or(|frame| !Arc::ptr_eq(frame, &hit.frame))
        || state
            .composer_recovery
            .drafts
            .front()
            .is_none_or(|draft| draft.id != hit.prepared.id)
    {
        state.screen.recovery_hit = None;
        return false;
    }
    let area = hit.prepared.area;
    if row != area.row || column < area.column || column >= area.column.saturating_add(area.width) {
        return false;
    }
    let Some(mut saved) = state.composer_recovery.drafts.pop_front() else {
        return false;
    };
    state.input_editor.exchange_draft(&mut saved.draft);
    // A fresh identity prevents a queued second click from acting on an unpainted owner.
    saved.id = Uuid::new_v4();
    saved.rejected = false;
    state.composer_recovery.drafts.push_back(saved);
    state.screen.recovery_hit = None;
    state.screen.prepared_recovery = None;
    state.autocomplete = None;
    state.screen.focus = super::focus::FocusState::new();
    state.screen.conversation.feedback =
        Some("Draft recovered; your other draft is saved. Nothing was sent.".to_owned());
    state.screen.dirty = true;
    true
}

#[cfg(test)]
#[path = "composer_recovery_tests.rs"]
mod tests;
