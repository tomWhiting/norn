//! Shared frontend navigation and original-byte reading state; no provider commands.

use super::focus::Focus;
use super::render::interaction;
use super::state::AppState;
use crate::TuiError;
use crate::render::layout::{Layout, SplitPreference, UpperLayout};
use std::num::NonZeroU16;

mod commands;
mod keys;
pub(in crate::app) mod latest;
mod mouse;
pub(in crate::app) mod reading;
mod shortcut_commands;
#[cfg(test)]
pub(super) use commands::command;
pub(super) use commands::{command_named, is_frontend_command};
pub(super) use keys::key;
pub(super) use mouse::mouse;

fn focus(state: &mut AppState, target: Focus) -> Result<(), TuiError> {
    super::render::navigation::apply(state)?;
    state
        .screen
        .focus
        .focus(target, state.screen.availability())
        .map_err(interaction)?;
    if target != Focus::Composer {
        pin_visible(state)?;
    }
    Ok(())
}

pub(super) fn pin_visible(state: &mut AppState) -> Result<(), TuiError> {
    super::render::navigation::apply(state)?;
    state.transcript.cancel_latest();
    if state.screen.viewport.anchor().is_none() {
        if let Some(anchor) = state.screen.visible.first().cloned() {
            state
                .screen
                .viewport
                .scroll_to(anchor, &state.transcript.projection)
                .map_err(interaction)?;
        } else {
            state.screen.viewport.pin();
        }
    }
    Ok(())
}

fn browse(state: &mut AppState, upwards: bool) -> Result<(), TuiError> {
    let height = match state.screen.layout {
        Layout::Ready {
            upper:
                UpperLayout::Split { conversation, .. }
                | UpperLayout::Single {
                    area: conversation, ..
                },
            ..
        } => usize::from(conversation.height),
        _ => 0,
    };
    browse_rows(state, upwards, height)
}

fn browse_rows(state: &mut AppState, upwards: bool, rows: usize) -> Result<(), TuiError> {
    let available = state.screen.availability();
    let target = if available.composer {
        state.screen.focus.visible(available).map_err(interaction)?
    } else {
        Focus::Conversation
    };
    browse_target_rows(state, target, upwards, rows)
}

fn browse_target_rows(
    state: &mut AppState,
    target: Focus,
    upwards: bool,
    rows: usize,
) -> Result<(), TuiError> {
    if target == Focus::Changes {
        super::render::navigation::apply(state)?;
        state.screen.changes_row = if upwards {
            state.screen.changes_row.saturating_sub(rows)
        } else {
            state.screen.changes_row.saturating_add(rows)
        };
        return Ok(());
    }
    super::render::navigation::queue(state, upwards, rows)
}

fn ensure_selected(state: &mut AppState) -> Result<(), TuiError> {
    if state.screen.viewport.selected().is_none() {
        if let Some(anchor) = state.screen.visible.first() {
            state
                .screen
                .viewport
                .select(anchor.item.clone(), &state.transcript.projection)
                .map_err(interaction)?;
        } else {
            return Err(interaction(std::io::Error::other(
                "no visible item to select",
            )));
        }
    }
    Ok(())
}

fn expand(state: &mut AppState, explicit: Option<bool>) -> Result<(), TuiError> {
    ensure_selected(state)?;
    if let Some(item) = state.screen.viewport.selected() {
        let current = state
            .screen
            .tool_overrides
            .get(item)
            .copied()
            .unwrap_or(state.transcript.config.expanded_tools);
        state
            .screen
            .tool_overrides
            .insert(item.clone(), explicit.unwrap_or(!current));
    }
    Ok(())
}

fn select_row(state: &mut AppState, upwards: bool) -> Result<(), TuiError> {
    let selected = state.screen.viewport.selected();
    let mut ids = Vec::new();
    for anchor in &state.screen.visible {
        if ids.last() != Some(&&anchor.item) {
            ids.push(&anchor.item);
        }
    }
    let index = selected.and_then(|selected| ids.iter().position(|id| *id == selected));
    let next = match index {
        Some(index) if upwards => index.saturating_sub(1),
        Some(index) => index.saturating_add(1).min(ids.len().saturating_sub(1)),
        None => 0,
    };
    if let Some(item) = ids.get(next) {
        state
            .screen
            .viewport
            .select((*item).clone(), &state.transcript.projection)
            .map_err(interaction)?;
        state.screen.changes_row = 0;
    }
    Ok(())
}

