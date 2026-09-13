//! Agent status tree occupies only the explicit Agents pane; no extra composer or conversation rows.

use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::TuiError;
use crate::agents::status_line::{AgentStatusPanel, RetainedAgentRow, RetainedAgentRowKind};
use crate::app::agent_pane::AgentHit;
use crate::render::frame::{Frame, PaintRow};
use crate::render::layout::{Layout, Rect, UpperLayout, UpperPane};
use crate::render::retained_markdown::{
    RenderedMarkdown, SourceDisplaySpan, SourceMapping, render_plain,
};
use crate::render::retained_text::{AtomKind, StyleSpan, StyledText};
use uuid::Uuid;

/// One agent snapshot and its allocation; the composer rectangle is unchanged.
pub(super) struct AgentFrame {
    pub layout: Layout,
    pub pane_next_refresh: Option<Instant>,
    all_rows: Vec<RetainedAgentRow>,
}

impl AgentFrame {
    /// Full-list ages matter only while the explicit Agents content is visible.
    pub(super) const fn refresh_deadline(&self, agents_selected: bool) -> Option<Instant> {
        let visible = matches!(
            self.layout,
            Layout::Ready {
                upper: UpperLayout::Split { .. }
                    | UpperLayout::Single {
                        pane: UpperPane::Changes,
                        ..
                    },
                ..
            }
        );
        if agents_selected && visible {
            self.pane_next_refresh
        } else {
            None
        }
    }
}

pub(super) fn prepare(
    panel: &mut AgentStatusPanel,
    layout: Layout,
    now: Instant,
    now_utc: DateTime<Utc>,
) -> AgentFrame {
    let snapshot = panel.retained_snapshot(now, now_utc);
    AgentFrame {
        layout,
        pane_next_refresh: snapshot.pane_next_refresh,
        all_rows: snapshot.all_rows,
    }
}

/// Explicit side-pane list from the very same frame snapshot; no target access is implied.
pub(super) fn paint_pane(
    agents: &AgentFrame,
    frame: &mut Frame,
    area: Rect,
    scroll: usize,
    selected: Option<Uuid>,
) -> Result<Vec<AgentHit>, TuiError> {
    if agents.all_rows.is_empty() {
        super::push_text(frame, "Agents · no registered agents", area, false, false)?;
        return Ok(Vec::new());
    }
    paint_rows(&agents.all_rows, frame, area, scroll, selected)
}

fn paint_rows(
    rows: &[RetainedAgentRow],
    frame: &mut Frame,
    area: Rect,
    scroll: usize,
    selected: Option<Uuid>,
) -> Result<Vec<AgentHit>, TuiError> {
    let mut hits = Vec::new();
    for (index, row) in rows
        .iter()
        .skip(scroll)
        .take(usize::from(area.height))
        .enumerate()
    {
        let text = Arc::new(display_row(row, area.width)?);
        if let Some(geometry) = super::layout_rows(&text.styled, area.width)?
            .into_iter()
            .next()
        {
            let offset = u16::try_from(index).map_err(|source| TuiError::FrameCoordinate {
                value: index,
                source,
            })?;
            let id = match row.kind {
                RetainedAgentRowKind::Agent { id, .. } => Some(id),
                RetainedAgentRowKind::Overflow { .. } => None,
            };
            if let Some(id) = id {
                hits.push(AgentHit {
                    id,
                    area: Rect {
                        row: area.row + offset,
                        height: 1,
                        ..area
                    },
                });
            }
            frame.rows.push(PaintRow {
                area,
                row: offset,
                text,
                geometry,
                selected: id.is_some() && id == selected,
                selection: Vec::new(),
                composer: false,
            });
        }
    }
    Ok(hits)
}

fn display_row(row: &RetainedAgentRow, columns: u16) -> Result<RenderedMarkdown, TuiError> {
    // Newlines/tabs are visible status text, never extra rows or cursor movement.
    let single_line = row.text.replace('\n', "\\n").replace('\t', "\\t");
    let safe = render_plain(&single_line)?;
    let wrapped = super::layout_rows(&safe.styled, columns)?;
    let text = if columns == 0 {
        String::new()
    } else if wrapped.len() > 1 {
        let prefix = super::layout_rows(&safe.styled, columns - 1)?
            .into_iter()
            .next();
        let end = prefix.as_ref().map_or(0, |line| {
            line.atoms()
                .iter()
                .take_while(|atom| atom.kind != AtomKind::Unpaintable)
                .last()
                .map_or(0, |atom| atom.bytes.end)
        });
        let mut text = safe
            .styled
            .text()
            .get(..end)
            .ok_or(TuiError::FrameBounds)?
            .to_owned();
        text.push('…');
        text
    } else {
        safe.styled.text().to_owned()
    };
    let spans = if text.is_empty() {
        Vec::new()
    } else {
        vec![StyleSpan {
            range: 0..text.len(),
            style: row.style,
        }]
    };
    let source = if text.is_empty() {
        Vec::new()
    } else {
        vec![SourceDisplaySpan {
            display: 0..text.len(),
            source: SourceMapping::Generated,
        }]
    };
    Ok(RenderedMarkdown {
        styled: StyledText::new(text, spans)?,
        spans: source,
    })
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
