//! Absolute full-screen painting of prepared styled rows; no history reads or semantic reduction.

use std::io::{self, Write};
use std::sync::Arc;

use super::layout::{Layout, Rect, UpperLayout};
use super::retained_markdown::RenderedMarkdown;
use super::retained_text::{AtomKind, TextAttribute, TextRow, TextStyle};
use crate::TuiError;
use crate::terminal::caps::TerminalCaps;

/// One already prepared row, with no terminal escape text in its content.
pub struct PaintRow {
    /// Bounding pane or overlay.
    pub area: Rect,
    /// Row offset within the bounding area.
    pub row: u16,
    /// Retained displayed text and ordered styles.
    pub text: Arc<RenderedMarkdown>,
    /// Whole-grapheme geometry into `text`.
    pub geometry: TextRow,
    /// Explicit selected-row emphasis.
    pub selected: bool,
    /// Display byte ranges selected from the exact original body, excluding generated chrome.
    pub selection: Vec<std::ops::Range<usize>>,
    /// Composer rows use the input surface background.
    pub composer: bool,
}

/// One coherent screen snapshot, built before terminal writes begin.
pub struct Frame {
    /// Geometry calculated by the shared layout owner.
    pub layout: Layout,
    /// Visible content only, in painting order.
    pub rows: Vec<PaintRow>,
    /// Zero-based cursor, absent when focus is outside the composer.
    pub cursor: Option<(u16, u16)>,
}

impl Frame {
    /// Encode one complete frame with bounded coordinates and no native scrolling.
    pub fn encode(&self, caps: &TerminalCaps) -> Result<Vec<u8>, TuiError> {
        if self.layout == Layout::NoPaint {
            return Ok(Vec::new());
        }
        let (columns, lines) = match self.layout {
            Layout::Ready { composer, .. } => (
                composer.width,
                composer
                    .row
                    .checked_add(composer.height)
                    .ok_or(TuiError::FrameBounds)?,
            ),
            Layout::ResizeRequired { area } => (area.width, area.height),
            Layout::NoPaint => return Ok(Vec::new()),
        };
        if self
            .cursor
            .is_some_and(|(column, row)| column >= columns || row >= lines)
        {
            return Err(TuiError::FrameBounds);
        }
        for row in &self.rows {
            if row
                .area
                .column
                .checked_add(row.area.width)
                .is_none_or(|right| right > columns)
                || row
                    .area
                    .row
                    .checked_add(row.area.height)
                    .is_none_or(|bottom| bottom > lines)
            {
                return Err(TuiError::FrameBounds);
            }
        }
        let mut output = Vec::new();
        output.extend_from_slice(b"\x1b[?25l\x1b[0m\x1b[2J");
        if let Layout::Ready { upper, composer } = self.layout {
            fill(&mut output, composer, false, caps)?;
            if let UpperLayout::Split { divider, .. } = upper {
                for row in 0..divider.height {
                    position(&mut output, divider.column, divider.row + row)?;
                    style(&mut output, TextStyle::default(), false, false, false, caps)?;
                    output.extend_from_slice("│".as_bytes());
                }
            }
        }
        for row in &self.rows {
            paint_row(&mut output, row, caps)?;
        }
        if let Some((column, row)) = self.cursor {
            position(&mut output, column, row)?;
            output.extend_from_slice(b"\x1b[?25h");
        }
        Ok(output)
    }
}

fn fill(output: &mut Vec<u8>, area: Rect, composer: bool, caps: &TerminalCaps) -> io::Result<()> {
    style(output, TextStyle::default(), composer, false, false, caps)?;
    for row in 0..area.height {
        position(output, area.column, area.row + row)?;
        output.extend(std::iter::repeat_n(b' ', usize::from(area.width)));
    }
    Ok(())
}

fn position(output: &mut Vec<u8>, column: u16, row: u16) -> io::Result<()> {
    write!(
        output,
        "\x1b[{};{}H",
        u32::from(row) + 1,
        u32::from(column) + 1
    )
}

