//! Selected-call evidence jobs and direct read-only Changes rows; no current-file access.

use std::fmt::Write as _;
use std::sync::Arc;

use norn::session_view::{BodyRef, ItemId, ToolState, ToolView, ViewItemKind};
use tokio::task::{JoinError, JoinSet};

use crate::TuiError;
use crate::app::changes::{ChangeEvidence, ChangeKind, Evidence, inspect_change};
use crate::app::state::AppState;
use crate::app::transcript::Transcript;
use crate::render::frame::{Frame, PaintRow};
use crate::render::layout::Rect;
use crate::render::retained_markdown::RenderedMarkdown;
use crate::render::retained_text::TextRow;

use super::{layout_rows, push_text, safe_text};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct ChangeKey {
    item: ItemId,
    arguments: Option<BodyRef>,
    result: Option<BodyRef>,
    arguments_loaded: bool,
    result_loaded: bool,
    state: ToolState,
    result_state: Option<ToolState>,
    committed: Option<bool>,
}

pub(in crate::app) type ChangeJob = (ChangeKey, Result<Arc<RenderedMarkdown>, TuiError>);

pub(in crate::app) struct ChangesState {
    pub jobs: JoinSet<ChangeJob>,
    requested: Option<ChangeKey>,
    cache: Option<(ChangeKey, Arc<RenderedMarkdown>)>,
    rows: Vec<TextRow>,
    columns: Option<u16>,
}

impl ChangesState {
    pub fn new() -> Self {
        Self {
            jobs: JoinSet::new(),
            requested: None,
            cache: None,
            rows: Vec::new(),
            columns: None,
        }
    }

    pub fn clear(&mut self) {
        // Spawn-blocking work may finish after cancellation; its identity is rejected.
        self.jobs = JoinSet::new();
        self.requested = None;
        self.cache = None;
        self.rows.clear();
        self.columns = None;
    }
}

fn selected(state: &AppState) -> Option<(ItemId, ToolView)> {
    let id = state.screen.viewport.selected()?;
    let item = state.transcript.projection.item(id)?;
    match &item.kind {
        ViewItemKind::Tool(tool) => Some((item.id.clone(), *tool.clone())),
        _ => None,
    }
}

fn complete<'a>(transcript: &'a Transcript, reference: Option<&BodyRef>) -> Option<&'a str> {
    let body = transcript.body(reference?)?;
    body.next_offset.is_none().then_some(body.original.as_str())
}

fn key(id: ItemId, tool: &ToolView, transcript: &Transcript) -> ChangeKey {
    ChangeKey {
        item: id,
        arguments: tool.arguments.clone(),
        result: tool.result.clone(),
        arguments_loaded: complete(transcript, tool.arguments.as_ref()).is_some(),
        result_loaded: complete(transcript, tool.result.as_ref()).is_some(),
        state: tool.state,
        result_state: tool.result_state,
        committed: tool.committed,
    }
}

/// Schedule only explicit selected-call work after approved body loading, never during paint.
pub(in crate::app) fn demand(state: &mut AppState) {
    if !state.screen.changes_open {
        return;
    }
    let Some((id, tool)) = selected(state) else {
        return;
    };
    let current = key(id, &tool, &state.transcript);
    if !state.screen.changes.jobs.is_empty() {
        return;
    }
    if state.screen.changes.requested.as_ref() == Some(&current) {
        return;
    }
    let arguments = complete(&state.transcript, tool.arguments.as_ref()).map(str::to_owned);
    let result = complete(&state.transcript, tool.result.as_ref()).map(str::to_owned);
    state.screen.changes.requested = Some(current.clone());
    state.screen.changes.jobs.spawn_blocking(move || {
        let output = inspect_change(&tool, arguments.as_deref(), result.as_deref())
            .map_err(super::interaction)
            .and_then(|evidence| {
                describe(&tool, &evidence).map_err(|source| TuiError::ChangeFormatting {
                    item: Box::new(current.item.clone()),
                    source,
                })
            })
            .and_then(|text| safe_text(&text));
        (current, output)
    });
}

/// A completed job is publishable only for the still-selected exact revision.
pub(in crate::app) fn finish(
    state: &mut AppState,
    result: Result<ChangeJob, JoinError>,
) -> Result<(), TuiError> {
    let (requested, result) = result.map_err(|source| TuiError::ViewTask {
        operation: "recorded change inspection",
        source,
    })?;
    state.screen.allow_body_load = true;
    state.screen.dirty = true;
    let Some((id, tool)) = selected(state) else {
        return Ok(());
    };
    if requested != key(id, &tool, &state.transcript) {
        return Ok(());
    }
    let text = match result {
        Ok(text) => text,
        Err(error) => safe_text(&format!(
            "Changes unavailable: {error}\nOriginal recorded tool detail remains available."
        ))?,
    };
    state.screen.changes.cache = Some((requested, text));
    state.screen.changes.columns = None;
    state.screen.dirty = true;
    Ok(())
}

