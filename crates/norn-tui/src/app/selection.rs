//! Original-byte selections bound to an exact body revision; no clipboard, file or runtime effects.

use std::ops::Range;

use norn::session_view::{BodyOrigin, BodyRef, ViewSource};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

use crate::render::retained_markdown::{
    BoundaryAffinity, MarkdownError, RenderedMarkdown, SourceBoundary,
};

/// The owner's currently loaded, contiguous original prefix for this exact body.
/// The prefix begins at byte zero, providing Unicode boundary context. This
/// borrowed input neither loads nor retains another copy of the body text.
/// The owner supplies the reference currently accepted for the selected item;
/// presence of an older revision in a cache alone does not establish currency.
#[derive(Clone, Copy)]
pub(super) struct OriginalBody<'a> {
    reference: &'a BodyRef,
    text: &'a str,
    complete: bool,
}

impl<'a> OriginalBody<'a> {
    /// `complete` means the entire named revision is loaded, even if a later
    /// streaming revision may eventually replace it.
    pub const fn new(reference: &'a BodyRef, text: &'a str, complete: bool) -> Self {
        Self {
            reference,
            text,
            complete,
        }
    }
}

/// A render cache entry tagged with the body revision that produced its mapping.
/// The caller supplies that original job/cache tag, never today's replacement tag.
#[derive(Clone, Copy)]
pub(super) struct MappedBody<'a> {
    reference: &'a BodyRef,
    rendered: &'a RenderedMarkdown,
}

impl<'a> MappedBody<'a> {
    pub const fn new(reference: &'a BodyRef, rendered: &'a RenderedMarkdown) -> Self {
        Self {
            reference,
            rendered,
        }
    }
}

/// Failures keep the selected identity and offsets visible without quoting body data.
#[derive(Debug, thiserror::Error)]
pub(super) enum SelectionError {
    #[error("selection source {selected:?} differs from current source {current:?}")]
    SourceChanged {
        selected: Box<ViewSource>,
        current: Box<ViewSource>,
    },
    #[error("selected body {selected:?} differs from current body {current:?}")]
    BodyChanged {
        selected: Box<BodyRef>,
        current: Box<BodyRef>,
    },
    #[error("render mapping for body {mapped:?} does not describe current body {current:?}")]
    MappingChanged {
        mapped: Box<BodyRef>,
        current: Box<BodyRef>,
    },
    #[error("selected body {reference:?} is not currently loaded")]
    Unavailable { reference: Box<BodyRef> },
    #[error("display byte {offset} in body {reference:?} has no original source")]
    Generated {
        reference: Box<BodyRef>,
        offset: usize,
    },
    #[error("display byte {offset} in body {reference:?} cannot be mapped: {source}")]
    Mapping {
        reference: Box<BodyRef>,
        offset: usize,
        source: MarkdownError,
    },
    #[error("transformed source interval {range:?} in body {reference:?} is invalid")]
    InvalidInterval {
        reference: Box<BodyRef>,
        range: Range<usize>,
    },
    #[error("body {reference:?} has only {loaded} loaded bytes; selection needs byte {offset}")]
    OutsideLoaded {
        reference: Box<BodyRef>,
        offset: usize,
        loaded: usize,
    },
    #[error("body {reference:?} needs more source context after loaded byte {offset}")]
    IncompleteBoundary {
        reference: Box<BodyRef>,
        offset: usize,
    },
    #[error("original byte {offset} in body {reference:?} splits a grapheme")]
    OriginalBoundary {
        reference: Box<BodyRef>,
        offset: usize,
    },
    #[error("original byte {offset} in body {reference:?} lacks Unicode context: {context:?}")]
    BoundaryContext {
        reference: Box<BodyRef>,
        offset: usize,
        context: GraphemeIncomplete,
    },
}

/// Endpoints retain source intervals, never screen cells or wrap-inserted newlines.
/// An exact endpoint is an empty interval; a transformed endpoint covers the
/// entire responsible original span. No interior transformed position is invented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Selection {
    reference: BodyRef,
    anchor: Range<usize>,
    focus: Range<usize>,
}

impl Selection {
    /// Explicit original-byte selection for a full body or an original-range search result.
    /// Hidden Markdown delimiters remain selectable without inventing a display inverse.
    pub fn from_original(
        source: &ViewSource,
        original: OriginalBody<'_>,
        range: Range<usize>,
    ) -> Result<Self, SelectionError> {
        check_source(original.reference, source)?;
        if range.start > range.end {
            return Err(SelectionError::InvalidInterval {
                reference: Box::new(original.reference.clone()),
                range,
            });
        }
        boundary(original, range.start)?;
        boundary(original, range.end)?;
        Ok(Self {
            reference: original.reference.clone(),
            anchor: range.start..range.start,
            focus: range.end..range.end,
        })
    }

    pub fn start(
        source: &ViewSource,
        original: OriginalBody<'_>,
        mapped: MappedBody<'_>,
        displayed_offset: usize,
        affinity: BoundaryAffinity,
    ) -> Result<Self, SelectionError> {
        check_source(original.reference, source)?;
        let anchor = endpoint(original, mapped, displayed_offset, affinity)?;
        Ok(Self {
            reference: original.reference.clone(),
            focus: anchor.clone(),
            anchor,
        })
    }

