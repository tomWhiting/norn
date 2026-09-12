//! One published reading viewport, retained across revision retirement without source authority.

use std::ops::Range;
use std::sync::Arc;

use norn::session_view::ViewSource;

use crate::TuiError;
use crate::app::state::AppState;
use crate::app::viewport::{AnchorPosition, ViewAnchor};
use crate::render::frame::{Frame, PaintRow};
use crate::render::layout::Rect;
use crate::render::retained_markdown::{RenderedMarkdown, SourceDisplaySpan, SourceMapping};
use crate::render::retained_text::{StyleSpan, StyledText, TextRow};

use super::ScreenState;
use super::hit::HitRow;

/// Shares only the last published visible rows until their revision is retired.
pub(in crate::app) struct ReadingSnapshot {
    source: ViewSource,
    anchor: ViewAnchor,
    area: Rect,
    hits: Vec<SnapshotRow>,
    pieces: Option<Vec<Piece>>,
}

struct SnapshotRow {
    hit: HitRow,
    selected: bool,
    selection: Vec<Range<usize>>,
}

struct Piece {
    text: Arc<RenderedMarkdown>,
    selected: bool,
    selection: Vec<Range<usize>>,
    input: bool,
    first: bool,
    layout: Option<(u16, Vec<TextRow>)>,
}

/// Prepared rows acquire display authority only after the terminal flush succeeds.
pub(in crate::app) fn published(screen: &mut ScreenState, frame: &Arc<Frame>) {
    let Some(area) = screen.prepared_reading.take() else {
        return;
    };
    let Some(anchor) = screen.viewport.anchor().or_else(|| screen.visible.first()) else {
        screen.reading_snapshot = None;
        return;
    };
    // An active drag can substitute older rows into the prepared frame. Do not
    // describe those pixels using the newer, unpainted original-body hit map.
    let mut paints = frame.rows.iter();
    let hits: Option<Vec<_>> = screen
        .hit_rows
        .iter()
        .map(|hit| {
            paints
                .find(|paint| {
                    paint.area == hit.area
                        && paint.row == hit.row
                        && Arc::ptr_eq(&paint.text, &hit.text)
                        && paint.geometry == hit.geometry
                })
                .map(|paint| SnapshotRow {
                    hit: hit.clone(),
                    selected: paint.selected,
                    selection: paint.selection.clone(),
                })
        })
        .collect();
    let Some(hits) = hits else {
        return;
    };
    screen.reading_snapshot = Some(ReadingSnapshot {
        source: screen.viewport.source().clone(),
        anchor: anchor.clone(),
        area,
        hits,
        pieces: None,
    });
}

/// Reflow the displayed snapshot only; no body read, identity substitution or live hit map.
pub(super) fn paint(state: &mut AppState, frame: &mut Frame, area: Rect) -> Result<bool, TuiError> {
    let Some(snapshot) = state.screen.reading_snapshot.as_mut() else {
        return Ok(false);
    };
    let Some(anchor) = state.screen.viewport.anchor() else {
        return Ok(false);
    };
    let projection = &state.transcript.projection;
    let current_item = projection
        .alias(&snapshot.anchor.item)
        .unwrap_or(&snapshot.anchor.item);
    if snapshot.source != *projection.source()
        || current_item != &anchor.item
        || snapshot.anchor.position != anchor.position
    {
        return Ok(false);
    }
    if snapshot.pieces.is_none() {
        snapshot.pieces = Some(pieces(&snapshot.hits, snapshot.area)?);
        snapshot.hits.clear();
    }
    if let Some(pieces) = snapshot.pieces.as_mut() {
        paint_pieces(pieces, frame, area)?;
    }
    Ok(true)
}

fn pieces(hits: &[SnapshotRow], area: Rect) -> Result<Vec<Piece>, TuiError> {
    let mut result = Vec::new();
    let mut start = 0;
    while let Some(first_row) = hits.get(start) {
        let first = &first_row.hit;
        let mut end = start + 1;
        let mut range = first.geometry.bytes();
        while let Some(next_row) = hits.get(end) {
            let next = &next_row.hit;
            let next_range = next.geometry.bytes();
            if first_row.selected != next_row.selected
                || !Arc::ptr_eq(&first.text, &next.text)
                || first.anchor.item != next.anchor.item
                || first.body != next.body
                || first.area != next.area
                || next_range.start < range.end
                || !matches!(
                    first.text.styled.text().get(range.end..next_range.start),
                    Some("" | "\n")
                )
            {
                break;
            }
            range.end = next_range.end;
            end += 1;
        }
        let selection = hits[start..end]
            .iter()
            .flat_map(|row| &row.selection)
            .filter_map(|selected| {
                let first = selected.start.max(range.start);
                let last = selected.end.min(range.end);
                (first < last)
                    .then_some(first.saturating_sub(range.start)..last.saturating_sub(range.start))
            })
            .collect();
        let text = display_slice(&first.text, range)?;
        result.push(Piece {
            text: Arc::new(text),
            selected: first_row.selected,
            selection,
            input: first.area.column > area.column,
            first: matches!(
                first.anchor.position,
                AnchorPosition::Body {
                    original_offset: 0,
                    ..
                }
            ),
            layout: None,
        });
        start = end;
    }
    Ok(result)
}

fn display_slice(
    text: &RenderedMarkdown,
    range: Range<usize>,
) -> Result<RenderedMarkdown, TuiError> {
    let display = text.styled.text()[range.clone()].to_owned();
    let spans = text
        .styled
        .spans()
        .iter()
        .filter_map(|span| {
            let start = span.range.start.max(range.start);
            let end = span.range.end.min(range.end);
            (start < end).then_some(StyleSpan {
                range: start.saturating_sub(range.start)..end.saturating_sub(range.start),
                style: span.style,
            })
        })
        .collect();
    let source_spans = if display.is_empty() {
        Vec::new()
    } else {
        vec![SourceDisplaySpan {
            display: 0..display.len(),
            source: SourceMapping::Generated,
        }]
    };
    Ok(RenderedMarkdown {
        styled: StyledText::new(display, spans)?,
        spans: source_spans,
    })
}

fn paint_pieces(pieces: &mut [Piece], frame: &mut Frame, area: Rect) -> Result<(), TuiError> {
    let mut row = 0;
    for piece in pieces {
        if row >= area.height {
            break;
        }
        let columns = if piece.input {
            area.width.saturating_sub(2)
        } else {
            area.width
        };
        if piece
            .layout
            .as_ref()
            .is_none_or(|(width, _)| *width != columns)
        {
            piece.layout = Some((columns, super::layout_rows(&piece.text.styled, columns)?));
        }
        if let Some((_, rows)) = &piece.layout {
            for (index, geometry) in rows.iter().enumerate() {
                if row >= area.height {
                    break;
                }
                let paint_area = if piece.input {
                    super::composer::input_margin(
                        frame,
                        area,
                        usize::from(row),
                        piece.first && index == 0,
                    )?
                } else {
                    area
                };
                frame.rows.push(PaintRow {
                    area: paint_area,
                    row,
                    text: Arc::clone(&piece.text),
                    geometry: geometry.clone(),
                    selected: piece.selected,
                    selection: piece.selection.clone(),
                    composer: false,
                });
                row += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "reading_snapshot_tests.rs"]
mod tests;
