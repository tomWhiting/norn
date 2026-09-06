//! Prepared-frame safety and whole-grapheme behavior across actual layout geometries.

use super::*;
use crate::render::layout::{LayoutPolicy, LayoutRequest, SplitPreference, UpperPane};
use crate::render::retained_markdown::render_plain;
use crate::render::retained_text::TextLayout;
use std::num::NonZeroUsize;

#[derive(Default)]
struct PrintableText(String);

impl vte::Perform for PrintableText {
    fn print(&mut self, character: char) {
        self.0.push(character);
    }
}

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
    assert!(!bytes.contains(&b'\x07'));
    assert!(!bytes.contains(&b'\r'));
    let mut decoded = PrintableText::default();
    vte::Parser::new().advance(&mut decoded, &bytes);
    assert!(decoded.0.contains("payload"));
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

#[test]
fn release_visible_surface_work_samples() -> Result<(), Box<dyn std::error::Error>> {
    for (columns, lines) in [(120u16, 40u16), (240, 80)] {
        let text = Arc::new(render_plain(&"x".repeat(usize::from(columns)))?);
        let TextLayout::Rows(geometry) = text
            .styled
            .layout(usize::from(columns), NonZeroUsize::MIN)?
        else {
            return Err("sample geometry missing".into());
        };
        let area = Rect {
            column: 0,
            row: 0,
            width: columns,
            height: lines - 1,
        };
        let composer = Rect {
            row: lines - 1,
            height: 1,
            ..area
        };
        let mut frame = Frame {
            layout: Layout::Ready {
                upper: UpperLayout::Single {
                    pane: UpperPane::Conversation,
                    area,
                },
                composer,
            },
            rows: (0..lines)
                .map(|row| PaintRow {
                    area: Rect {
                        height: lines,
                        ..area
                    },
                    row,
                    text: Arc::clone(&text),
                    geometry: geometry[0].clone(),
                    selected: false,
                    selection: Vec::new(),
                    composer: row == lines - 1,
                })
                .collect(),
            cursor: Some((0, lines - 1)),
        };
        let caps = TerminalCaps::baseline();
        let old = frame.prepare(&caps)?;
        let started = std::time::Instant::now();
        let samples = 100;
        for _ in 0..samples {
            let prepared = std::hint::black_box(&frame).prepare(&caps)?;
            assert!(prepared.encode_delta(Some(&old))?.is_empty());
        }
        let unchanged = started.elapsed();
        let changed = Arc::new(render_plain(&format!(
            "{}y",
            "x".repeat(usize::from(columns) - 1)
        ))?);
        frame.rows.last_mut().ok_or("sample composer missing")?.text = changed;
        let started = std::time::Instant::now();
        let mut bytes = 0;
        for _ in 0..samples {
            let prepared = std::hint::black_box(&frame).prepare(&caps)?;
            let delta = prepared.encode_delta(Some(&old))?;
            bytes = delta.len();
            assert!(!delta.windows(4).any(|window| window == b"\x1b[2J"));
            assert_eq!(delta, format!("\x1b[?25l\x1b[{lines};{columns}H\x1b[0m\x1b[39;49my\x1b[0m\x1b[{lines};1H\x1b[?25h").as_bytes());
        }
        println!(
            "NUI_FRAME_SAMPLE columns={columns} rows={lines} samples={samples} unchanged_total_us={} changed_total_us={} changed_bytes={bytes} unchanged_bytes=0",
            unchanged.as_micros(),
            started.elapsed().as_micros()
        );
    }
    Ok(())
}
