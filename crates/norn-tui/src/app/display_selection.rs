//! Immutable visible-pane selection; displayed bytes never grant original-source authority.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use norn::session_view::ViewSource;

use crate::render::frame::{Frame, PaintRow};
use crate::render::layout::{Layout, Rect, UpperLayout, UpperPane};
use crate::render::retained_text::{AtomKind, TextAtom, TextError};

use super::render::{AuxiliaryPane, ScreenState, hit::HitRow};

/// Geometry alone cannot identify content after a pane command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DisplayPane {
    Conversation(Rect),
    Auxiliary(Rect, AuxiliaryPane),
}

impl DisplayPane {
    fn area(self) -> Rect {
        match self {
            Self::Conversation(area) | Self::Auxiliary(area, _) => area,
        }
    }

    fn matches(self, layout: Layout, auxiliary: AuxiliaryPane) -> bool {
        match (self, layout) {
            (
                Self::Conversation(area),
                Layout::Ready {
                    upper:
                        UpperLayout::Single {
                            pane: UpperPane::Conversation,
                            area: current,
                        },
                    ..
                },
            ) => area == current,
            (
                Self::Conversation(area),
                Layout::Ready {
                    upper: UpperLayout::Split { conversation, .. },
                    ..
                },
            ) => area == conversation,
            (
                Self::Auxiliary(area, content),
                Layout::Ready {
                    upper:
                        UpperLayout::Single {
                            pane: UpperPane::Changes,
                            area: current,
                        },
                    ..
                },
            ) => area == current && content == auxiliary,
            (
                Self::Auxiliary(area, content),
                Layout::Ready {
                    upper: UpperLayout::Split { changes, .. },
                    ..
                },
            ) => area == changes && content == auxiliary,
            _ => false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum DisplaySelectionError {
    #[error("display selection source no longer owns the current view")]
    Source,
    #[error("display selection point ({column}, {row}) is outside pane {area:?}")]
    Point { column: u16, row: u16, area: Rect },
    #[error("display selection column {column} exceeds terminal coordinates: {source}")]
    Column {
        column: usize,
        #[source]
        source: std::num::TryFromIntError,
    },
    #[error("display selection geometry: {0}")]
    Text(#[from] TextError),
}

struct Glyph {
    column: usize,
    paint: usize,
    atom: TextAtom,
}

/// A gesture captures only the visible pane, sharing its already-published text.
pub(super) struct DisplaySelection {
    source: ViewSource,
    frame: Arc<Frame>,
    pane: DisplayPane,
    area: Rect,
    lines: Vec<Vec<Glyph>>,
    hits: Vec<HitRow>,
    anchor: (usize, usize),
    focus: (usize, usize),
    pointer_origin: (usize, usize),
}

impl DisplaySelection {
    pub fn capture(
        source: ViewSource,
        frame: Arc<Frame>,
        pane: DisplayPane,
        hits: &[HitRow],
        column: u16,
        row: u16,
    ) -> Result<Self, DisplaySelectionError> {
        let area = pane.area();
        if !contains(area, column, row) {
            return Err(DisplaySelectionError::Point { column, row, area });
        }
        let mut lines: Vec<BTreeMap<usize, Glyph>> =
            (0..area.height).map(|_| BTreeMap::new()).collect();
        for (paint, line) in frame.rows.iter().enumerate() {
            if !in_pane(line, area) {
                continue;
            }
            let target = &mut lines[usize::from(line.area.row + line.row - area.row)];
            for atom in line.geometry.clip(0, usize::from(line.area.width))? {
                if atom.kind == AtomKind::Invisible {
                    continue;
                }
                let column = usize::from(line.area.column - area.column) + atom.column;
                insert_glyph(
                    target,
                    Glyph {
                        column,
                        paint,
                        atom,
                    },
                );
            }
        }
        let mut selection = Self {
            source,
            frame,
            pane,
            area,
            lines: lines
                .into_iter()
                .map(|line| line.into_values().collect())
                .collect(),
            hits: hits
                .iter()
                .filter(|hit| contains(area, hit.area.column, hit.area.row + hit.row))
                .cloned()
                .collect(),
            anchor: (
                usize::from(row - area.row),
                usize::from(column - area.column),
            ),
            focus: (0, 0),
            pointer_origin: (
                usize::from(row - area.row),
                usize::from(column - area.column),
            ),
        };
        selection.anchor = selection.boundary(column, row, false);
        selection.focus = selection.anchor;
        Ok(selection)
    }

    pub fn extend(&mut self, column: u16, row: u16) {
        let point = (
            usize::from(row.saturating_sub(self.area.row)),
            usize::from(column.saturating_sub(self.area.column)),
        );
        self.focus = self.boundary(column, row, point > self.pointer_origin);
    }

    /// Original mapping consumes the same whole-grapheme edge as display paint.
    pub fn focus_column(&self) -> Result<u16, DisplaySelectionError> {
        let column = usize::from(self.area.column) + self.focus.1;
        u16::try_from(column).map_err(|source| DisplaySelectionError::Column { column, source })
    }

    pub fn moved(&self) -> bool {
        self.anchor != self.focus
    }

    pub fn hit(&self, column: u16, row: u16) -> Option<&HitRow> {
        self.hits.iter().rev().find(|hit| hit.contains(column, row))
    }

    pub fn text(&self, source: &ViewSource) -> Result<String, DisplaySelectionError> {
        if &self.source != source {
            return Err(DisplaySelectionError::Source);
        }
        let (start, end) = self.ordered();
        let mut output = String::new();
        for row in start.0..=end.0 {
            if row > start.0 {
                output.push('\n');
            }
            let selected = self.columns(row);
            let mut column = selected.start;
            for glyph in &self.lines[row] {
                if glyph.column >= selected.end {
                    break;
                }
                if glyph.column + glyph.atom.width <= selected.start {
                    continue;
                }
                output.extend(std::iter::repeat_n(
                    ' ',
                    glyph.column.saturating_sub(column),
                ));
                match glyph.atom.kind {
                    AtomKind::Glyph => output.push_str(
                        &self.frame.rows[glyph.paint].text.styled.text()[glyph.atom.bytes.clone()],
                    ),
                    AtomKind::Tab | AtomKind::Unpaintable => {
                        output.extend(std::iter::repeat_n(' ', glyph.atom.width));
                    }
                    AtomKind::Invisible => {}
                }
                column = glyph.column + glyph.atom.width;
            }
        }
        Ok(output)
    }

    fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    fn columns(&self, row: usize) -> Range<usize> {
        let (start, end) = self.ordered();
        if row < start.0 || row > end.0 {
            return 0..0;
        }
        let first = if row == start.0 { start.1 } else { 0 };
        let last = if row == end.0 {
            end.1
        } else {
            usize::from(self.area.width)
        };
        first..last
    }

    fn boundary(&self, column: u16, row: u16, after: bool) -> (usize, usize) {
        let row = usize::from(row.saturating_sub(self.area.row).min(self.area.height - 1));
        let column = usize::from(column.saturating_sub(self.area.column).min(self.area.width));
        let boundary = self.lines[row]
            .iter()
            .find(|glyph| column > glyph.column && column < glyph.column + glyph.atom.width)
            .map_or(column, |glyph| {
                glyph.column + if after { glyph.atom.width } else { 0 }
            });
        (row, boundary)
    }
}

/// Replace only overlapping atoms; normal left-to-right capture is not quadratic.
fn insert_glyph(target: &mut BTreeMap<usize, Glyph>, glyph: Glyph) {
    let column = glyph.column;
    let end = column + glyph.atom.width;
    let previous = target
        .range(..column)
        .next_back()
        .and_then(|(start, previous)| (start + previous.atom.width > column).then_some(*start));
    if let Some(start) = previous {
        target.remove(&start);
    }
    let covered: Vec<_> = target.range(column..end).map(|(start, _)| *start).collect();
    for start in covered {
        target.remove(&start);
    }
    target.insert(column, glyph);
}

/// Keep copy bytes, but no old frame may authorize a new pointer gesture.
pub(super) fn revoke_pointer_mapping(screen: &mut ScreenState) {
    screen.dragging_selection = false;
    screen.display_frame = None;
}

/// Run before resize measurement, including when publication is event-batched.
pub(super) fn sync_geometry(screen: &mut ScreenState, columns: u16, rows: u16) {
    let changed = screen
        .display_frame
        .as_ref()
        .is_some_and(|frame| match frame.layout {
            Layout::Ready { composer, .. } => {
                composer.width != columns
                    || u32::from(composer.row) + u32::from(composer.height) != u32::from(rows)
            }
            Layout::ResizeRequired { area } => area.width != columns || area.height != rows,
            Layout::NoPaint => columns != 0 && rows != 0,
        });
    if changed {
        revoke_pointer_mapping(screen);
    }
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.column
        && column < area.column.saturating_add(area.width)
        && row >= area.row
        && row < area.row.saturating_add(area.height)
}

fn in_pane(row: &PaintRow, area: Rect) -> bool {
    contains(area, row.area.column, row.area.row + row.row)
        && row.area.column.saturating_add(row.area.width) <= area.column.saturating_add(area.width)
}

/// Keep a dragged pane stable; completed snapshots highlight only unchanged displayed rows.
pub(super) fn paint(
    screen: &mut ScreenState,
    frame: &mut Frame,
) -> Result<(), DisplaySelectionError> {
    let Some(selection) = screen.display_selection.as_ref() else {
        return Ok(());
    };
    if &selection.source != screen.viewport.source() {
        return Err(DisplaySelectionError::Source);
    }
    if !selection.pane.matches(frame.layout, screen.auxiliary) {
        screen.dragging_selection = false;
        return Ok(());
    }
    if screen.dragging_selection {
        frame.rows.retain(|row| !in_pane(row, selection.area));
        frame.rows.extend(
            selection
                .frame
                .rows
                .iter()
                .filter(|row| in_pane(row, selection.area))
                .map(|row| PaintRow {
                    area: row.area,
                    row: row.row,
                    text: Arc::clone(&row.text),
                    geometry: row.geometry.clone(),
                    selected: row.selected,
                    selection: Vec::new(),
                    composer: false,
                }),
        );
    }
    // Several overlays may occupy one terminal row. Keep the captured paint
    // identity on each range rather than collapsing them by screen position.
    let selected: Vec<BTreeMap<usize, Vec<Range<usize>>>> = selection
        .lines
        .iter()
        .enumerate()
        .map(|(index, glyphs)| {
            let columns = selection.columns(index);
            let mut paints = BTreeMap::<usize, Vec<Range<usize>>>::new();
            for glyph in glyphs {
                if glyph.column < columns.end && columns.start < glyph.column + glyph.atom.width {
                    let ranges = paints.entry(glyph.paint).or_default();
                    if let Some(previous) = ranges
                        .last_mut()
                        .filter(|range| range.end == glyph.atom.bytes.start)
                    {
                        previous.end = glyph.atom.bytes.end;
                    } else {
                        ranges.push(glyph.atom.bytes.clone());
                    }
                }
            }
            paints
        })
        .collect();
    for row in &mut frame.rows {
        if !in_pane(row, selection.area) {
            continue;
        }
        let index = usize::from(row.area.row + row.row - selection.area.row);
        for (paint, ranges) in &selected[index] {
            let original = &selection.frame.rows[*paint];
            if original.area == row.area
                && original.row == row.row
                && original.geometry == row.geometry
                && (Arc::ptr_eq(&original.text, &row.text)
                    || original.text.styled.text() == row.text.styled.text())
            {
                row.selection.extend(ranges.iter().cloned());
            }
        }
        compact_ranges(&mut row.selection);
    }
    Ok(())
}

fn compact_ranges(ranges: &mut Vec<Range<usize>>) {
    ranges.sort_unstable_by_key(|range| (range.start, range.end));
    let mut used = 0;
    for index in 0..ranges.len() {
        if used > 0 && ranges[index].start <= ranges[used - 1].end {
            ranges[used - 1].end = ranges[used - 1].end.max(ranges[index].end);
        } else {
            ranges[used] = ranges[index].clone();
            used += 1;
        }
    }
    ranges.truncate(used);
}

#[cfg(test)]
#[path = "display_selection_tests.rs"]
mod tests;
