//! Current painted-row hit geometry tagged with the exact original body rendering, if any.

use std::sync::Arc;

use norn::session_view::BodyRef;

use crate::app::viewport::ViewAnchor;
use crate::render::layout::Rect;
use crate::render::retained_markdown::RenderedMarkdown;
use crate::render::retained_text::TextRow;

/// Header/loading/continuation chrome can anchor a viewport but has no body mapping.
#[derive(Clone)]
pub(in crate::app) struct HitRow {
    pub area: Rect,
    pub row: u16,
    pub anchor: ViewAnchor,
    pub body: Option<BodyRef>,
    pub text: Arc<RenderedMarkdown>,
    pub geometry: TextRow,
}

impl HitRow {
    pub fn contains(&self, column: u16, row: u16) -> bool {
        row == self.area.row + self.row
            && column >= self.area.column
            && column < self.area.column.saturating_add(self.area.width)
    }

    /// A glyph is hit as one whole grapheme, including a wide glyph's second cell.
    pub fn displayed_offset(&self, column: u16) -> usize {
        self.geometry
            .hit(usize::from(column.saturating_sub(self.area.column)))
    }
}

/// Highlight only ranges proved by the current original body and actual cached map.
pub(super) fn selection_ranges(
    state: &crate::app::state::AppState,
    item: &norn::session_view::ItemId,
    reference: Option<&BodyRef>,
    mapped: &RenderedMarkdown,
    visible: std::ops::Range<usize>,
) -> Vec<std::ops::Range<usize>> {
    use crate::render::retained_markdown::SourceMapping;
    let Some(selection) = &state.screen.selection else {
        return Vec::new();
    };
    if reference != Some(selection.reference())
        || state.screen.selection_item.as_ref() != Some(item)
        || crate::app::view_actions::selected_text(state).is_err()
    {
        return Vec::new();
    }
    let selected = selection.range();
    let first = mapped
        .spans
        .partition_point(|span| span.display.end <= visible.start);
    mapped.spans[first..]
        .iter()
        .take_while(|span| span.display.start < visible.end)
        .filter_map(|span| {
            let original = match &span.source {
                SourceMapping::Exact { original } | SourceMapping::Transformed { original } => {
                    original
                }
                SourceMapping::Generated => return None,
            };
            let start = original.start.max(selected.start);
            let end = original.end.min(selected.end);
            if start >= end {
                return None;
            }
            Some(match span.source {
                SourceMapping::Exact { .. } => {
                    (span.display.start + start - original.start)
                        ..(span.display.start + end - original.start)
                }
                _ => span.display.clone(),
            })
        })
        .collect()
}
