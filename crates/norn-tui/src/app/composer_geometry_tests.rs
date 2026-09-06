//! Kernel-backed composer measurement, caret, pointer and stale-extent contracts.

use super::*;
use crate::input::history::InputHistory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn editor(text: &str) -> Result<InputEditor, crate::input::composer_kernel::ComposerError> {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells(text)?;
    Ok(editor)
}

fn input_area(height: u16, columns: u16, rows: u16) -> Result<Rect, TuiError> {
    let Layout::Ready { composer, .. } = Layout::calculate(
        LayoutRequest {
            columns,
            rows,
            requested_composer_rows: height,
            changes_open: false,
            split: SplitPreference::default(),
            active_upper_pane: UpperPane::Conversation,
        },
        LayoutPolicy::default(),
    )?
    else {
        return Err(TuiError::FrameBounds);
    };
    Ok(composer_input_area(composer))
}

#[test]
fn painted_cells_caret_and_pointer_use_the_same_full_width_kernel_rows() -> TestResult {
    for text in [
        "plain",
        "e\u{301}🇦🇺👩‍💻Z",
        "one\ntwo\nthree",
        "宽\t界",
        "abcdef",
    ] {
        for width in [1, 4, 12, 80] {
            let mut editor = editor(text)?;
            let original_cursor = editor.kernel().state().cursor.clone();
            let mut geometry = ComposerGeometry::default();
            let height = geometry.measure(&editor, width, 24)?;
            let area = input_area(height, width, 24)?;
            let (layer, cursor) = geometry.prepare(&editor, area)?;
            assert!(geometry.pointer(&editor, area.column, area.row).is_err());
            geometry.finish_publication(Ok(()))?;
            assert_eq!(layer.area, area);
            assert_eq!(layer.cells.width(), usize::from(width));
            assert_eq!(layer.cells.height(), usize::from(height));
            assert_eq!(editor.text(), text);
            assert_eq!(editor.kernel().state().cursor, original_cursor);
            let (column, row) = cursor.ok_or("primary caret missing")?;
            assert!(column < width);
            assert!(row >= area.row && row < area.row + height);
            let expected = {
                let prepared = IridiumFrame::prepare_cells(
                    editor.kernel(),
                    CellFrameOptions {
                        columns: usize::from(width),
                        rows: usize::from(height),
                        first_row: geometry.first_row(),
                        chrome: None,
                    },
                )?;
                prepared
                    .position_at(usize::from(column), usize::from(row - area.row))?
                    .ok_or("caret cell has no current kernel hit")?
            };
            let pointer = geometry
                .pointer(&editor, column, row)?
                .ok_or("pointer missing")?;
            editor.set_cell_pointer(pointer.row, pointer.column, false, pointer.options)?;
            assert_eq!(editor.kernel().cursor(), expected.position);
            assert_eq!(
                editor.kernel().cell_affinity(pointer.options),
                expected.affinity
            );
            assert_eq!(editor.text(), text);
        }
    }
    Ok(())
}

#[test]
fn resize_keeps_original_selection_and_scrolls_to_the_actual_caret() -> TestResult {
    let mut editor = editor("first line α🙂\nsecond\nthird\nfourth\nfifth\nsixth")?;
    let mut geometry = ComposerGeometry::default();
    geometry.measure(&editor, 80, 24)?;
    let options = geometry.input_options();
    editor.set_cell_pointer(ScreenRow(0), CellColumn(0), false, options)?;
    editor.set_cell_pointer(ScreenRow(5), CellColumn(2), true, options)?;
    let selection = editor.kernel().state().cursor.clone();
    assert!(!selection.primary.is_collapsed());
    for (columns, rows, expected_height) in [(80, 24, 6), (4, 8, 2), (1, 3, 1), (80, 24, 6)] {
        let height = geometry.measure(&editor, columns, rows)?;
        assert_eq!(height, expected_height);
        let cursor = geometry.cursor_row().ok_or("caret row missing")?;
        assert!(cursor >= geometry.first_row());
        assert!(cursor.0 - geometry.first_row().0 < usize::from(height));
        geometry.prepare(&editor, input_area(height, columns, rows)?)?;
        assert_eq!(editor.kernel().state().cursor, selection);
        assert_eq!(
            geometry.input_options().wrap.columns(),
            usize::from(columns)
        );
    }
    Ok(())
}