    /// Failed extension leaves the original selection intact for a named stale
    /// or partial state. Only explicit reselection changes its body capability.
    pub fn extend(
        &mut self,
        source: &ViewSource,
        original: OriginalBody<'_>,
        mapped: MappedBody<'_>,
        displayed_offset: usize,
        affinity: BoundaryAffinity,
    ) -> Result<(), SelectionError> {
        self.validate_body(source, original)?;
        let retained = self.range();
        boundary(original, retained.start)?;
        boundary(original, retained.end)?;
        let focus = endpoint(original, mapped, displayed_offset, affinity)?;
        self.focus = focus;
        Ok(())
    }

    pub const fn reference(&self) -> &BodyRef {
        &self.reference
    }

    /// Ordered original-byte interval, independent of drag direction or geometry.
    pub fn range(&self) -> Range<usize> {
        self.anchor.start.min(self.focus.start)..self.anchor.end.max(self.focus.end)
    }

    /// Borrow exactly the original selected bytes after revalidating source,
    /// revision, loaded coverage and Unicode boundaries. Hard newlines survive;
    /// display wrapping and generated chrome never enter the returned string.
    /// This is not clipboard sanitization: original control bytes remain data for
    /// the separately authorized copy/export consumer to handle explicitly.
    pub fn read<'a>(
        &self,
        source: &ViewSource,
        original: Option<OriginalBody<'a>>,
    ) -> Result<&'a str, SelectionError> {
        check_source(&self.reference, source)?;
        let original = original.ok_or_else(|| SelectionError::Unavailable {
            reference: Box::new(self.reference.clone()),
        })?;
        self.validate_body(source, original)?;
        let range = self.range();
        boundary(original, range.start)?;
        boundary(original, range.end)?;
        original
            .text
            .get(range.clone())
            .ok_or_else(|| SelectionError::InvalidInterval {
                reference: Box::new(self.reference.clone()),
                range,
            })
    }

    fn validate_body(
        &self,
        source: &ViewSource,
        original: OriginalBody<'_>,
    ) -> Result<(), SelectionError> {
        check_source(&self.reference, source)?;
        check_source(original.reference, source)?;
        if original.reference == &self.reference {
            Ok(())
        } else {
            Err(SelectionError::BodyChanged {
                selected: Box::new(self.reference.clone()),
                current: Box::new(original.reference.clone()),
            })
        }
    }
}

fn endpoint(
    original: OriginalBody<'_>,
    mapped: MappedBody<'_>,
    offset: usize,
    affinity: BoundaryAffinity,
) -> Result<Range<usize>, SelectionError> {
    if original.reference != mapped.reference {
        return Err(SelectionError::MappingChanged {
            mapped: Box::new(mapped.reference.clone()),
            current: Box::new(original.reference.clone()),
        });
    }
    let point = mapped
        .rendered
        .source_boundary(offset, affinity)
        .map_err(|source| SelectionError::Mapping {
            reference: Box::new(original.reference.clone()),
            offset,
            source,
        })?;
    let range = match point {
        SourceBoundary::Exact { original_offset } => original_offset..original_offset,
        SourceBoundary::Transformed {
            original: range, ..
        } => {
            if range.start >= range.end {
                return Err(SelectionError::InvalidInterval {
                    reference: Box::new(original.reference.clone()),
                    range,
                });
            }
            range
        }
        SourceBoundary::Generated => {
            return Err(SelectionError::Generated {
                reference: Box::new(original.reference.clone()),
                offset,
            });
        }
    };
    boundary(original, range.start)?;
    boundary(original, range.end)?;
    Ok(range)
}

fn boundary(original: OriginalBody<'_>, offset: usize) -> Result<(), SelectionError> {
    let reference = || Box::new(original.reference.clone());
    if offset > original.text.len() {
        return Err(SelectionError::OutsideLoaded {
            reference: reference(),
            offset,
            loaded: original.text.len(),
        });
    }
    if !original.text.is_char_boundary(offset) {
        return Err(SelectionError::OriginalBoundary {
            reference: reference(),
            offset,
        });
    }
    if offset != 0 && offset == original.text.len() && !original.complete {
        return Err(SelectionError::IncompleteBoundary {
            reference: reference(),
            offset,
        });
    }
    match GraphemeCursor::new(offset, original.text.len(), true).is_boundary(original.text, 0) {
        Ok(true) => Ok(()),
        Ok(false) => Err(SelectionError::OriginalBoundary {
            reference: reference(),
            offset,
        }),
        Err(context) => Err(SelectionError::BoundaryContext {
            reference: reference(),
            offset,
            context,
        }),
    }
}

fn check_source(reference: &BodyRef, current: &ViewSource) -> Result<(), SelectionError> {
    let selected = match reference.origin() {
        BodyOrigin::Committed { cursor, .. } => cursor.source(),
        BodyOrigin::Provisional { key, .. } => &key.source,
        BodyOrigin::Local { source, .. } => source,
    };
    if selected == current {
        Ok(())
    } else {
        Err(SelectionError::SourceChanged {
            selected: Box::new(selected.clone()),
            current: Box::new(current.clone()),
        })
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
