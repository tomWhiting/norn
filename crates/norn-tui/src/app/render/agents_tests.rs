//! Agent status allocation and actual retained frame safety at wide, narrow and tiny geometry.

use std::error::Error;
use std::sync::Arc;

use crate::agents::status_line::RetainedAgentRowKind;
use crate::render::layout::{LayoutPolicy, LayoutRequest, SplitPreference};
use crate::render::retained_markdown::{BoundaryAffinity, SourceBoundary};
use crate::render::retained_text::TextStyle;
use crate::terminal::caps::TerminalCaps;
use uuid::Uuid;

use super::*;

type TestResult = Result<(), Box<dyn Error>>;

fn layout(
    columns: u16,
    rows: u16,
    split: bool,
) -> Result<Layout, crate::render::layout::LayoutError> {
    Layout::calculate(
        LayoutRequest {
            columns,
            rows,
            requested_composer_rows: 1,
            changes_open: split,
            split: SplitPreference::default(),
            active_upper_pane: UpperPane::Conversation,
        },
        LayoutPolicy::default(),
    )
}

fn status(text: &str) -> RetainedAgentRow {
    RetainedAgentRow {
        kind: RetainedAgentRowKind::Agent {
            id: Uuid::nil(),
            parent_id: None,
        },
        text: text.to_owned(),
        style: TextStyle {
            foreground: Some([95, 215, 95]),
            ..TextStyle::default()
        },
    }
}

#[test]
fn agent_snapshot_never_reduces_conversation_or_composer_geometry() -> TestResult {
    let mut panel = AgentStatusPanel::new(norn::agent::registry::AgentRegistry::shared());
    for (columns, lines) in [(120, 24), (80, 12), (8, 8), (1, 5), (1, 4), (0, 0)] {
        for split in [false, true] {
            let original = layout(columns, lines, split)?;
            let agents = prepare(&mut panel, original, Instant::now(), Utc::now());
            assert_eq!(agents.layout, original);
            assert_eq!(agents.refresh_deadline(false), None);
        }
    }
    Ok(())
}

#[test]
fn control_payloads_are_visible_and_generated_without_terminal_or_body_authority() -> TestResult {
    let row = status("worker\n\t\u{1b}]52;c;payload\u{7}\u{202e}");
    let text = display_row(&row, 120)?;
    assert!(
        !text
            .styled
            .text()
            .contains(['\n', '\t', '\u{1b}', '\u{7}', '\u{202e}'])
    );
    assert!(text.styled.text().contains("payload"));
    assert_eq!(
        text.source_boundary(0, BoundaryAffinity::After)?,
        SourceBoundary::Generated
    );
    assert_eq!(
        text.styled
            .spans()
            .first()
            .ok_or("missing style")?
            .style
            .foreground,
        Some([95, 215, 95])
    );
    let layout = layout(120, 24, false)?;
    let area = Rect {
        column: 0,
        row: 0,
        width: 120,
        height: 1,
    };
    let agents = AgentFrame {
        layout,
        pane_next_refresh: None,
        all_rows: vec![row],
    };
    let mut frame = Frame {
        layout,
        rows: Vec::new(),
        composer: None,
        cursor: None,
    };
    paint_pane(&agents, &mut frame, area, 0)?;
    let output = frame.encode(&TerminalCaps::baseline())?;
    assert!(!output.windows(5).any(|window| window == b"\x1b]52;"));
    assert_eq!(frame.rows.len(), 1);
    assert!(
        frame
            .rows
            .iter()
            .all(|row| !row.composer && !row.selected && row.selection.is_empty())
    );
    Ok(())
}

#[test]
fn clipping_keeps_complete_combining_and_wide_graphemes() -> TestResult {
    let row = status("e\u{301}界tail");
    assert_eq!(display_row(&row, 4)?.styled.text(), "e\u{301}界…");
    assert_eq!(display_row(&row, 1)?.styled.text(), "…");
    assert_eq!(display_row(&status("界tail"), 2)?.styled.text(), "…");
    assert!(display_row(&row, 0)?.styled.text().is_empty());
    let text = Arc::new(display_row(&row, 4)?);
    assert_eq!(super::super::layout_rows(&text.styled, 4)?.len(), 1);
    Ok(())
}

#[test]
fn agents_pane_uses_full_typed_snapshot_and_explicit_row_scroll() -> TestResult {
    let layout = layout(120, 24, true)?;
    let Layout::Ready {
        upper: UpperLayout::Split { changes, .. },
        ..
    } = layout
    else {
        return Err("expected wide split".into());
    };
    let agents = AgentFrame {
        layout,
        pane_next_refresh: None,
        all_rows: (0..9)
            .map(|index| status(&format!("agent-{index}")))
            .collect(),
    };
    let mut frame = Frame {
        layout,
        rows: Vec::new(),
        composer: None,
        cursor: None,
    };
    let area = Rect {
        height: 2,
        ..changes
    };
    paint_pane(&agents, &mut frame, area, 6)?;
    assert_eq!(frame.rows.len(), 2);
    assert_eq!(
        frame
            .rows
            .first()
            .ok_or("missing first visible agent")?
            .text
            .styled
            .text(),
        "agent-6"
    );
    assert_eq!(
        frame
            .rows
            .get(1)
            .ok_or("missing second visible agent")?
            .text
            .styled
            .text(),
        "agent-7"
    );
    assert!(
        frame
            .rows
            .iter()
            .all(|row| row.area == area && row.selection.is_empty())
    );
    frame.prepare(&TerminalCaps::baseline())?;
    Ok(())
}

#[test]
fn full_list_deadline_is_used_only_for_visible_agents_content() -> TestResult {
    let now = Instant::now();
    let full = now + std::time::Duration::from_secs(1);
    let mut agents = AgentFrame {
        layout: layout(120, 24, true)?,
        pane_next_refresh: Some(full),
        all_rows: Vec::new(),
    };
    assert_eq!(agents.refresh_deadline(true), Some(full));
    assert_eq!(agents.refresh_deadline(false), None);
    agents.layout = layout(40, 24, true)?;
    assert_eq!(agents.refresh_deadline(true), None);
    if let Layout::Ready {
        upper: UpperLayout::Single { area, .. },
        composer,
    } = agents.layout
    {
        agents.layout = Layout::Ready {
            upper: UpperLayout::Single {
                pane: UpperPane::Changes,
                area,
            },
            composer,
        };
    } else {
        return Err("expected narrow single pane".into());
    }
    assert_eq!(agents.refresh_deadline(true), Some(full));
    Ok(())
}
