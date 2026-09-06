//! User-message colour remains uniform across original lines and terminal wrapping.

use super::*;
use crate::render::layout::Layout;
use crate::terminal::caps::TerminalCaps;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn every_user_line_keeps_its_colour_and_original_source_mapping() -> TestResult {
    for content in [
        "first\nsecond\nthird",
        "first\r\nsecond",
        "\nsecond\n",
        "🙂 e\u{301}\n宽\t界",
        "one\n**literal user text**\nthree",
        "first\n\u{1b}]52;c;untrusted\u{7}",
        "",
    ] {
        let rendered = input_text(content)?;
        let plain = crate::render::retained_markdown::render_plain(content)?;
        assert_eq!(rendered.styled.text(), plain.styled.text());
        assert_eq!(rendered.spans, plain.spans);
        for (byte, character) in rendered.styled.text().char_indices() {
            if character.is_whitespace() {
                continue;
            }
            let span = rendered
                .styled
                .spans()
                .iter()
                .find(|span| span.range.contains(&byte))
                .ok_or("a user-message character lost its style")?;
            assert_eq!(span.style.foreground, Some([80, 160, 220]));
        }
    }
    Ok(())
}

#[test]
fn encoded_user_rows_keep_the_same_colour_after_newlines_and_wrapping() -> TestResult {
    for (content, width, expected_rows) in
        [("first\nsecond\nthird", 12, 3), ("abcdef\nghijkl", 3, 4)]
    {
        let text = input_text(content)?;
        let rows = super::super::layout_rows(&text.styled, width)?;
        assert_eq!(rows.len(), expected_rows);
        let area = Rect {
            column: 0,
            row: 0,
            width,
            height: u16::try_from(rows.len())?,
        };
        let mut frame = Frame {
            layout: Layout::ResizeRequired { area },
            rows: Vec::new(),
            cursor: None,
        };
        for (row, geometry) in rows.into_iter().enumerate() {
            frame.rows.push(PaintRow {
                area,
                row: u16::try_from(row)?,
                text: Arc::clone(&text),
                geometry,
                selected: false,
                selection: Vec::new(),
                composer: false,
            });
        }
        let mut caps = TerminalCaps::baseline();
        caps.true_colour = true;
        let encoded = frame.encode(&caps)?;
        let mut observed = UserColours::default();
        vte::Parser::new().advance(&mut observed, &encoded);
        assert_eq!(
            observed.printed.len(),
            content.chars().filter(|c| !c.is_whitespace()).count()
        );
        assert!(
            observed
                .printed
                .iter()
                .all(|colour| *colour == Some([80, 160, 220])),
            "all visible user characters need the same colour, regardless of escape batching"
        );
    }
    Ok(())
}

#[derive(Default)]
struct UserColours {
    foreground: Option<[u8; 3]>,
    printed: Vec<Option<[u8; 3]>>,
}

impl vte::Perform for UserColours {
    fn print(&mut self, character: char) {
        if !character.is_whitespace() {
            self.printed.push(self.foreground);
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore || !intermediates.is_empty() || action != 'm' {
            return;
        }
        let codes: Vec<u16> = params
            .iter()
            .flat_map(|values| values.iter().copied())
            .collect();
        let mut remaining = codes.as_slice();
        while let Some((code, rest)) = remaining.split_first() {
            match (*code, rest) {
                (0 | 39, _) => self.foreground = None,
                (38, [2, red, green, blue, following @ ..]) => {
                    self.foreground = match (
                        u8::try_from(*red),
                        u8::try_from(*green),
                        u8::try_from(*blue),
                    ) {
                        (Ok(red), Ok(green), Ok(blue)) => Some([red, green, blue]),
                        _ => None,
                    };
                    remaining = following;
                    continue;
                }
                _ => {}
            }
            remaining = rest;
        }
    }
}