fn resize_split(state: &mut AppState, left: bool) -> Result<(), TuiError> {
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
    let (a, b) = if left {
        (
            conversation.width.saturating_sub(1),
            changes.width.saturating_add(1),
        )
    } else {
        (
            conversation.width.saturating_add(1),
            changes.width.saturating_sub(1),
        )
    };
    let (Some(a), Some(b)) = (NonZeroU16::new(a), NonZeroU16::new(b)) else {
        return Err(interaction(std::io::Error::other(
            "split cannot have a zero weight",
        )));
    };
    state.screen.split = SplitPreference::new(a, b);
    Ok(())
}

/// Revalidate the item owner before lending any original bytes to selection/copy/export.
fn original_for<'a>(
    state: &'a AppState,
    item: &norn::session_view::ItemId,
    reference: &'a norn::session_view::BodyRef,
) -> Result<super::selection::OriginalBody<'a>, TuiError> {
    let projection = &state.transcript.projection;
    if state.screen.viewport.source() != projection.source() {
        return Err(interaction(std::io::Error::other(format!(
            "view source {:?} no longer matches projection {:?}",
            state.screen.viewport.source(),
            projection.source()
        ))));
    }
    let current_id = projection.alias(item).unwrap_or(item);
    if projection
        .item(current_id)
        .is_none_or(|item| !item.bodies.contains(reference))
    {
        return Err(interaction(std::io::Error::other(format!(
            "selected original body revision is no longer current for {item:?}"
        ))));
    }
    let body = state.transcript.body(reference).ok_or_else(|| {
        interaction(std::io::Error::other(format!(
            "selected original body is not loaded for {item:?}"
        )))
    })?;
    Ok(super::selection::OriginalBody::new(
        reference,
        &body.original,
        body.next_offset.is_none(),
    ))
}

pub(in crate::app) fn selected_text(state: &AppState) -> Result<&str, TuiError> {
    let selection = state.screen.selection.as_ref().ok_or_else(|| {
        interaction(std::io::Error::other(
            "no original text selection; drag text or use /view select",
        ))
    })?;
    let item = state
        .screen
        .selection_item
        .as_ref()
        .ok_or_else(|| interaction(std::io::Error::other("selection has no item owner")))?;
    let original = original_for(state, item, selection.reference())?;
    selection
        .read(state.transcript.projection.source(), Some(original))
        .map_err(interaction)
}

fn select_original(
    state: &mut AppState,
    body_index: usize,
    range: Option<std::ops::Range<usize>>,
) -> Result<(), TuiError> {
    ensure_selected(state)?;
    let id = state
        .screen
        .viewport
        .selected()
        .cloned()
        .ok_or_else(|| interaction(std::io::Error::other("no selected item")))?;
    let reference = state
        .transcript
        .projection
        .item(&id)
        .and_then(|item| item.bodies.get(body_index))
        .cloned()
        .ok_or_else(|| {
            interaction(std::io::Error::other(format!(
                "item {id:?} has no body at index {body_index}"
            )))
        })?;
    let body = state.transcript.body(&reference).ok_or_else(|| {
        interaction(std::io::Error::other(format!(
            "body {body_index} of {id:?} is not loaded; expand it and use /view more"
        )))
    })?;
    let range = match range {
        Some(range) => range,
        None if body.next_offset.is_none() => 0..body.original.len(),
        None => {
            return Err(interaction(std::io::Error::other(
                "whole body is not loaded; use /view more or select an explicit loaded range",
            )));
        }
    };
    let selection = super::selection::Selection::from_original(
        state.transcript.projection.source(),
        original_for(state, &id, &reference)?,
        range,
    )
    .map_err(interaction)?;
    state.screen.selection = Some(selection);
    state.screen.selection_item = Some(id);
    state.screen.display_selection = None;
    state.screen.feedback =
        Some("Original text selected; F4 copies, F5 prepares export".to_owned());
    Ok(())
}

