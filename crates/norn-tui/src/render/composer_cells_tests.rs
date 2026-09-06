//! Iridium-to-Norn cell adaptation, whole-cluster safety and pre-mutation refusal.

use iridium_tui::cell::{Attributes, CellBuffer, Color, Grapheme, Style, WriteOutcome};

use super::paint_composer_cells;
use crate::TuiError;
use crate::render::frame::PreparedFrame;
use crate::render::layout::Rect;
use crate::terminal::caps::TerminalCaps;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn true_colour() -> TerminalCaps {
    TerminalCaps {
        true_colour: true,
        ..TerminalCaps::baseline()
    }
}

fn area(column: u16, row: u16, width: u16, height: u16) -> Rect {
    Rect {
        column,
        row,
        width,
        height,
    }
}

fn set(
    cells: &mut CellBuffer,
    column: usize,
    row: usize,
    text: &str,
    style: Style,
    width: usize,
) -> TestResult {
    let glyph = Grapheme::new(text).ok_or("fixture text is not an Iridium glyph")?;
    assert_eq!(
        cells.set_grapheme(column, row, &glyph, style),
        WriteOutcome::Written(width)
    );
    Ok(())
}

#[test]
fn default_cells_preserve_terminal_background_and_supplied_rectangle() -> TestResult {
    let mut cells = CellBuffer::new(2, 1);
    set(&mut cells, 0, 0, "x", Style::DEFAULT, 1)?;
    let mut output = PreparedFrame::new(6, 3, Some((3, 1)));
    output.put(0, 0, 1, b"\x1b[0mU")?;
    paint_composer_cells(&mut output, area(2, 1, 2, 1), &cells, &true_colour())?;
    let mut expected = PreparedFrame::new(6, 3, Some((3, 1)));
    expected.put(0, 0, 1, b"\x1b[0mU")?;
    expected.put(2, 1, 1, b"\x1b[0mx")?;
    expected.put(3, 1, 1, b"\x1b[0m ")?;
    assert_eq!(output, expected);
    let bytes = output.encode_delta(None)?;
    assert!(!bytes.windows(4).any(|window| window == b"\x1b[48"));
    Ok(())
}

#[test]
fn true_colour_and_every_iridium_attribute_reach_the_same_glyph() -> TestResult {
    let style = Style::new(
        Color::Rgb(12, 34, 56),
        Color::Rgb(65, 43, 21),
        Attributes::ALL,
    );
    let mut cells = CellBuffer::new(1, 1);
    set(&mut cells, 0, 0, "x", style, 1)?;
    let mut output = PreparedFrame::new(1, 1, None);
    paint_composer_cells(&mut output, area(0, 0, 1, 1), &cells, &true_colour())?;
    let mut expected = PreparedFrame::new(1, 1, None);
    expected.put(
        0,
        0,
        1,
        b"\x1b[0m\x1b[38;2;12;34;56m\x1b[48;2;65;43;21m\x1b[1m\x1b[2m\x1b[3m\x1b[4m\x1b[7m\x1b[9mx",
    )?;
    assert_eq!(output, expected);
    Ok(())
}

#[test]
fn baseline_degrades_both_colours_without_losing_selection() -> TestResult {
    let style = Style::new(
        Color::Rgb(255, 0, 0),
        Color::Rgb(0, 0, 0),
        Attributes::REVERSE,
    );
    let mut cells = CellBuffer::new(1, 1);
    set(&mut cells, 0, 0, "x", style, 1)?;
    let mut output = PreparedFrame::new(1, 1, None);
    paint_composer_cells(
        &mut output,
        area(0, 0, 1, 1),
        &cells,
        &TerminalCaps::baseline(),
    )?;
    let mut expected = PreparedFrame::new(1, 1, None);
    expected.put(0, 0, 1, b"\x1b[0m\x1b[38;5;196m\x1b[48;5;16m\x1b[7mx")?;
    assert_eq!(output, expected);
    Ok(())
}

#[test]
fn indexed_colours_and_selected_blank_cells_are_not_rethemed() -> TestResult {
    let style = Style::new(
        Color::Indexed(123),
        Color::Indexed(234),
        Attributes::REVERSE,
    );
    let mut cells = CellBuffer::new(1, 1);
    cells.fill(style);
    for caps in [true_colour(), TerminalCaps::baseline()] {
        let mut output = PreparedFrame::new(1, 1, None);
        paint_composer_cells(&mut output, area(0, 0, 1, 1), &cells, &caps)?;
        let mut expected = PreparedFrame::new(1, 1, None);
        expected.put(0, 0, 1, b"\x1b[0m\x1b[38;5;123m\x1b[48;5;234m\x1b[7m ")?;
        assert_eq!(output, expected);
    }
    Ok(())
}

