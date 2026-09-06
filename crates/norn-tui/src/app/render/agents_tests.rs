//! Agent status allocation and actual retained frame safety at wide, narrow and tiny geometry.

use std::error::Error;
use std::sync::Arc;

use crate::render::layout::{LayoutPolicy, LayoutRequest, SplitPreference, composer_input_area};
use crate::render::retained_markdown::{BoundaryAffinity, SourceBoundary};
use crate::render::retained_text::{TextAttribute, TextStyle};
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
fn allocation_preserves_composer_full_width_and_one_upper_content_row() -> TestResult {
    for (columns, lines) in [(120, 24), (80, 12), (8, 8), (1, 5), (1, 4), (0, 0)] {
        for split in [false, true] {
            let original = layout(columns, lines, split)?;
            let (reduced, area) = allocate(original, 6)?;
            if let (
                Layout::Ready {
                    composer: before, ..
                },
                Layout::Ready {
                    composer: after,
                    upper,
                },
            ) = (original, reduced)
            {
                assert_eq!(before, after);
                assert_eq!(composer_input_area(after).width, columns);
                match upper {
                    UpperLayout::Single { area, .. } => assert!(area.height >= 1),
                    UpperLayout::Split {
                        conversation,
                        changes,
                        divider,
                    } => {
                        assert!(conversation.height >= 1);
                        assert_eq!(conversation.height, changes.height);
                        assert_eq!(conversation.height, divider.height);
                    }
                }
                if let Some(area) = area {
                    assert_eq!(area.row + area.height, before.row);
                    assert_eq!(area.width, before.width);
                }
            } else {
                assert_eq!(original, reduced);
                assert!(area.is_none());
            }
            let frame = Frame {
                layout: reduced,
                rows: Vec::new(),
                composer: None,
                cursor: None,
            };
            frame.prepare(&TerminalCaps::baseline())?;
        }
    }
    Ok(())
}

#[test]
fn absent_status_leaves_original_layout_byte_for_byte() -> TestResult {
    let original = layout(120, 24, true)?;
    assert_eq!(allocate(original, 0)?, (original, None));
    let mut panel = AgentStatusPanel::new(norn::agent::registry::AgentRegistry::shared());
    let agents = prepare(&mut panel, original, Instant::now(), Utc::now())?;
    let mut frame = Frame {
        layout: agents.layout,
        rows: Vec::new(),
        composer: None,
        cursor: None,
    };
    paint(&agents, &mut frame)?;
    assert!(frame.rows.is_empty());
    Ok(())
}

#[test]
fn geometry_clipping_counts_collapsed_and_newly_hidden_agents() -> TestResult {
    let mut rows = vec![
        status("root"),
        status("one"),
        status("two"),
        status("three"),
        status("four"),
    ];
    rows.push(RetainedAgentRow::overflow(7));
    let fitted = fit_rows(rows, 3);
    assert_eq!(fitted.len(), 3);
    assert_eq!(
        fitted.last().ok_or("missing clipped overflow")?.kind,
        RetainedAgentRowKind::Overflow { count: 10 }
    );
    assert!(
        fitted
            .last()
            .ok_or("missing clipped overflow")?
            .style
            .attributes
            .contains(TextAttribute::Dim)
    );
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
    let (layout, area) = allocate(layout, 1)?;
    let agents = AgentFrame {
        layout,
        next_refresh: None,
        pane_next_refresh: None,
        all_rows: Vec::new(),
        area,
        rows: vec![row],
    };
    let mut frame = Frame {
        layout,
        rows: Vec::new(),
        composer: None,
        cursor: None,
    };
    paint(&agents, &mut frame)?;
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
        next_refresh: None,
        pane_next_refresh: None,
        area: None,
        rows: vec![status("root"), RetainedAgentRow::overflow(8)],
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
    let compact = now + std::time::Duration::from_secs(2);
    let full = now + std::time::Duration::from_secs(1);
    let mut agents = AgentFrame {
        layout: layout(120, 24, true)?,
        next_refresh: Some(compact),
        pane_next_refresh: Some(full),
        area: None,
        rows: Vec::new(),
        all_rows: Vec::new(),
    };
    assert_eq!(agents.refresh_deadline(true), Some(full));
    assert_eq!(agents.refresh_deadline(false), Some(compact));
    agents.layout = layout(40, 24, true)?;
    assert_eq!(agents.refresh_deadline(true), Some(compact));
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
