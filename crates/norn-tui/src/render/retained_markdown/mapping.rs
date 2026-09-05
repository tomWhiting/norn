//! Source/display provenance and grapheme boundary queries; no Markdown parsing or terminal I/O.

use std::ops::Range;
use std::sync::Arc;

use unicode_segmentation::GraphemeCursor;

use super::attribute;
use crate::render::retained_text::{StyleSpan, StyledText, TextAttribute, TextError, TextStyle};
use crate::render::syntax::SyntaxError;

/// Relationship of displayed bytes to the supplied original body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceMapping {
    /// Byte-for-byte identical text; only validated grapheme edges may be mapped.
    Exact {
        /// Original byte-for-byte content interval.
        original: Range<usize>,
    },
    /// Derived text; no interior displayed byte is claimed as an original offset.
    Transformed {
        /// Entire original interval responsible for these derived display bytes.
        original: Range<usize>,
    },
    /// Display chrome with no corresponding original content bytes.
    Generated,
}

/// One contiguous displayed range and its precise provenance category.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceDisplaySpan {
    /// Displayed UTF-8 bytes, after visible control escaping.
    pub display: Range<usize>,
    /// Original body relationship; never a filesystem or execution authority.
    pub source: SourceMapping,
}

/// Which neighboring span owns a shared displayed boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryAffinity {
    /// Prefer the span ending at this boundary.
    Before,
    /// Prefer the span beginning at this boundary.
    After,
}

/// A source location, or explicit evidence that no exact location can be inferred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceBoundary {
    /// Proven byte-for-byte original boundary.
    Exact {
        /// Proven original UTF-8 byte boundary.
        original_offset: usize,
    },
    /// Selectable transformed interval; callers must not invent an interior offset.
    Transformed {
        /// Responsible original interval, not an inferred interior position.
        original: Range<usize>,
        /// Complete corresponding displayed interval.
        display: Range<usize>,
    },
    /// Generated chrome, including an empty render, has no original position.
    Generated,
}

/// Safe retained text plus the provenance of every displayed byte.
#[derive(Clone, Debug)]
pub struct RenderedMarkdown {
    /// Direct styles consumed by grapheme-aware layout and the frame writer.
    pub styled: StyledText,
    /// Ordered, nonoverlapping and exhaustive displayed-byte provenance.
    pub spans: Vec<SourceDisplaySpan>,
}