#[test]
fn pointers_refuse_mutation_or_resize_since_their_displayed_geometry() -> TestResult {
    let mut editor = editor("original")?;
    let mut geometry = ComposerGeometry::default();
    assert!(geometry.pointer(&editor, 0, 0).is_err());
    let height = geometry.measure(&editor, 12, 24)?;
    let area = input_area(height, 12, 24)?;
    geometry.prepare(&editor, area)?;
    geometry.finish_publication(Ok(()))?;
    assert!(geometry.pointer(&editor, area.column, area.row)?.is_some());
    editor.paste_cells(" changed")?;
    let before = editor.text();
    assert!(geometry.pointer(&editor, area.column, area.row).is_err());
    assert_eq!(editor.text(), before);
    let height = geometry.measure(&editor, 12, 24)?;
    let area = input_area(height, 12, 24)?;
    geometry.prepare(&editor, area)?;
    geometry.measure(&editor, 12, 25)?;
    assert!(geometry.pointer(&editor, area.column, area.row).is_err());
    let height = geometry.measure(&editor, 12, 25)?;
    geometry.prepare(&editor, input_area(height, 12, 25)?)?;
    let mismatch = Rect { width: 11, ..area };
    assert!(matches!(
        geometry.prepare(&editor, mismatch),
        Err(TuiError::FrameBounds)
    ));
    Ok(())
}

#[test]
fn zero_or_tiny_extents_do_not_mutate_the_document_or_invent_rows() -> TestResult {
    let editor = editor("unchanged")?;
    let mut geometry = ComposerGeometry::default();
    for (columns, rows) in [(0, 0), (0, 24), (80, 0), (1, 1)] {
        assert_eq!(geometry.measure(&editor, columns, rows)?, 0);
        assert_eq!(editor.text(), "unchanged");
    }
    Ok(())
}