fn paint_row(output: &mut Vec<u8>, row: &PaintRow, caps: &TerminalCaps) -> Result<(), TuiError> {
    if row.row >= row.area.height {
        return Err(TuiError::FrameBounds);
    }
    let mut current_style = None;
    let mut next_column = None;
    for atom in row.geometry.clip(0, usize::from(row.area.width))? {
        if atom.kind == AtomKind::Invisible {
            continue;
        }
        if next_column != Some(atom.column) {
            let column =
                u16::try_from(atom.column).map_err(|source| TuiError::FrameCoordinate {
                    value: atom.column,
                    source,
                })?;
            position(
                output,
                row.area
                    .column
                    .checked_add(column)
                    .ok_or(TuiError::FrameBounds)?,
                row.area
                    .row
                    .checked_add(row.row)
                    .ok_or(TuiError::FrameBounds)?,
            )?;
        }
        next_column = Some(atom.column + atom.width);
        if matches!(atom.kind, AtomKind::Unpaintable | AtomKind::Tab) {
            style(
                output,
                TextStyle::default(),
                row.composer,
                row.selected,
                row.selection
                    .iter()
                    .any(|range| range.start < atom.bytes.end && atom.bytes.start < range.end),
                caps,
            )?;
            current_style = None;
            output.extend(std::iter::repeat_n(b' ', atom.width));
            continue;
        }
        let highlighted = row
            .selection
            .iter()
            .any(|range| range.start < atom.bytes.end && atom.bytes.start < range.end);
        let mut byte = atom.bytes.start;
        let mut span_index = row
            .text
            .styled
            .spans()
            .partition_point(|span| span.range.end <= byte);
        while byte < atom.bytes.end {
            let span = row.text.styled.spans().get(span_index);
            let (end, selected) = match span {
                Some(span) if span.range.start <= byte => {
                    (span.range.end.min(atom.bytes.end), span.style)
                }
                Some(span) => (span.range.start.min(atom.bytes.end), TextStyle::default()),
                None => (atom.bytes.end, TextStyle::default()),
            };
            if current_style != Some((selected, highlighted)) {
                style(
                    output,
                    selected,
                    row.composer,
                    row.selected,
                    highlighted,
                    caps,
                )?;
                current_style = Some((selected, highlighted));
            }
            let bytes = row
                .text
                .styled
                .text()
                .get(byte..end)
                .ok_or(TuiError::FrameBounds)?;
            output.extend_from_slice(bytes.as_bytes());
            byte = end;
            if span.is_some_and(|span| span.range.end <= byte) {
                span_index += 1;
            }
        }
    }
    Ok(())
}

fn style(
    output: &mut Vec<u8>,
    text: TextStyle,
    composer: bool,
    selected: bool,
    highlighted: bool,
    caps: &TerminalCaps,
) -> io::Result<()> {
    output.extend_from_slice(b"\x1b[0m");
    if composer {
        output.extend_from_slice(b"\x1b[39;49m");
    }
    if caps.true_colour {
        if let Some([red, green, blue]) = text.foreground {
            write!(output, "\x1b[38;2;{red};{green};{blue}m")?;
        }
        if let Some([red, green, blue]) = text.background {
            write!(output, "\x1b[48;2;{red};{green};{blue}m")?;
        }
    } else if let Some([red, green, blue]) = text.foreground {
        let colour =
            crate::render::style::colour_for(termina::style::RgbColor::new(red, green, blue), caps);
        output.extend_from_slice(colour.as_bytes());
    }
    if selected {
        output.extend_from_slice(b"\x1b[1m");
    }
    if highlighted {
        output.extend_from_slice(b"\x1b[7m");
    }
    for (attribute, code) in [
        (TextAttribute::Bold, 1),
        (TextAttribute::Dim, 2),
        (TextAttribute::Italic, 3),
        (TextAttribute::Underline, 4),
        (TextAttribute::Strike, 9),
    ] {
        if text.attributes.contains(attribute) {
            write!(output, "\x1b[{code}m")?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
