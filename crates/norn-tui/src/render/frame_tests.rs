//! Prepared-frame safety and whole-grapheme behavior across actual layout geometries.

use super::*;
use crate::render::layout::{LayoutPolicy, LayoutRequest, SplitPreference, UpperPane};
use crate::render::retained_markdown::render_plain;
use crate::render::retained_text::TextLayout;
use std::num::NonZeroUsize;

#[test]
fn untrusted_controls_are_never_emitted_as_terminal_commands()
-> Result<(), Box<dyn std::error::Error>> {
    let text = Arc::new(render_plain("hello\u{1b}]52;c;payload\u{7}\rworld")?);
    let TextLayout::Rows(rows) = text.styled.layout(80, NonZeroUsize::MIN)? else {
        return Err("missing rows".into());
    };
    let area = Rect {
        column: 0,
        row: 0,
        width: 80,
        height: 23,
    };
    let frame = Frame {
        layout: Layout::Ready {
            upper: UpperLayout::Single {
                pane: UpperPane::Conversation,
                area,
            },
            composer: Rect {
                row: 23,
                height: 1,
                ..area
            },
        },
        rows: rows
            .into_iter()
            .enumerate()
            .map(|(index, geometry)| {
                Ok(PaintRow {
                    area,
                    row: u16::try_from(index)?,
                    text: Arc::clone(&text),
                    geometry,
                    selected: false,
                    selection: Vec::new(),
                    composer: false,
                })
            })
            .collect::<Result<_, std::num::TryFromIntError>>()?,
        cursor: Some((0, 23)),
    };
    let bytes = frame.encode(&TerminalCaps::baseline())?;
    assert!(!bytes.windows(5).any(|window| window == b"\x1b]52;"));
    assert!(String::from_utf8(bytes)?.contains("payload"));
    Ok(())
}

#[test]
fn tiny_and_zero_geometry_never_invent_extra_screen_rows() -> Result<(), Box<dyn std::error::Error>>
{
    for (columns, rows, panel_height) in [
        (100, 24, 4),
        (80, 24, 4),
        (100, 12, 4),
        (40, 4, 1),
        (1, 1, 0),
        (0, 0, 0),
    ] {
        let layout = Layout::calculate(
            LayoutRequest {
                columns,
                rows,
                requested_composer_rows: 1,
                changes_open: true,
                split: SplitPreference::default(),
                active_upper_pane: UpperPane::Conversation,
            },
            LayoutPolicy::default(),
        )?;
        let frame = Frame {
            layout,
            rows: Vec::new(),
            cursor: None,
        };
        let bytes = frame.encode(&TerminalCaps::baseline())?;
        if columns == 0 || rows == 0 {
            assert!(bytes.is_empty());
        }
        if let Layout::Ready { composer, .. } = layout {
            assert_eq!(composer.row + composer.height, rows);
            assert_eq!(composer.height, panel_height);
            let input = crate::render::layout::composer_input_area(composer);
            assert_eq!(input.height, 1);
            assert_eq!(input.width, columns);
            assert!(input.row >= composer.row);
            assert!(input.row + input.height <= composer.row + composer.height);
        }
        assert!(!bytes.contains(&b'\n'));
    }
    Ok(())
}

#[test]
fn selected_original_range_highlights_whole_grapheme_only() -> Result<(), Box<dyn std::error::Error>>
{
    let text = Arc::new(crate::render::retained_markdown::render_plain("A👩‍💻Z")?);
    let crate::render::retained_text::TextLayout::Rows(rows) =
        text.styled.layout(10, std::num::NonZeroUsize::MIN)?
    else {
        return Err("fixture has no row".into());
    };
    let area = Rect {
        column: 0,
        row: 0,
        width: 10,
        height: 1,
    };
    let frame = Frame {
        layout: Layout::Ready {
            upper: UpperLayout::Single {
                pane: UpperPane::Conversation,
                area,
            },
            composer: Rect { row: 1, ..area },
        },
        rows: vec![PaintRow {
            area,
            row: 0,
            text,
            geometry: rows.first().ok_or("fixture row missing")?.clone(),
            selected: false,
            selection: std::iter::once(1..12).collect(),
            composer: false,
        }],
        cursor: None,
    };
    let encoded = String::from_utf8(frame.encode(&TerminalCaps::baseline())?)?;
    assert!(
        encoded.contains("A\x1b[0m\x1b[7m👩‍💻\x1b[0mZ"),
        "exact selected glyph style boundary missing"
    );
    Ok(())
}
