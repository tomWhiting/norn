//! Mouse focus, pane controls and source-aware visible-pane drag selection; no runtime control.

use std::num::NonZeroU16;

use termina::event::{KeyCode, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::TuiError;
use crate::app::display_selection::{DisplayPane, revoke_pointer_mapping};
use crate::app::focus::Focus;
use crate::app::render::interaction;
use crate::app::state::AppState;
use crate::render::layout::{Layout, Rect, SplitPreference, UpperLayout, UpperPane};

use super::{browse_rows, expand, pin_visible, select_hit};

pub(in crate::app) fn mouse(event: MouseEvent, state: &mut AppState) -> bool {
    let result = apply_mouse(event, state).and_then(|handled| {
        if handled {
            crate::app::frontend_preferences::edited(state)?;
        }
        Ok(handled)
    });
    match result {
        Ok(handled) => {
            if handled {
                state.screen.dirty = true;
                state.screen.allow_body_load = true;
            }
            handled
        }
        Err(error) => {
            state.screen.feedback = Some(error.to_string());
            state.screen.dirty = true;
            true
        }
    }
}

fn apply_mouse(event: MouseEvent, state: &mut AppState) -> Result<bool, TuiError> {
    let Layout::Ready { upper, composer } = state.screen.layout else {
        return Ok(false);
    };
    if popup(event, state, composer)? {
        return Ok(true);
    }
    if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
        && state
            .screen
            .pane_switch
            .is_some_and(|area| contains(area, event))
    {
        revoke_pointer_mapping(&mut state.screen);
        state.screen.upper = match state.screen.upper {
            UpperPane::Conversation => UpperPane::Changes,
            UpperPane::Changes => UpperPane::Conversation,
        };
        return Ok(true);
    }
    if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
        && state
            .screen
            .composer_send_key_area
            .is_some_and(|area| contains(area, event))
    {
        state.composer_send_key = state.composer_send_key.next_policy();
        state.screen.feedback = Some(format!(
            "Composer send key: {}",
            state.composer_send_key.label()
        ));
        return Ok(true);
    }
    if state.screen.dragging_composer {
        if matches!(event.kind, MouseEventKind::Up(_)) {
            state.screen.dragging_composer = false;
        }
        if matches!(
            event.kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        ) {
            composer_pointer(state, event, true)?;
        }
        return Ok(true);
    }
    if state.screen.dragging_divider {
        if matches!(
            event.kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        ) {
            drag_divider(state, event.column)?;
        }
        if matches!(event.kind, MouseEventKind::Up(_)) {
            state.screen.dragging_divider = false;
        }
        return Ok(true);
    }
    if state.screen.dragging_selection
        && matches!(
            event.kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        )
    {
        extend_display(state, event)?;
        return Ok(true);
    }
    let target = if contains(composer, event) {
        Some(Focus::Composer)
    } else {
        match upper {
            UpperLayout::Single { pane, area } if contains(area, event) => Some(match pane {
                UpperPane::Conversation => Focus::Conversation,
                UpperPane::Changes => Focus::Changes,
            }),
            UpperLayout::Split {
                conversation,
                changes,
                divider,
            } => {
                if contains(divider, event) {
                    Some(Focus::Divider)
                } else if contains(conversation, event) {
                    Some(Focus::Conversation)
                } else if contains(changes, event) {
                    Some(Focus::Changes)
                } else {
                    None
                }
            }
            UpperLayout::Single { .. } => None,
        }
    };
    let Some(target) = target else {
        return Ok(false);
    };
    match event.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if target != Focus::Composer => {
            state
                .screen
                .focus
                .focus(target, state.screen.availability())
                .map_err(interaction)?;
            browse_rows(state, event.kind == MouseEventKind::ScrollUp, 1)?;
            Ok(true)
        }
        MouseEventKind::Down(MouseButton::Left) => {
            state.screen.feedback = None;
            state
                .screen
                .focus
                .focus(target, state.screen.availability())
                .map_err(interaction)?;
            crate::app::autocomplete::dismiss(state);
            pin_visible(state)?;
            state.screen.dragging_selection = false;
            state.screen.dragging_divider = target == Focus::Divider;
            state.screen.dragging_composer = false;
            if target == Focus::Composer {
                state.screen.dragging_composer =
                    composer_pointer(state, event, event.modifiers.contains(Modifiers::SHIFT))?;
            }
            if let Some(pane) = selection_pane(upper, target, state.screen.auxiliary) {
                begin_display(state, event, pane)?;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn selection_pane(
    upper: UpperLayout,
    target: Focus,
    auxiliary: crate::app::render::AuxiliaryPane,
) -> Option<DisplayPane> {
    match (upper, target) {
        (UpperLayout::Single { area, .. }, Focus::Conversation) => {
            Some(DisplayPane::Conversation(area))
        }
        (UpperLayout::Single { area, .. }, Focus::Changes) => {
            Some(DisplayPane::Auxiliary(area, auxiliary))
        }
        (UpperLayout::Split { conversation, .. }, Focus::Conversation) => {
            Some(DisplayPane::Conversation(conversation))
        }
        (UpperLayout::Split { changes, .. }, Focus::Changes) => {
            Some(DisplayPane::Auxiliary(changes, auxiliary))
        }
        _ => None,
    }
}

fn begin_display(
    state: &mut AppState,
    event: MouseEvent,
    pane: DisplayPane,
) -> Result<(), TuiError> {
    let frame =
        state.screen.display_frame.as_ref().ok_or_else(|| {
            interaction(std::io::Error::other("no published display for selection"))
        })?;
    let selection = crate::app::display_selection::DisplaySelection::capture(
        state.transcript.projection.source().clone(),
        std::sync::Arc::clone(frame),
        pane,
        &state.screen.hit_rows,
        event.column,
        event.row,
    )
    .map_err(interaction)?;
    let hit = selection.hit(event.column, event.row).cloned();
    let column = selection.focus_column().map_err(interaction)?;
    state.screen.selection = None;
    state.screen.selection_item = None;
    state.screen.display_selection = Some(selection);
    state.screen.dragging_selection = true;
    if let Some(hit) = hit {
        state
            .screen
            .viewport
            .select(hit.anchor.item.clone(), &state.transcript.projection)
            .map_err(interaction)?;
        state.screen.changes_row = 0;
        if hit.body.is_some() {
            attempt_original(state, &hit, column, false);
        }
    }
    Ok(())
}

fn extend_display(state: &mut AppState, event: MouseEvent) -> Result<(), TuiError> {
    let Some(selection) = state.screen.display_selection.as_mut() else {
        state.screen.dragging_selection = false;
        return Ok(());
    };
    selection.extend(event.column, event.row);
    let column = selection.focus_column().map_err(interaction)?;
    let moved = selection.moved();
    let hit = selection.hit(event.column, event.row).cloned();
    let released = matches!(event.kind, MouseEventKind::Up(_));
    if released {
        state.screen.dragging_selection = false;
    }
    if let Some(hit) = hit.as_ref().filter(|hit| {
        hit.body.as_ref()
            == state
                .screen
                .selection
                .as_ref()
                .map(crate::app::selection::Selection::reference)
    }) {
        if hit.body.is_some() {
            attempt_original(state, hit, column, true);
        }
    } else {
        state.screen.selection = None;
        state.screen.selection_item = None;
    }
    if released
        && !moved
        && hit.as_ref().is_some_and(|hit| {
            hit.body.is_none()
                && state
                    .transcript
                    .projection
                    .item(&hit.anchor.item)
                    .is_some_and(|item| {
                        matches!(item.kind, norn::session_view::ViewItemKind::Tool(_))
                    })
        })
    {
        expand(state, None)?;
    }
    Ok(())
}

fn attempt_original(
    state: &mut AppState,
    hit: &crate::app::render::hit::HitRow,
    column: u16,
    extend: bool,
) {
    if let Err(error) = select_hit(state, hit, column, extend) {
        state.screen.selection = None;
        state.screen.selection_item = None;
        state.screen.feedback = Some(format!(
            "Displayed-text selection; original mapping unavailable: {error}"
        ));
    }
}

fn contains(area: Rect, event: MouseEvent) -> bool {
    event.column >= area.column
        && event.column < area.column.saturating_add(area.width)
        && event.row >= area.row
        && event.row < area.row.saturating_add(area.height)
}

fn drag_divider(state: &mut AppState, column: u16) -> Result<(), TuiError> {
    let Layout::Ready {
        upper:
            UpperLayout::Split {
                conversation,
                changes,
                ..
            },
        ..
    } = state.screen.layout
    else {
        return Ok(());
    };
    let width = conversation.width.saturating_add(changes.width);
    let left = column.clamp(1, width.saturating_sub(1));
    let Some(a) = NonZeroU16::new(left) else {
        return Err(interaction(std::io::Error::other(
            "divider has no conversation width",
        )));
    };
    let Some(b) = NonZeroU16::new(width - left) else {
        return Err(interaction(std::io::Error::other(
            "divider has no Changes width",
        )));
    };
    state.screen.split = SplitPreference::new(a, b);
    Ok(())
}

fn popup(event: MouseEvent, state: &mut AppState, composer: Rect) -> Result<bool, TuiError> {
    let Some(popup) = state.autocomplete.as_mut() else {
        return Ok(false);
    };
    let height = popup.height().min(composer.row);
    let area = Rect {
        row: composer.row - height,
        height,
        ..composer
    };
    if !contains(area, event) {
        return Ok(false);
    }
    match event.kind {
        MouseEventKind::ScrollUp => popup.select_up(),
        MouseEventKind::ScrollDown => popup.select_down(),
        MouseEventKind::Down(MouseButton::Left) => {
            let index = popup.visible_offset + usize::from(event.row - area.row);
            if index < popup.candidates.len() {
                popup.selected_index = index;
                crate::app::autocomplete::handle_popup_key(
                    KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                    state,
                    composer.width,
                    composer.row + composer.height,
                )?;
            }
        }
        _ => {}
    }
    Ok(true)
}

/// Apply an exact displayed input hit through the kernel's own cell map.
fn composer_pointer(
    state: &mut AppState,
    event: MouseEvent,
    extend: bool,
) -> Result<bool, TuiError> {
    if let Some(hit) =
        state
            .composer_geometry
            .pointer(&state.input_editor, event.column, event.row)?
    {
        state
            .input_editor
            .set_cell_pointer(hit.row, hit.column, extend, hit.options)?;
        return Ok(true);
    }
    Ok(false)
}
