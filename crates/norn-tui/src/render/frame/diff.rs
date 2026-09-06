//! Visible-cell comparison with whole-grapheme updates and no pre-paint screen erasure.

use std::io::{self, Write};
use std::ops::Range;

use crate::TuiError;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Cell {
    Blank,
    Glyph { width: u16, bytes: Range<usize> },
    Continuation { start: u16 },
}

/// One prepared visible surface, retained only after its terminal publication succeeds.
#[derive(Debug, PartialEq, Eq)]
pub struct PreparedFrame {
    columns: u16,
    cells: Vec<Vec<Cell>>,
    cursor: Option<(u16, u16)>,
    bytes: Vec<u8>,
}

impl PreparedFrame {
    pub(super) fn new(columns: u16, lines: u16, cursor: Option<(u16, u16)>) -> Self {
        Self {
            columns,
            cells: vec![vec![Cell::Blank; usize::from(columns)]; usize::from(lines)],
            cursor,
            bytes: Vec::new(),
        }
    }

    /// Whether a zero-size terminal has no cells available to paint.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.columns == 0 || self.cells.is_empty()
    }

    pub(super) fn put(
        &mut self,
        column: u16,
        row: u16,
        width: u16,
        bytes: &[u8],
    ) -> Result<(), TuiError> {
        if width == 0 {
            return Err(TuiError::FrameBounds);
        }
        let end = column.checked_add(width).ok_or(TuiError::FrameBounds)?;
        if end > self.columns {
            return Err(TuiError::FrameBounds);
        }
        let cells = self
            .cells
            .get_mut(usize::from(row))
            .ok_or(TuiError::FrameBounds)?;
        // An overlay cannot leave half of an earlier wide glyph behind.
        let clear_start = glyph_range(cells, usize::from(column))?.start;
        let clear_end = glyph_range(cells, usize::from(end) - 1)?.end;
        cells[clear_start..clear_end].fill(Cell::Blank);
        let start = self.bytes.len();
        self.bytes.extend_from_slice(bytes);
        cells[usize::from(column)] = Cell::Glyph {
            width,
            bytes: start..self.bytes.len(),
        };
        for cell in &mut cells[usize::from(column) + 1..usize::from(end)] {
            *cell = Cell::Continuation { start: column };
        }
        Ok(())
    }

    /// Publish a prepared delta and commit its baseline only after write and flush succeed.
    pub fn publish(
        self,
        previous: &mut Option<Self>,
        writer: &mut impl Write,
        synchronized: bool,
    ) -> Result<(), TuiError> {
        let bytes = self.encode_delta(previous.as_ref())?;
        publish(writer, &bytes, synchronized)?;
        *previous = Some(self);
        Ok(())
    }

    /// Encode only changed visible spans against the last successfully published surface.
    /// A changed terminal size invalidates the baseline; no screen erase is emitted.
    pub fn encode_delta(&self, previous: Option<&Self>) -> Result<Vec<u8>, TuiError> {
        if self.is_empty() {
            return Ok(Vec::new());
        }
        let previous = previous
            .filter(|old| old.columns == self.columns && old.cells.len() == self.cells.len());
        let mut output = Vec::new();
        for (row, cells) in self.cells.iter().enumerate() {
            let old = previous.map(|frame| (frame.cells[row].as_slice(), frame.bytes.as_slice()));
            let mut column = 0;
            while let Some(range) = changed_range(cells, &self.bytes, old, column)? {
                if output.is_empty() {
                    output.extend_from_slice(b"\x1b[?25l");
                }
                column = range.end;
                encode_row(&mut output, cells, &self.bytes, row, range)?;
            }
        }
        let cursor_changed = previous.is_none_or(|old| old.cursor != self.cursor);
        if !output.is_empty() || cursor_changed {
            output.extend_from_slice(b"\x1b[0m");
            if let Some((column, row)) = self.cursor {
                super::position(&mut output, column, row)?;
                output.extend_from_slice(b"\x1b[?25h");
            } else {
                output.extend_from_slice(b"\x1b[?25l");
            }
        }
        Ok(output)
    }
}