/// Mapping/render failures report offsets and categories, never rejected body contents.
#[derive(Debug, thiserror::Error)]
pub enum MarkdownError {
    /// The caller requested an interior grapheme or an out-of-bounds byte.
    #[error("Markdown display byte {offset} is not a complete grapheme boundary")]
    Boundary {
        /// Rejected displayed byte offset.
        offset: usize,
    },
    /// A parser range or stack contradicted its supplied source.
    #[error("Markdown parser structure is inconsistent at original range {range:?}")]
    Structure {
        /// Original range at which the contradiction was found.
        range: Range<usize>,
    },
    /// Ordered numbering cannot be represented without inventing a marker.
    #[error("Markdown list numbering overflows at original range {range:?}")]
    ListOverflow {
        /// Original list item whose number cannot be represented.
        range: Range<usize>,
    },
    /// Final styled text failed its own safety validation.
    #[error(transparent)]
    Text(#[from] TextError),
    /// Direct syntax highlighting refused the code block; no partial output is returned.
    #[error(transparent)]
    Syntax(#[from] SyntaxError),
}

impl RenderedMarkdown {
    /// Map a displayed grapheme edge with explicit ownership at shared boundaries.
    ///
    /// # Errors
    /// Rejects invalid or partial grapheme boundaries. Transformed text remains a range.
    pub fn source_boundary(
        &self,
        offset: usize,
        affinity: BoundaryAffinity,
    ) -> Result<SourceBoundary, MarkdownError> {
        let text = self.styled.text();
        if !text.is_char_boundary(offset) {
            return Err(MarkdownError::Boundary { offset });
        }
        let boundary = GraphemeCursor::new(offset, text.len(), true)
            .is_boundary(text, 0)
            .map_err(|context| TextError::IncompleteGrapheme {
                offset,
                context: Arc::new(context),
            })?;
        if !boundary {
            return Err(MarkdownError::Boundary { offset });
        }
        let index = match affinity {
            BoundaryAffinity::Before => {
                self.spans.partition_point(|span| span.display.end < offset)
            }
            BoundaryAffinity::After => self
                .spans
                .partition_point(|span| span.display.end <= offset),
        };
        let Some(span) = self.spans.get(index) else {
            return Ok(SourceBoundary::Generated);
        };
        if offset < span.display.start {
            return Ok(SourceBoundary::Generated);
        }
        Ok(match &span.source {
            SourceMapping::Exact { original } => {
                if original.end.checked_sub(original.start)
                    != span.display.end.checked_sub(span.display.start)
                    || offset > span.display.end
                {
                    return Err(MarkdownError::Structure {
                        range: original.clone(),
                    });
                }
                SourceBoundary::Exact {
                    original_offset: original
                        .start
                        .checked_add(offset - span.display.start)
                        .ok_or_else(|| MarkdownError::Structure {
                            range: original.clone(),
                        })?,
                }
            }
            SourceMapping::Transformed { original } => SourceBoundary::Transformed {
                original: original.clone(),
                display: span.display.clone(),
            },
            SourceMapping::Generated => SourceBoundary::Generated,
        })
    }
}

impl super::Builder<'_> {
    pub(super) fn emit(&mut self, text: &str, source: &SourceMapping, style: TextStyle) {
        for (offset, character) in text.char_indices() {
            if character != '\n' && (self.text.is_empty() || self.text.ends_with('\n')) {
                for _ in 0..self.quote_depth {
                    self.put(
                        "│ ",
                        SourceMapping::Generated,
                        attribute(style, TextAttribute::Dim),
                    );
                }
            }
            let original = match &source {
                SourceMapping::Exact { original } => {
                    original.start + offset..original.start + offset + character.len_utf8()
                }
                SourceMapping::Transformed { original } => original.clone(),
                SourceMapping::Generated => 0..0,
            };
            let escaped = (character.is_control() && !matches!(character, '\n' | '\t'))
                || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
            if escaped {
                let mapped = if matches!(source, SourceMapping::Generated) {
                    SourceMapping::Generated
                } else {
                    SourceMapping::Transformed { original }
                };
                self.put(&character.escape_unicode().to_string(), mapped, style);
            } else {
                let mapped = match &source {
                    SourceMapping::Exact { .. } => SourceMapping::Exact { original },
                    SourceMapping::Transformed { .. } => SourceMapping::Transformed { original },
                    SourceMapping::Generated => SourceMapping::Generated,
                };
                self.put(character.encode_utf8(&mut [0; 4]), mapped, style);
            }
        }
    }

    fn put(&mut self, text: &str, source: SourceMapping, style: TextStyle) {
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        match self.styles.last_mut() {
            Some(span) if span.style == style => span.range.end = end,
            _ => self.styles.push(StyleSpan {
                range: start..end,
                style,
            }),
        }
        if let Some(span) = self.spans.last_mut() {
            let merged = match (&mut span.source, &source) {
                (
                    SourceMapping::Exact { original: left },
                    SourceMapping::Exact { original: right },
                ) if left.end == right.start => {
                    left.end = right.end;
                    true
                }
                (
                    SourceMapping::Transformed { original: left },
                    SourceMapping::Transformed { original: right },
                ) => left == right,
                (SourceMapping::Generated, SourceMapping::Generated) => true,
                _ => false,
            };
            if merged {
                span.display.end = end;
                return;
            }
        }
        self.spans.push(SourceDisplaySpan {
            display: start..end,
            source,
        });
    }

    pub(super) fn truncate(&mut self, end: usize) {
        self.text.truncate(end);
        self.styles.retain(|span| span.range.start < end);
        if let Some(span) = self.styles.last_mut() {
            span.range.end = span.range.end.min(end);
        }
        self.spans.retain(|span| span.display.start < end);
        if let Some(span) = self.spans.last_mut() {
            span.display.end = span.display.end.min(end);
        }
    }

    pub(super) fn finish(mut self) -> Result<RenderedMarkdown, MarkdownError> {
        while self.text.ends_with('\n')
            && self
                .spans
                .last()
                .is_some_and(|span| matches!(span.source, SourceMapping::Generated))
        {
            self.truncate(self.text.len() - 1);
        }
        Ok(RenderedMarkdown {
            styled: StyledText::new(self.text, self.styles)?,
            spans: self.spans,
        })
    }
}