fn select_hit(
    state: &mut AppState,
    hit: &super::render::hit::HitRow,
    column: u16,
    extend: bool,
) -> Result<(), TuiError> {
    use crate::render::retained_markdown::BoundaryAffinity;
    let reference = hit.body.as_ref().ok_or_else(|| {
        interaction(std::io::Error::other(
            "this row is display chrome, not original body text",
        ))
    })?;
    let offset = hit.displayed_offset(column);
    let original = original_for(state, &hit.anchor.item, reference)?;
    let mapped = super::selection::MappedBody::new(reference, &hit.text);
    let selection = if extend {
        let mut selection = state.screen.selection.clone().ok_or_else(|| {
            interaction(std::io::Error::other(
                "selection drag has no original starting point",
            ))
        })?;
        selection
            .extend(
                state.transcript.projection.source(),
                original,
                mapped,
                offset,
                BoundaryAffinity::Before,
            )
            .map_err(interaction)?;
        selection
    } else {
        super::selection::Selection::start(
            state.transcript.projection.source(),
            original,
            mapped,
            offset,
            BoundaryAffinity::After,
        )
        .map_err(interaction)?
    };
    state
        .screen
        .viewport
        .select(hit.anchor.item.clone(), &state.transcript.projection)
        .map_err(interaction)?;
    state.screen.selection = Some(selection);
    state.screen.selection_item = Some(hit.anchor.item.clone());
    Ok(())
}

/// Clipboard writes stay with the one terminal writer and run only after explicit request.
pub(super) fn flush_copy(
    state: &mut AppState,
    guard: &mut crate::terminal::setup::TerminalGuard,
) -> Result<(), TuiError> {
    use crate::terminal::clipboard::{CopyPreparation, prepare_copy};
    use std::io::Write as _;
    if !std::mem::take(&mut state.screen.request_copy) {
        return Ok(());
    }
    let selected = match selected_text(state) {
        Ok(text) => Ok((text.to_owned(), "original selected bytes")),
        Err(error) => match state.screen.display_selection.as_ref() {
            Some(selection) => selection
                .text(state.transcript.projection.source())
                .map(|text| (text, "displayed-text snapshot bytes"))
                .map_err(interaction),
            None => Err(error),
        },
    };
    let prepared = selected.map(|(text, scope)| {
        (
            prepare_copy(state.transcript.config.clipboard, &text),
            scope,
        )
    });
    let message = match prepared {
        Err(error) => format!("Copy unavailable: {error}"),
        Ok((CopyPreparation::Unavailable(reason), _)) => format!(
            "Clipboard unavailable ({reason:?}); use /view clipboard osc52 to permit terminal copy, or /view export <absolute-path>"
        ),
        Ok((CopyPreparation::Ready(copy), scope)) => match guard
            .terminal_mut()
            .write_all(copy.as_bytes())
            .and_then(|()| guard.terminal_mut().flush())
        {
            Ok(()) => format!(
                "Sent {} selected bytes (scope: {scope}; {} after control escaping) to the terminal clipboard transport; clipboard acceptance is unconfirmed",
                copy.original_bytes(),
                copy.sanitized_bytes()
            ),
            Err(error) => format!("Clipboard send failed: {error}"),
        },
    };
    state.screen.feedback = Some(message.clone());
    super::notices::notice(state, "Copy", Some(&message))?;
    state.screen.dirty = true;
    Ok(())
}

fn prepare_command(state: &mut AppState, command: &str) -> Result<(), TuiError> {
    if !state.input_editor.is_empty() {
        state.screen.feedback = Some(format!("Draft preserved; use {command} when ready"));
        return Ok(());
    }
    state
        .screen
        .focus
        .focus(Focus::Composer, state.screen.availability())
        .map_err(interaction)?;
    state.input_editor.paste_cells(command)?;
    Ok(())
}