fn glyph_range(cells: &[Cell], column: usize) -> Result<Range<usize>, TuiError> {
    match cells.get(column).ok_or(TuiError::FrameBounds)? {
        Cell::Blank => Ok(column..column + 1),
        Cell::Glyph { width, .. } => Ok(column..column + usize::from(*width)),
        Cell::Continuation { start } => match cells.get(usize::from(*start)) {
            Some(Cell::Glyph { width, .. }) => {
                Ok(usize::from(*start)..usize::from(*start) + usize::from(*width))
            }
            _ => Err(TuiError::FrameBounds),
        },
    }
}

fn changed_range(
    cells: &[Cell],
    arena: &[u8],
    previous: Option<(&[Cell], &[u8])>,
    column: usize,
) -> Result<Option<Range<usize>>, TuiError> {
    if column >= cells.len() {
        return Ok(None);
    }
    let Some((old, old_arena)) = previous else {
        return Ok(Some(column..cells.len()));
    };
    let Some(offset) = cells[column..]
        .iter()
        .zip(&old[column..])
        .position(|(new, old)| !same_cell(new, arena, old, old_arena))
    else {
        return Ok(None);
    };
    let start = column + offset;
    let end = cells[start..]
        .iter()
        .zip(&old[start..])
        .position(|(new, old)| same_cell(new, arena, old, old_arena))
        .map_or(cells.len(), |offset| start + offset);
    let mut range = start..end;
    loop {
        let start = glyph_range(cells, range.start)?
            .start
            .min(glyph_range(old, range.start)?.start);
        let end = glyph_range(cells, range.end - 1)?
            .end
            .max(glyph_range(old, range.end - 1)?.end);
        if start == range.start && end == range.end {
            return Ok(Some(range));
        }
        range = start..end;
    }
}

fn same_cell(new: &Cell, arena: &[u8], old: &Cell, old_arena: &[u8]) -> bool {
    match (new, old) {
        (
            Cell::Glyph { width, bytes },
            Cell::Glyph {
                width: old_width,
                bytes: old_bytes,
            },
        ) => width == old_width && arena[bytes.clone()] == old_arena[old_bytes.clone()],
        _ => new == old,
    }
}

fn encode_row(
    output: &mut Vec<u8>,
    cells: &[Cell],
    arena: &[u8],
    row: usize,
    range: Range<usize>,
) -> Result<(), TuiError> {
    write!(output, "\x1b[{};{}H", row + 1, range.start + 1)?;
    let mut column = range.start;
    while column < range.end {
        match &cells[column] {
            Cell::Blank => {
                output.extend_from_slice(b"\x1b[0m");
                while column < range.end && cells[column] == Cell::Blank {
                    output.push(b' ');
                    column += 1;
                }
            }
            Cell::Glyph { width, bytes } => {
                output.extend_from_slice(arena.get(bytes.clone()).ok_or(TuiError::FrameBounds)?);
                column += usize::from(*width);
            }
            Cell::Continuation { .. } => return Err(TuiError::FrameBounds),
        }
    }
    Ok(())
}

/// Publish one already encoded update in a single write/flush transaction.
/// A failed write or flush attempts sync/cursor recovery and preserves the first error.
pub(crate) fn publish(writer: &mut impl Write, body: &[u8], synchronized: bool) -> io::Result<()> {
    if body.is_empty() {
        return Ok(());
    }
    let mut bytes = Vec::new();
    if synchronized {
        bytes.extend_from_slice(b"\x1b[?2026h");
    }
    bytes.extend_from_slice(body);
    if synchronized {
        bytes.extend_from_slice(b"\x1b[?2026l");
    }
    let result = writer.write_all(&bytes).and_then(|()| writer.flush());
    if let Err(error) = result {
        let recovery = writer
            .write_all(b"\x1b[?2026l\x1b[0m\x1b[?25h")
            .and_then(|()| writer.flush());
        if let Err(recovery) = recovery {
            tracing::error!(%recovery, "frame publication recovery also failed");
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
