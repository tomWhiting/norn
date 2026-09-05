//! Grapheme-safe displayed-text rows; styles are data and no terminal bytes are emitted.

use std::num::NonZeroUsize;
use std::ops::Range;
use std::sync::Arc;

use unicode_segmentation::{GraphemeIncomplete, UnicodeSegmentation};
use unicode_width::UnicodeWidthStr;

/// Visual attributes applied by the final frame writer, never escape sequences.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStyle {
    /// Optional explicit RGB foreground.
    pub foreground: Option<[u8; 3]>,
    /// Optional explicit RGB background.
    pub background: Option<[u8; 3]>,
    /// Independent visual attributes represented as a compact typed set.
    pub attributes: TextAttributes,
}

/// An independent visual attribute; combinations do not alter source text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAttribute {
    /// Strong emphasis.
    Bold,
    /// Muted intensity.
    Dim,
    /// Italic emphasis.
    Italic,
    /// Underlined text.
    Underline,
    /// Struck-through text.
    Strike,
}

impl TextAttribute {
    const fn bit(self) -> u8 {
        match self {
            Self::Bold => 1,
            Self::Dim => 2,
            Self::Italic => 4,
            Self::Underline => 8,
            Self::Strike => 16,
        }
    }
}

/// Compact set of independently combinable attributes with no unchecked bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextAttributes(u8);

impl TextAttributes {
    /// Add one declared attribute to this set.
    #[must_use]
    pub const fn with(self, attribute: TextAttribute) -> Self {
        Self(self.0 | attribute.bit())
    }

    /// Whether the named attribute belongs to this set.
    #[must_use]
    pub const fn contains(self, attribute: TextAttribute) -> bool {
        self.0 & attribute.bit() != 0
    }
}

/// One nonempty, nonoverlapping style interval in displayed UTF-8 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleSpan {
    /// Displayed byte interval, not original tool-output bytes.
    pub range: Range<usize>,
    /// Style for this interval; gaps use the frame's base style.
    pub style: TextStyle,
}

/// Invalid displayed text or geometry, identified without echoing its contents.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    /// Style interval is empty, unordered, outside text or splits a UTF-8 scalar.
    #[error("invalid display style span {index} at {range:?}")]
    InvalidSpan {
        /// Invalid span's index in the supplied sequence.
        index: usize,
        /// Supplied displayed-byte interval.
        range: Range<usize>,
    },
    /// A terminal or directional control must first be visibly escaped.
    #[error("unescaped display control at byte {offset}")]
    Control {
        /// Offending displayed-byte offset.
        offset: usize,
    },
    /// Requested byte is not a complete grapheme edge in this row.
    #[error("display byte {offset} is not a grapheme edge in row {row:?}")]
    InvalidBoundary {
        /// Requested displayed-byte offset.
        offset: usize,
        /// Row's actual displayed-byte interval.
        row: Range<usize>,
    },
    /// A grapheme query lacked the context required to establish its boundary.
    #[error("display byte {offset} has incomplete grapheme context: {context:?}")]
    IncompleteGrapheme {
        /// Displayed byte whose boundary could not be established.
        offset: usize,
        /// Original typed query failure; shared to preserve this error's `Clone` API.
        context: Arc<GraphemeIncomplete>,
    },
    /// Explicit geometry would overflow addressable coordinates.
    #[error("display cell geometry overflows")]
    GeometryOverflow,
}

/// One owned displayed string; source/style offsets survive every geometry change.
#[derive(Clone, Debug)]
pub struct StyledText {
    text: String,
    spans: Vec<StyleSpan>,
}

/// How an atomic source interval can be painted at the requested geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomKind {
    /// A whole printable extended grapheme.
    Glyph,
    /// A tab painted as cells, never emitted as a terminal tab character.
    Tab,
    /// Zero-cell source content retained for selection and identity.
    Invisible,
    /// The original glyph remains available but cannot fit this allocation.
    Unpaintable,
}

/// An indivisible grapheme and its original displayed-byte identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextAtom {
    /// Exact source bytes. Styles within this range remain in `StyledText::spans`.
    pub bytes: Range<usize>,
    /// Cell offset within its row, or within the clipping interval after clipping.
    pub column: usize,
    /// Allocated cells, always bounded by the row or clipping extent.
    pub width: usize,
    /// Rendering instruction that preserves whole graphemes.
    pub kind: AtomKind,
}

/// A hard or wrapped line with its complete displayed-byte interval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRow {
    bytes: Range<usize>,
    atoms: Vec<TextAtom>,
    width: usize,
}

/// Explicit zero-width nonpaint or rows from the same retained displayed string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextLayout {
    /// No segmentation is needed until the target has columns.
    NoPaint,
    /// Visible rows, including real empty hard lines.
    Rows(Vec<TextRow>),
}

impl StyledText {
    /// Validate once before layout; controls must already have visible escaping.
    ///
    /// # Errors
    /// Reports the offending style index/range or control byte offset.
    pub fn new(text: String, spans: Vec<StyleSpan>) -> Result<Self, TextError> {
        let mut previous = 0;
        for (index, span) in spans.iter().enumerate() {
            let range = &span.range;
            if range.start < previous
                || range.start >= range.end
                || range.end > text.len()
                || !text.is_char_boundary(range.start)
                || !text.is_char_boundary(range.end)
            {
                return Err(TextError::InvalidSpan {
                    index,
                    range: range.clone(),
                });
            }
            previous = range.end;
        }
        for (offset, character) in text.char_indices() {
            let directional = matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
            if (character.is_control() && !matches!(character, '\n' | '\t')) || directional {
                return Err(TextError::Control { offset });
            }
        }
        Ok(Self { text, spans })
    }