#[test]
fn failed_actual_frame_flush_keeps_the_old_baseline_and_revokes_pointer_authority() -> TestResult {
    use crate::render::frame::Frame;
    use crate::terminal::caps::TerminalCaps;
    struct FailedFlush {
        written: Vec<u8>,
    }
    impl std::io::Write for FailedFlush {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("fixture terminal flush failed"))
        }
    }
    fn frame(layer: ComposerLayer, cursor: Option<(u16, u16)>) -> Result<Frame, TuiError> {
        let layout = Layout::calculate(
            LayoutRequest {
                columns: 12,
                rows: 24,
                requested_composer_rows: layer.area.height,
                changes_open: false,
                split: SplitPreference::default(),
                active_upper_pane: UpperPane::Conversation,
            },
            LayoutPolicy::default(),
        )?;
        Ok(Frame {
            layout,
            rows: Vec::new(),
            composer: Some(layer),
            cursor,
        })
    }
    let mut editor = editor("first")?;
    let mut geometry = ComposerGeometry::default();
    let height = geometry.measure(&editor, 12, 24)?;
    let area = input_area(height, 12, 24)?;
    geometry.begin_frame();
    let (layer, cursor) = geometry.prepare(&editor, area)?;
    assert!(geometry.pointer(&editor, area.column, area.row).is_err());
    let mut baseline = None;
    let first = frame(layer, cursor)?.prepare(&TerminalCaps::baseline())?;
    let mut writer = Vec::new();
    geometry.finish_publication(first.publish(&mut baseline, &mut writer, false))?;
    assert!(geometry.pointer(&editor, area.column, area.row)?.is_some());
    let baseline_bytes = baseline
        .as_ref()
        .ok_or("published baseline missing")?
        .encode_delta(None)?;
    editor.paste_cells(" changed")?;
    let height = geometry.measure(&editor, 12, 24)?;
    let area = input_area(height, 12, 24)?;
    geometry.begin_frame();
    let (layer, cursor) = geometry.prepare(&editor, area)?;
    assert!(geometry.pointer(&editor, area.column, area.row).is_err());
    let second = frame(layer, cursor)?.prepare(&TerminalCaps::baseline())?;
    let mut writer = FailedFlush {
        written: Vec::new(),
    };
    let failure = geometry.finish_publication(second.publish(&mut baseline, &mut writer, false));
    assert!(
        failure
            .as_ref()
            .is_err_and(|error| error.to_string().contains("fixture terminal flush failed"))
    );
    assert!(
        !writer.written.is_empty(),
        "flush failure follows actual terminal bytes"
    );
    assert_eq!(
        baseline
            .as_ref()
            .ok_or("original baseline lost")?
            .encode_delta(None)?,
        baseline_bytes
    );
    assert!(geometry.pointer(&editor, area.column, area.row).is_err());
    assert!(geometry.displayed.is_none());
    assert!(geometry.staged.is_none());
    geometry.begin_frame();
    let (layer, cursor) = geometry.prepare(&editor, area)?;
    let retried = frame(layer, cursor)?.prepare(&TerminalCaps::baseline())?;
    geometry.finish_publication(retried.publish(&mut baseline, &mut Vec::new(), false))?;
    assert!(geometry.pointer(&editor, area.column, area.row)?.is_some());
    geometry.begin_frame();
    geometry.finish_publication(Ok(()))?;
    assert!(
        geometry.pointer(&editor, area.column, area.row).is_err(),
        "a frame without composer revokes its old pointer rectangle"
    );
    Ok(())
}

#[test]
fn queued_down_drag_and_up_keep_geometry_authority_across_cursor_only_changes() -> TestResult {
    let mut editor = editor("abcdefghij")?;
    let mut geometry = ComposerGeometry::default();
    let height = geometry.measure(&editor, 12, 24)?;
    let area = input_area(height, 12, 24)?;
    geometry.begin_frame();
    geometry.prepare(&editor, area)?;
    geometry.finish_publication(Ok(()))?;
    let original_revision = editor.kernel().state().document.revision();
    let original_cursor = editor.kernel().state().cursor.clone();
    for (column, extend) in [(1, false), (8, true), (9, true)] {
        let pointer = geometry
            .pointer(&editor, area.column + column, area.row)?
            .ok_or("queued mouse event lost its visible document hit")?;
        editor.set_cell_pointer(pointer.row, pointer.column, extend, pointer.options)?;
        assert_eq!(
            editor.kernel().state().document.revision(),
            original_revision
        );
        assert_eq!(editor.text(), "abcdefghij");
        assert_eq!(
            editor.kernel().cursor(),
            iridium_editor::Position::new(0, usize::from(column))
        );
    }
    let selected = &editor.kernel().state().cursor.primary;
    assert_eq!(selected.anchor, iridium_editor::Position::new(0, 1));
    assert_eq!(selected.head, iridium_editor::Position::new(0, 9));
    assert_ne!(editor.kernel().state().cursor, original_cursor);
    assert_ne!(
        geometry.measured.as_ref(),
        Some(&Source::capture(editor.kernel(), geometry.input_options())),
        "measurement cache must remain cursor-sensitive even though pointer authority is not"
    );
    geometry.measure(&editor, 12, 24)?;
    assert_eq!(
        geometry.measured.as_ref(),
        Some(&Source::capture(editor.kernel(), geometry.input_options()))
    );
    assert!(
        geometry
            .pointer(&editor, area.column + 2, area.row)?
            .is_some()
    );
    Ok(())
}
