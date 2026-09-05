//! Displayed-byte, Unicode and cell geometry regression cases for retained rows.

use std::num::NonZeroUsize;

use super::{
    AtomKind, StyleSpan, StyledText, TextAttribute, TextAttributes, TextError, TextLayout, TextRow,
    TextStyle,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rows(
    text: &StyledText,
    columns: usize,
    tab_width: usize,
) -> Result<Vec<TextRow>, Box<dyn std::error::Error>> {
    let tab = NonZeroUsize::new(tab_width).ok_or("test requires explicit positive tab width")?;
    match text.layout(columns, tab)? {
        TextLayout::Rows(rows) => Ok(rows),
        TextLayout::NoPaint => Err("test requires nonzero columns".into()),
    }
}

fn plain(text: &str) -> Result<StyledText, TextError> {
    StyledText::new(text.to_owned(), Vec::new())
}

#[test]
fn preserves_empty_hard_lines_and_trailing_spaces() -> TestResult {
    for (source, expected) in [("", vec![""]), ("a\n\nb \n", vec!["a", "", "b ", ""])] {
        let text = plain(source)?;
        let layout = rows(&text, 20, 4)?;
        let actual: Vec<&str> = layout.iter().map(|row| &text.text()[row.bytes()]).collect();
        assert_eq!(actual, expected);
    }
    Ok(())
}

#[test]
fn greedy_wrap_preserves_spaces_and_exact_full_final_row() -> TestResult {
    let text = plain("alpha beta gamma")?;
    let layout = rows(&text, 10, 4)?;
    assert_eq!(layout.len(), 2);
    assert_eq!(&text.text()[layout[0].bytes()], "alpha beta");
    assert_eq!(&text.text()[layout[1].bytes()], " gamma");
    assert_eq!(rows(&plain("abcd")?, 4, 4)?.len(), 1);
    Ok(())
}

#[test]
fn zwj_combining_variation_and_flags_are_atomic_across_styles() -> TestResult {
    for cluster in ["👩‍💻", "e\u{301}", "✈️", "🇦🇺"] {
        let split = cluster.chars().next().ok_or("empty fixture")?.len_utf8();
        let spans = vec![
            StyleSpan {
                range: 0..split,
                style: TextStyle {
                    attributes: TextAttributes::default().with(TextAttribute::Bold),
                    ..TextStyle::default()
                },
            },
            StyleSpan {
                range: split..cluster.len(),
                style: TextStyle {
                    attributes: TextAttributes::default().with(TextAttribute::Italic),
                    ..TextStyle::default()
                },
            },
        ];
        let text = StyledText::new(cluster.to_owned(), spans.clone())?;
        let layout = rows(&text, 10, 4)?;
        assert_eq!(layout[0].atoms().len(), 1);
        assert_eq!(layout[0].atoms()[0].bytes, 0..cluster.len());
        assert_eq!(text.spans(), spans);
        assert!(matches!(
            layout[0].column_for(split),
            Err(TextError::InvalidBoundary { .. })
        ));
        assert_eq!(layout[0].hit(0), 0);
    }
    Ok(())
}

#[test]
fn cjk_tiny_width_is_unpaintable_then_revealed_after_resize() -> TestResult {
    let text = plain("界x")?;
    let narrow = rows(&text, 1, 4)?;
    assert_eq!(narrow.len(), 2);
    assert_eq!(narrow[0].atoms()[0].kind, AtomKind::Unpaintable);
    assert_eq!(narrow[0].atoms()[0].bytes, 0..3);
    assert_eq!(narrow[0].width(), 1);
    let wide = rows(&text, 3, 4)?;
    assert_eq!(wide.len(), 1);
    assert_eq!(wide[0].atoms()[0].kind, AtomKind::Glyph);
    assert_eq!(wide[0].atoms()[0].width, 2);
    assert_eq!(text.text(), "界x");
    Ok(())
}

#[test]
fn tabs_use_explicit_stops_and_recompute_after_wrap() -> TestResult {
    let text = plain("abc\tx")?;
    let four = rows(&text, 4, 4)?;
    assert_eq!(four[0].atoms()[3].width, 1);
    assert_eq!(four[0].atoms()[3].kind, AtomKind::Tab);
    assert_eq!(&text.text()[four[1].bytes()], "x");
    let narrow = rows(&text, 3, 4)?;
    assert_eq!(narrow[1].atoms()[0].kind, AtomKind::Unpaintable);
    assert_eq!(narrow[1].atoms()[0].width, 3);
    let two = rows(&plain("a\tb")?, 10, 2)?;
    assert_eq!(two[0].width(), 3);
    let clipped = four[0].clip(3, 1)?;
    assert_eq!(clipped[0].bytes, 3..4);
    Ok(())
}

#[test]
fn clipping_either_half_of_wide_glyph_never_slices_its_bytes() -> TestResult {
    let text = plain("a界b")?;
    let layout = rows(&text, 10, 4)?;
    let row = &layout[0];
    for start in [1, 2] {
        let clipped = row.clip(start, 1)?;
        assert_eq!(clipped.len(), 1);
        assert_eq!(clipped[0].bytes, 1..4);
        assert_eq!(clipped[0].width, 1);
        assert_eq!(clipped[0].column, 0);
        assert_eq!(clipped[0].kind, AtomKind::Unpaintable);
    }
    assert_eq!(row.clip(1, 2)?[0].kind, AtomKind::Glyph);
    assert!(row.clip(99, 2)?.is_empty());
    assert!(row.clip(0, 0)?.is_empty());
    assert_eq!(row.bytes(), 0..5);
    assert!(matches!(
        row.clip(usize::MAX, 1),
        Err(TextError::GeometryOverflow)
    ));
    Ok(())
}

#[test]
fn cell_hit_uses_displayed_byte_boundaries_and_full_row_end() -> TestResult {
    let layout = rows(&plain("a界b ")?, 20, 4)?;
    let row = &layout[0];
    for (cell, byte) in [(0, 0), (1, 1), (2, 1), (3, 4), (4, 5), (5, 6), (99, 6)] {
        assert_eq!(row.hit(cell), byte);
    }
    for (byte, cell) in [(0, 0), (1, 1), (4, 3), (5, 4), (6, 5)] {
        assert_eq!(row.column_for(byte)?, cell);
    }
    assert!(row.column_for(2).is_err());
    assert!(row.column_for(7).is_err());
    Ok(())
}

#[test]
fn zero_columns_and_zero_cell_graphemes_preserve_identity() -> TestResult {
    let text = plain("\u{200b}x\u{200b}")?;
    let tab = NonZeroUsize::new(4).ok_or("positive tab fixture")?;
    assert_eq!(text.layout(0, tab)?, TextLayout::NoPaint);
    let layout = rows(&text, 1, 4)?;
    assert_eq!(layout.len(), 1);
    assert_eq!(layout[0].atoms().len(), 3);
    assert_eq!(layout[0].hit(0), 3);
    assert_eq!(layout[0].hit(1), text.text().len());
    assert_eq!(layout[0].column_for(0)?, 0);
    assert_eq!(layout[0].column_for(3)?, 0);
    Ok(())
}

#[test]
fn malformed_style_ranges_and_controls_are_typed() {
    let style = TextStyle::default();
    for ranges in [
        vec![(0, 0)],
        vec![(0, 5)],
        vec![(0, 2)],
        vec![(3, 4), (0, 3)],
        vec![(0, 3), (1, 4)],
    ] {
        let spans = ranges
            .into_iter()
            .map(|(start, end)| StyleSpan {
                range: start..end,
                style,
            })
            .collect();
        assert!(matches!(
            StyledText::new("界x".to_owned(), spans),
            Err(TextError::InvalidSpan { .. })
        ));
    }
    for control in [
        '\x1b', '\r', '\0', '\u{7f}', '\u{061c}', '\u{200e}', '\u{202e}', '\u{2066}',
    ] {
        assert!(matches!(
            plain(&format!("ok{control}")),
            Err(TextError::Control { offset: 2 })
        ));
    }
    assert!(plain("tab\tline\nnext").is_ok());
}

#[test]
fn every_unicode_atom_is_bounded_and_source_ranges_survive_clipping() -> TestResult {
    let text = plain("a界👩‍💻e\u{301}\t ✈️🇦🇺\nlast ")?;
    for columns in 1..=12 {
        for tab in [1, 2, 4, 8] {
            for row in rows(&text, columns, tab)? {
                assert!(row.width() <= columns);
                for atom in row.atoms() {
                    assert!(text.text().is_char_boundary(atom.bytes.start));
                    assert!(text.text().is_char_boundary(atom.bytes.end));
                    assert!(atom.column + atom.width <= columns);
                    assert_eq!(row.column_for(atom.bytes.start)?, atom.column);
                }
                for start in 0..=columns {
                    for atom in row.clip(start, columns - start)? {
                        assert!(atom.column + atom.width <= columns - start);
                        assert!(
                            row.atoms()
                                .iter()
                                .any(|original| original.bytes == atom.bytes)
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