    /// Original displayed string, independent of wrapping and clipping.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Exact ordered styles, including changes inside an indivisible grapheme.
    /// A frame may change attributes inside an atom, but must clip it as a whole.
    #[must_use]
    pub fn spans(&self) -> &[StyleSpan] {
        &self.spans
    }

    /// Greedy whole-grapheme wrap using explicit columns and tab stops.
    ///
    /// # Errors
    /// Refuses geometry whose cell arithmetic cannot be represented.
    pub fn layout(&self, columns: usize, tab_width: NonZeroUsize) -> Result<TextLayout, TextError> {
        if columns == 0 {
            return Ok(TextLayout::NoPaint);
        }
        let mut rows = Vec::new();
        let mut row = TextRow::empty(0);
        for (offset, grapheme) in self.text.grapheme_indices(true) {
            if grapheme == "\n" {
                rows.push(row);
                row = TextRow::empty(offset + 1);
                continue;
            }
            let mut width = atom_width(grapheme, row.width, tab_width);
            if width > 0 && row.width > 0 && width > columns - row.width {
                rows.push(row);
                row = TextRow::empty(offset);
                width = atom_width(grapheme, 0, tab_width);
            }
            let kind = if width > columns {
                AtomKind::Unpaintable
            } else if grapheme == "\t" {
                AtomKind::Tab
            } else if width == 0 {
                AtomKind::Invisible
            } else {
                AtomKind::Glyph
            };
            let allocated = width.min(columns);
            let end = offset + grapheme.len();
            row.atoms.push(TextAtom {
                bytes: offset..end,
                column: row.width,
                width: allocated,
                kind,
            });
            row.bytes.end = end;
            row.width = row
                .width
                .checked_add(allocated)
                .ok_or(TextError::GeometryOverflow)?;
        }
        rows.push(row);
        Ok(TextLayout::Rows(rows))
    }
}

impl TextRow {
    const fn empty(offset: usize) -> Self {
        Self {
            bytes: offset..offset,
            atoms: Vec::new(),
            width: 0,
        }
    }

    /// Complete displayed source range, excluding a hard newline delimiter.
    #[must_use]
    pub fn bytes(&self) -> Range<usize> {
        self.bytes.clone()
    }

    /// Atomic layout units in source order.
    #[must_use]
    pub fn atoms(&self) -> &[TextAtom] {
        &self.atoms
    }

    /// Allocated row cells, including blank space for an unpaintable atom.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Map a cell to a canonical displayed-byte edge.
    /// Wide/tab continuation cells choose their atom start; beyond text chooses
    /// row end. Invisible atoms at an edge are skipped to their trailing boundary.
    #[must_use]
    pub fn hit(&self, column: usize) -> usize {
        let index = self
            .atoms
            .partition_point(|atom| atom.column + atom.width <= column);
        self.atoms
            .get(index)
            .map_or(self.bytes.end, |atom| atom.bytes.start)
    }

    /// Map a complete displayed-byte edge to its cell column.
    ///
    /// # Errors
    /// Refuses a byte outside the row or inside a grapheme, including UTF-8 interiors.
    pub fn column_for(&self, offset: usize) -> Result<usize, TextError> {
        if offset == self.bytes.end {
            return Ok(self.width);
        }
        self.atoms
            .binary_search_by_key(&offset, |atom| atom.bytes.start)
            .ok()
            .and_then(|index| self.atoms.get(index))
            .map(|atom| atom.column)
            .ok_or_else(|| TextError::InvalidBoundary {
                offset,
                row: self.bytes.clone(),
            })
    }

    /// Clip cells without splitting glyphs or changing any saved logical range.
    /// A partly visible glyph becomes explicit blank/unpaintable cells. Tabs can
    /// expose a subset of their blank cells while retaining the whole tab range.
    ///
    /// # Errors
    /// Refuses an interval whose end overflows addressable cell coordinates.
    pub fn clip(&self, start: usize, width: usize) -> Result<Vec<TextAtom>, TextError> {
        let end = start
            .checked_add(width)
            .ok_or(TextError::GeometryOverflow)?;
        if width == 0 {
            return Ok(Vec::new());
        }
        let first = self
            .atoms
            .partition_point(|atom| atom.column + atom.width < start);
        let mut result = Vec::new();
        for atom in &self.atoms[first..] {
            if atom.column >= end {
                break;
            }
            let atom_end = atom.column + atom.width;
            if atom_end < start || (atom_end == start && atom.width > 0) {
                continue;
            }
            let left = atom.column.max(start);
            let right = atom_end.min(end);
            let partial = left != atom.column || right != atom_end;
            result.push(TextAtom {
                bytes: atom.bytes.clone(),
                column: left - start,
                width: right - left,
                kind: if partial && atom.kind == AtomKind::Glyph {
                    AtomKind::Unpaintable
                } else {
                    atom.kind
                },
            });
        }
        Ok(result)
    }
}

fn atom_width(grapheme: &str, column: usize, tab_width: NonZeroUsize) -> usize {
    if grapheme == "\t" {
        tab_width.get() - column % tab_width.get()
    } else {
        UnicodeWidthStr::width(grapheme)
    }
}

#[cfg(test)]
#[path = "retained_text_tests.rs"]
mod tests;