#[test]
fn complete_combining_cjk_and_zwj_clusters_keep_their_cell_widths() -> TestResult {
    let mut cells = CellBuffer::new(5, 1);
    set(&mut cells, 0, 0, "e\u{301}", Style::DEFAULT, 1)?;
    set(&mut cells, 1, 0, "界", Style::DEFAULT, 2)?;
    set(&mut cells, 3, 0, "👩‍💻", Style::DEFAULT, 2)?;
    let mut output = PreparedFrame::new(7, 2, None);
    paint_composer_cells(&mut output, area(1, 1, 5, 1), &cells, &true_colour())?;
    let mut expected = PreparedFrame::new(7, 2, None);
    expected.put(1, 1, 1, "\x1b[0me\u{301}".as_bytes())?;
    expected.put(2, 1, 2, "\x1b[0m界".as_bytes())?;
    expected.put(4, 1, 2, "\x1b[0m👩‍💻".as_bytes())?;
    assert_eq!(output, expected);
    Ok(())
}

#[test]
fn width_or_height_mismatch_and_extent_overflow_leave_output_unchanged() -> TestResult {
    for rect in [
        area(0, 0, 1, 1),
        area(0, 0, 2, 2),
        area(3, 0, 2, 1),
        area(0, 2, 2, 1),
        area(u16::MAX, 0, 2, 1),
        area(0, u16::MAX, 2, 1),
    ] {
        let cells = CellBuffer::new(2, 1);
        let mut output = PreparedFrame::new(4, 2, Some((0, 0)));
        output.put(0, 0, 1, b"\x1b[0mU")?;
        let mut expected = PreparedFrame::new(4, 2, Some((0, 0)));
        expected.put(0, 0, 1, b"\x1b[0mU")?;
        assert!(matches!(
            paint_composer_cells(&mut output, rect, &cells, &true_colour()),
            Err(TuiError::FrameBounds)
        ));
        assert_eq!(
            output, expected,
            "refused rectangle {rect:?} changed the frame"
        );
    }
    Ok(())
}

#[test]
fn late_multicluster_or_clamped_width_refusal_is_atomic() -> TestResult {
    for invalid in ["ab", "abc"] {
        let mut cells = CellBuffer::new(3, 2);
        set(&mut cells, 0, 0, "x", Style::DEFAULT, 1)?;
        set(&mut cells, 1, 1, invalid, Style::DEFAULT, 2)?;
        let mut output = PreparedFrame::new(3, 2, Some((0, 0)));
        output.put(0, 0, 1, b"\x1b[0mU")?;
        let mut expected = PreparedFrame::new(3, 2, Some((0, 0)));
        expected.put(0, 0, 1, b"\x1b[0mU")?;
        assert!(matches!(
            paint_composer_cells(&mut output, area(0, 0, 3, 2), &cells, &true_colour()),
            Err(TuiError::FrameBounds)
        ));
        assert_eq!(output, expected);
    }
    Ok(())
}

#[test]
fn zero_extent_emits_no_cells_but_still_checks_exact_dimensions() -> TestResult {
    for (width, height) in [(0, 0), (0, 2), (2, 0)] {
        let cells = CellBuffer::new(usize::from(width), usize::from(height));
        let mut output = PreparedFrame::new(3, 3, None);
        paint_composer_cells(
            &mut output,
            area(1, 1, width, height),
            &cells,
            &true_colour(),
        )?;
        assert_eq!(output, PreparedFrame::new(3, 3, None));
    }
    Ok(())
}

#[test]
fn replacing_wide_cells_with_short_text_clears_the_tail_in_the_same_frame() -> TestResult {
    let mut previous_cells = CellBuffer::new(2, 1);
    set(&mut previous_cells, 0, 0, "界", Style::DEFAULT, 2)?;
    let mut previous = PreparedFrame::new(2, 1, None);
    paint_composer_cells(
        &mut previous,
        area(0, 0, 2, 1),
        &previous_cells,
        &true_colour(),
    )?;
    let mut current_cells = CellBuffer::new(2, 1);
    set(&mut current_cells, 0, 0, "x", Style::DEFAULT, 1)?;
    let mut current = PreparedFrame::new(2, 1, None);
    paint_composer_cells(
        &mut current,
        area(0, 0, 2, 1),
        &current_cells,
        &true_colour(),
    )?;
    let bytes = current.encode_delta(Some(&previous))?;
    assert_eq!(bytes, b"\x1b[?25l\x1b[1;1H\x1b[0mx\x1b[0m \x1b[0m\x1b[?25l");
    assert!(current.encode_delta(Some(&current))?.is_empty());
    Ok(())
}
