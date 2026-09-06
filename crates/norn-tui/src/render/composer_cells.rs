//! Validated Iridium cell payloads for Norn's existing frame owner; no terminal or text layout.

use std::io::{self, Write as _};
use std::ops::Range;

use iridium_tui::cell::{Attributes, CellBuffer, CellContent, Color, ColorDepth, Style};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use super::frame::PreparedFrame;
use super::layout::Rect;
use crate::TuiError;
use crate::terminal::caps::TerminalCaps;

struct Glyph {
    column: u16,
    row: u16,
    width: u16,
    bytes: Range<usize>,
}

/// Stage a complete borrowed cell rectangle before changing the prepared frame.
///
/// The caller supplies the actual input rectangle, excluding Norn's chrome. Its
/// dimensions must exactly match the Iridium buffer. Glyph widths, continuation
/// cells and styles are checked across every row before any destination cell is
/// changed. Foreground, background and attributes come only from Iridium cells;
/// the terminal capability determines their colour depth.
pub(crate) fn paint_composer_cells(
    output: &mut PreparedFrame,
    area: Rect,
    cells: &CellBuffer,
    caps: &TerminalCaps,
) -> Result<(), TuiError> {
    validate_extent(output, area, cells)?;
    let depth = if caps.true_colour {
        ColorDepth::TrueColor
    } else {
        ColorDepth::Ansi256
    };
    let mut glyphs = Vec::new();
    let mut bytes = Vec::new();
    let mut text = String::new();
    for row in 0..cells.height() {
        let mut column = 0;
        while column < cells.width() {
            let cell = cells.get(column, row).ok_or(TuiError::FrameBounds)?;
            let CellContent::Grapheme(grapheme) = cell.content() else {
                return Err(TuiError::FrameBounds);
            };
            text.clear();
            grapheme.push_to(&mut text);
            let width = grapheme.width();
            validate_cluster(cells, column, row, &text, width, cell.style())?;
            let start = bytes.len();
            encode_style(&mut bytes, cell.style().degrade(depth))?;
            bytes.extend_from_slice(text.as_bytes());
            glyphs.push(Glyph {
                column: area
                    .column
                    .checked_add(coordinate(column)?)
                    .ok_or(TuiError::FrameBounds)?,
                row: area
                    .row
                    .checked_add(coordinate(row)?)
                    .ok_or(TuiError::FrameBounds)?,
                width: coordinate(width)?,
                bytes: start..bytes.len(),
            });
            column += width;
        }
    }
    for glyph in glyphs {
        output.put(glyph.column, glyph.row, glyph.width, &bytes[glyph.bytes])?;
    }
    Ok(())
}

fn validate_extent(output: &PreparedFrame, area: Rect, cells: &CellBuffer) -> Result<(), TuiError> {
    let (columns, rows) = output.dimensions();
    if cells.width() != usize::from(area.width)
        || cells.height() != usize::from(area.height)
        || area
            .column
            .checked_add(area.width)
            .is_none_or(|right| right > columns)
        || area
            .row
            .checked_add(area.height)
            .is_none_or(|bottom| usize::from(bottom) > rows)
    {
        return Err(TuiError::FrameBounds);
    }
    Ok(())
}

fn validate_cluster(
    cells: &CellBuffer,
    column: usize,
    row: usize,
    text: &str,
    width: usize,
    style: Style,
) -> Result<(), TuiError> {
    let mut clusters = text.graphemes(true);
    if width == 0
        || text.chars().any(char::is_control)
        || clusters.next() != Some(text)
        || clusters.next().is_some()
        || text.width() != width
    {
        return Err(TuiError::FrameBounds);
    }
    let end = column.checked_add(width).ok_or(TuiError::FrameBounds)?;
    if end > cells.width() {
        return Err(TuiError::FrameBounds);
    }
    for tail in column + 1..end {
        let cell = cells.get(tail, row).ok_or(TuiError::FrameBounds)?;
        if !cell.is_continuation() || cell.style() != style {
            return Err(TuiError::FrameBounds);
        }
    }
    Ok(())
}

fn coordinate(value: usize) -> Result<u16, TuiError> {
    u16::try_from(value).map_err(|source| TuiError::FrameCoordinate { value, source })
}

fn encode_style(output: &mut Vec<u8>, style: Style) -> io::Result<()> {
    // Each glyph is self-contained, so a default cell also clears an earlier
    // selection/background. These are protocol codes, not a composer theme.
    output.extend_from_slice(b"\x1b[0m");
    encode_colour(output, style.foreground, 38)?;
    encode_colour(output, style.background, 48)?;
    for (attribute, code) in [
        (Attributes::BOLD, 1),
        (Attributes::DIM, 2),
        (Attributes::ITALIC, 3),
        (Attributes::UNDERLINE, 4),
        (Attributes::REVERSE, 7),
        (Attributes::STRIKETHROUGH, 9),
    ] {
        if style.attributes.contains(attribute) {
            write!(output, "\x1b[{code}m")?;
        }
    }
    Ok(())
}

fn encode_colour(output: &mut Vec<u8>, colour: Color, code: u8) -> io::Result<()> {
    match colour {
        Color::Default => Ok(()),
        Color::Indexed(index) => write!(output, "\x1b[{code};5;{index}m"),
        Color::Rgb(red, green, blue) => write!(output, "\x1b[{code};2;{red};{green};{blue}m"),
    }
}

#[cfg(test)]
#[path = "composer_cells_tests.rs"]
mod tests;