pub(super) fn paint(state: &mut AppState, frame: &mut Frame, area: Rect) -> Result<(), TuiError> {
    let Some((id, tool)) = selected(state) else {
        return push_text(
            frame,
            "Changes · select a tool call with Up/Down in the conversation",
            area,
            false,
            false,
        );
    };
    let current = key(id, &tool, &state.transcript);
    let Some((cached, text)) = &state.screen.changes.cache else {
        return push_text(
            frame,
            "Changes · reading recorded call evidence",
            area,
            false,
            false,
        );
    };
    if cached != &current {
        return push_text(
            frame,
            "Changes · selected call revision changed; reading its evidence",
            area,
            false,
            false,
        );
    }
    if state.screen.changes.columns != Some(area.width) {
        state.screen.changes.rows = layout_rows(&text.styled, area.width)?;
        state.screen.changes.columns = Some(area.width);
    }
    let start = state.screen.changes_row.min(
        state
            .screen
            .changes
            .rows
            .len()
            .saturating_sub(usize::from(area.height)),
    );
    for (index, geometry) in state
        .screen
        .changes
        .rows
        .iter()
        .skip(start)
        .take(usize::from(area.height))
        .enumerate()
    {
        frame.rows.push(PaintRow {
            area,
            row: u16::try_from(index).map_err(|source| TuiError::FrameCoordinate {
                value: index,
                source,
            })?,
            text: Arc::clone(text),
            geometry: geometry.clone(),
            selected: false,
            selection: Vec::new(),
            composer: false,
        });
    }
    Ok(())
}

fn describe(tool: &ToolView, evidence: &ChangeEvidence) -> Result<String, std::fmt::Error> {
    let summary = crate::tools::summary::summarize(tool, true).header();
    let mut text = format!(
        "Changes · recorded call only\n{summary}\n{}\n",
        evidence.applied.label()
    );
    match &evidence.change {
        ChangeKind::Edit { path, result_path, old_string, new_string, occurrence, after_hash } => {
            text.push_str("Requested edit fragment (not a whole-file baseline)\n");
            field(&mut text, "argument path", path)?; field(&mut text, "result path", result_path)?;
            field(&mut text, "occurrence", occurrence)?; field(&mut text, "recorded after hash", after_hash)?;
            fragment(&mut text, "-", old_string)?; fragment(&mut text, "+", new_string)?;
        }
        ChangeKind::Write { path, result_path, content, before, bytes_written } => {
            text.push_str("Submitted replacement content\n");
            field(&mut text, "argument path", path)?; field(&mut text, "result path", result_path)?;
            field(&mut text, "before baseline", before)?; field(&mut text, "recorded bytes written", bytes_written)?;
            fragment(&mut text, "+", content)?;
        }
        ChangeKind::Patch { supplied_patch, working_dir, per_file, files_modified, files_attempted } => {
            text.push_str("Submitted patch (recorded format; no current-disk comparison)\n");
            field(&mut text, "recorded working directory", working_dir)?;
            match per_file { Evidence::Available(files) => for file in files { field(&mut text, "receipt path", &file.path)?; field(&mut text, "receipt status", &file.status)?; }, Evidence::Unavailable(reason) => writeln!(text, "Per-file receipts unavailable: {reason:?}")? }
            list(&mut text, "modified list", files_modified)?; list(&mut text, "attempted list", files_attempted)?;
            fragment(&mut text, "", supplied_patch)?;
        }
        ChangeKind::Unknown => text.push_str("Structured file-change coverage is unknown for this tool. Expand its original arguments/result; process success is not mutation evidence.\n"),
    }
    writeln!(
        text,
        "Evidence call: {:?}; tool: {:?}; lifecycle: {}; result: {:?}",
        evidence.call_id,
        evidence.tool_name,
        crate::tools::summary::state_label(evidence.state()),
        evidence.result_state()
    )?;
    field(&mut text, "reported committed field", &evidence.committed)?;
    if let Some(error) = &evidence.error {
        writeln!(text, "Recorded error: {error}")?;
    }
    match &evidence.diagnostics {
        Evidence::Available(entries) => {
            for entry in entries {
                writeln!(text, "Diagnostic: {entry}")?;
            }
        }
        Evidence::Unavailable(reason) => {
            writeln!(text, "Diagnostics unavailable: {reason:?}")?;
        }
    }
    Ok(text)
}

fn field<T: std::fmt::Display>(
    text: &mut String,
    label: &str,
    value: &Evidence<T>,
) -> std::fmt::Result {
    match value {
        Evidence::Available(value) => writeln!(text, "{label}: {value}"),
        Evidence::Unavailable(reason) => writeln!(text, "{label}: unavailable ({reason:?})"),
    }
}

fn fragment(text: &mut String, prefix: &str, value: &Evidence<String>) -> std::fmt::Result {
    match value {
        Evidence::Available(value) => {
            for line in value.split('\n') {
                text.push_str(prefix);
                text.push_str(line);
                text.push('\n');
            }
            Ok(())
        }
        Evidence::Unavailable(reason) => writeln!(text, "Content unavailable: {reason:?}"),
    }
}

fn list(text: &mut String, label: &str, values: &Evidence<Vec<String>>) -> std::fmt::Result {
    match values {
        Evidence::Available(values) => {
            text.push_str(label);
            text.push_str(":\n");
            for value in values {
                text.push_str(value);
                text.push('\n');
            }
            Ok(())
        }
        Evidence::Unavailable(reason) => writeln!(text, "{label}: unavailable ({reason:?})"),
    }
}
