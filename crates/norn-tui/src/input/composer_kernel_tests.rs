//! Real kernel edits, Unicode motion and rejection regressions through the host facade.

use crate::input::{ComposerError, InputEditor, InputHistory};
use iridium_editor::cell_layout::{CellColumn, CellRowMap, CellWrapParameters, ScreenRow};
use iridium_editor::editor::{CellInputOptions, CellReplacementCursor};
use iridium_editor::{
    CommandArgs, CursorState, EditorConfig, EditorKeyResult, KeyCode, KeyEvent, Position, Range,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn options() -> CellInputOptions {
    CellInputOptions {
        wrap: CellWrapParameters::new(8, 4),
        visible_rows: 3,
    }
}

fn command(editor: &mut InputEditor, id: &str) -> TestResult {
    assert_eq!(
        editor.run_cell_command(id, CommandArgs::default(), options())?,
        EditorKeyResult::None
    );
    Ok(())
}

fn type_char(editor: &mut InputEditor, ch: char) -> TestResult {
    assert_eq!(
        editor.handle_cell_key(&KeyEvent::simple(KeyCode::Char(ch)), options())?,
        EditorKeyResult::None
    );
    Ok(())
}

fn caret(editor: &mut InputEditor, line: usize, column: usize) -> TestResult {
    editor.replace_cells(
        Range::empty(Position::zero()),
        "",
        CellReplacementCursor::Exact(CursorState::at(Position::new(line, column))),
    )?;
    Ok(())
}

#[test]
fn construction_is_plain_and_has_one_empty_line_without_parser_or_chrome_defaults() {
    let editor = InputEditor::new(InputHistory::in_memory());
    assert!(editor.is_empty());
    assert_eq!(editor.height(), 1);
    assert_eq!(editor.cursor_position(), (0, 0));
    assert_eq!(editor.text(), "");
    assert_eq!(editor.kernel().language(), None);
    let expected = EditorConfig {
        highlight_current_line: false,
        ..EditorConfig::default()
    };
    assert_eq!(editor.kernel().get_config(), &expected);
    assert_eq!(
        editor.kernel().get_theme().editor.foreground.a.to_bits(),
        0.0_f32.to_bits()
    );
    assert_eq!(
        editor.kernel().get_theme().editor.background.a.to_bits(),
        0.0_f32.to_bits()
    );
}

#[test]
fn auto_pair_typing_skip_and_pair_backspace_use_kernel_commands() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    type_char(&mut editor, '(')?;
    assert_eq!(editor.text(), "()");
    assert_eq!(editor.cursor_position(), (0, 1));
    command(&mut editor, "edit.deleteBackward")?;
    assert_eq!(editor.text(), "");
    type_char(&mut editor, '(')?;
    type_char(&mut editor, ')')?;
    assert_eq!(editor.text(), "()");
    assert_eq!(editor.cursor_position(), (0, 2));
    Ok(())
}

#[test]
fn newline_splits_multiline_cursor_and_preserves_blank_submit_semantics() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("helloworld")?;
    caret(&mut editor, 0, 5)?;
    command(&mut editor, "edit.insertNewline")?;
    assert_eq!(editor.text(), "hello\nworld");
    assert_eq!(editor.height(), 2);
    assert_eq!(editor.cursor_position(), (1, 0));
    editor.clear()?;
    command(&mut editor, "edit.insertNewline")?;
    assert!(editor.is_empty());
    assert_eq!(editor.text(), "\n");
    assert_eq!(editor.height(), 2);
    Ok(())
}

#[test]
fn logical_line_join_delete_and_cross_line_motion_preserve_bytes() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("ab\ncd")?;
    caret(&mut editor, 1, 0)?;
    command(&mut editor, "cursor.charLeft")?;
    assert_eq!(editor.cursor_position(), (0, 2));
    command(&mut editor, "cursor.charRight")?;
    assert_eq!(editor.cursor_position(), (1, 0));
    command(&mut editor, "edit.deleteBackward")?;
    assert_eq!(editor.text(), "abcd");
    assert_eq!(editor.cursor_position(), (0, 2));
    command(&mut editor, "edit.deleteForward")?;
    assert_eq!(editor.text(), "abd");
    Ok(())
}

#[test]
fn character_edits_and_byte_scalar_offsets_are_derived_from_the_same_document() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("hi\nhéx")?;
    assert_eq!(editor.cursor_position(), (1, 3));
    assert_eq!(editor.cursor_char_index()?, 6);
    assert_eq!(editor.cursor_byte_index()?, 7);
    caret(&mut editor, 1, 1)?;
    type_char(&mut editor, 'x')?;
    assert_eq!(editor.text(), "hi\nhxéx");
    caret(&mut editor, 0, 1)?;
    assert_eq!(editor.cursor_char_index()?, 1);
    Ok(())
}

#[test]
fn combining_flags_and_zwj_move_and_delete_as_whole_graphemes() -> TestResult {
    for atom in ["e\u{301}", "🇦🇺", "👩‍🔬", "界"] {
        let mut editor = InputEditor::new(InputHistory::in_memory());
        editor.paste_cells(&format!("a{atom}z"))?;
        command(&mut editor, "cursor.charLeft")?;
        assert_eq!(editor.cursor_byte_index()?, 1 + atom.len());
        command(&mut editor, "cursor.charLeft")?;
        assert_eq!(editor.cursor_byte_index()?, 1);
        command(&mut editor, "edit.deleteForward")?;
        assert_eq!(editor.text(), "az");
        command(&mut editor, "history.undo")?;
        assert_eq!(editor.text(), format!("a{atom}z"));
        command(&mut editor, "cursor.charRight")?;
        command(&mut editor, "edit.deleteBackward")?;
        assert_eq!(editor.text(), "az");
    }
    Ok(())
}

#[test]
fn one_paste_preserves_original_line_endings_and_isolated_undo() -> TestResult {
    let config = EditorConfig {
        undo_group_timeout_ms: u64::MAX,
        ..EditorConfig::default()
    };
    let mut editor = InputEditor::with_config(InputHistory::in_memory(), config);
    type_char(&mut editor, 'a')?;
    let typed = editor.kernel().current_history_node();
    editor.paste_cells("β\r\n\u{1b}[31m\n👩‍🔬")?;
    let pasted = editor.kernel().current_history_node();
    assert_ne!(typed, pasted);
    type_char(&mut editor, 'z')?;
    assert_ne!(editor.kernel().current_history_node(), pasted);
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), "aβ\r\n\u{1b}[31m\n👩‍🔬");
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), "a");
    command(&mut editor, "history.redo")?;
    assert_eq!(editor.text(), "aβ\r\n\u{1b}[31m\n👩‍🔬");
    assert_eq!(editor.kernel().language(), None);
    Ok(())
}

#[test]
fn cell_map_and_pointer_use_actual_width_and_keep_wide_graphemes_whole() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("ab界cde")?;
    let narrow = CellInputOptions {
        wrap: CellWrapParameters::new(4, 4),
        visible_rows: 2,
    };
    let original = editor.snapshot()?;
    let map = CellRowMap::prepare(
        &editor.kernel().state().document,
        &editor.kernel().state().fold_state,
        narrow.wrap,
    )?;
    assert_eq!(map.total_rows(), 2);
    drop(map);
    editor.set_cell_pointer(ScreenRow(0), CellColumn(3), false, narrow)?;
    assert_eq!(editor.cursor_position(), (0, 2));
    assert_eq!(editor.text(), original.text());
    let revision = editor.kernel().state().document.revision();
    let wide = CellRowMap::prepare(
        &editor.kernel().state().document,
        &editor.kernel().state().fold_state,
        CellWrapParameters::new(80, 4),
    )?;
    assert_eq!(wide.total_rows(), 1);
    drop(wide);
    assert_eq!(editor.kernel().state().document.revision(), revision);
    assert_eq!(editor.cursor_position(), (0, 2));
    Ok(())
}

#[test]
fn rejected_command_range_and_pointer_preserve_text_selection_revision_and_undo() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("e\u{301}界")?;
    let snapshot = editor.snapshot()?;
    let node = editor.kernel().current_history_node();
    assert!(matches!(
        editor.run_cell_command("host.does-not-exist", CommandArgs::default(), options()),
        Err(ComposerError::Cell { .. })
    ));
    assert!(editor.replace_snapshot_range(&snapshot, 2..3, "x").is_err());
    assert!(
        editor
            .replace_cells(
                Range::empty(Position::new(0, 1)),
                "x",
                CellReplacementCursor::EndOfReplacement
            )
            .is_err()
    );
    assert!(
        editor
            .set_cell_pointer(ScreenRow(99), CellColumn(0), false, options())
            .is_err()
    );
    editor.validate_snapshot(&snapshot)?;
    assert_eq!(editor.text(), snapshot.text());
    assert_eq!(editor.kernel().state().cursor, *snapshot.cursor());
    assert_eq!(editor.kernel().current_history_node(), node);
    Ok(())
}

#[test]
fn debug_does_not_include_live_content() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("draft-secret")?;
    let text = format!("{editor:?} {:?}", editor.snapshot()?);
    assert!(!text.contains("draft-secret"));
    assert!(text.contains("bytes"));
    Ok(())
}

#[test]
fn word_line_and_document_commands_preserve_existing_navigation_semantics() -> TestResult {
    for (text, start, id, expected) in [
        ("hello world", (0, 11), "cursor.wordLeft", (0, 6)),
        ("hello world", (0, 6), "cursor.wordLeft", (0, 0)),
        ("hello world", (0, 0), "cursor.wordRight", (0, 6)),
        ("hello world", (0, 6), "cursor.wordRight", (0, 11)),
        ("foo_bar baz", (0, 8), "cursor.wordLeft", (0, 0)),
        ("hello", (0, 5), "cursor.lineStart", (0, 0)),
        ("hello", (0, 0), "cursor.lineEnd", (0, 5)),
        (
            "line1\nline2\nline3",
            (2, 5),
            "cursor.documentStart",
            (0, 0),
        ),
        ("line1\nline2\nline3", (0, 0), "cursor.documentEnd", (2, 5)),
    ] {
        let mut editor = InputEditor::new(InputHistory::in_memory());
        editor.paste_cells(text)?;
        caret(&mut editor, start.0, start.1)?;
        command(&mut editor, id)?;
        assert_eq!(editor.cursor_position(), expected, "{id}");
        assert_eq!(editor.text(), text);
    }
    Ok(())
}

#[test]
fn word_and_line_deletion_preserve_existing_boundaries_and_noops() -> TestResult {
    for (text, start, id, expected, end) in [
        (
            "hello world",
            (0, 11),
            "edit.deleteWordBackward",
            "hello ",
            (0, 6),
        ),
        ("ab\ncd", (1, 0), "edit.deleteWordBackward", "abcd", (0, 2)),
        (
            "hello world",
            (0, 0),
            "edit.deleteWordForward",
            "world",
            (0, 0),
        ),
        ("ab\ncd", (0, 2), "edit.deleteWordForward", "abcd", (0, 2)),
        (
            "hello world",
            (0, 6),
            "edit.deleteToLineStart",
            "world",
            (0, 0),
        ),
        ("hello", (0, 0), "edit.deleteToLineStart", "hello", (0, 0)),
        (
            "hello world",
            (0, 5),
            "edit.deleteToLineEnd",
            "hello",
            (0, 5),
        ),
        ("hello", (0, 5), "edit.deleteToLineEnd", "hello", (0, 5)),
    ] {
        let mut editor = InputEditor::new(InputHistory::in_memory());
        editor.paste_cells(text)?;
        caret(&mut editor, start.0, start.1)?;
        let before = editor.snapshot()?;
        let node = editor.kernel().current_history_node();
        command(&mut editor, id)?;
        assert_eq!(editor.text(), expected, "{id}");
        assert_eq!(editor.cursor_position(), end, "{id}");
        if text == expected {
            editor.validate_snapshot(&before)?;
            assert_eq!(editor.kernel().current_history_node(), node);
        } else {
            command(&mut editor, "history.undo")?;
            assert_eq!(editor.text(), text);
            assert_eq!(editor.kernel().state().cursor, *before.cursor());
        }
    }
    Ok(())
}

#[test]
fn visual_up_down_use_wrapped_rows_and_preserve_sticky_column() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("abcdefghij")?;
    caret(&mut editor, 0, 7)?;
    let geometry = CellInputOptions {
        wrap: CellWrapParameters::new(5, 4),
        visible_rows: 2,
    };
    assert_eq!(
        editor.run_cell_command("cursor.lineUp", CommandArgs::default(), geometry)?,
        EditorKeyResult::None
    );
    assert_eq!(editor.cursor_position(), (0, 2));
    assert_eq!(
        editor.run_cell_command("cursor.lineDown", CommandArgs::default(), geometry)?,
        EditorKeyResult::None
    );
    assert_eq!(editor.cursor_position(), (0, 7));
    assert_eq!(editor.text(), "abcdefghij");
    Ok(())
}

#[test]
fn cell_geometry_replaces_legacy_wrap_without_changing_original_row_and_caret_cases() -> TestResult
{
    use iridium_editor::cell_layout::Affinity;
    for (text, width, cursor, rows, placed) in [
        ("hello", 80, (0, 3), 1, (0, 3)),
        ("abcdefghij", 5, (0, 7), 2, (1, 2)),
        ("abcd世", 5, (0, 4), 2, (1, 0)),
        ("😀😀😀", 5, (0, 2), 2, (1, 0)),
        ("", 80, (0, 0), 1, (0, 0)),
        ("abc\ndefgh", 80, (1, 2), 2, (1, 2)),
        ("abc", 80, (0, 3), 1, (0, 3)),
        ("abcde", 3, (0, 5), 2, (1, 2)),
        ("abc", 1, (0, 2), 3, (2, 0)),
        ("abcde", 5, (0, 5), 1, (0, 5)),
        ("abcdefghijklmno", 5, (0, 14), 3, (2, 4)),
    ] {
        let mut editor = InputEditor::new(InputHistory::in_memory());
        editor.paste_cells(text)?;
        caret(&mut editor, cursor.0, cursor.1)?;
        let map = CellRowMap::prepare(
            &editor.kernel().state().document,
            &editor.kernel().state().fold_state,
            CellWrapParameters::new(width, 4),
        )?;
        assert_eq!(map.total_rows(), rows, "{text:?}");
        let actual = map
            .place(editor.kernel().cursor(), Affinity::Downstream)?
            .ok_or("fixture cursor absent")?;
        assert_eq!((actual.row.0, actual.column.0), placed, "{text:?}");
    }
    Ok(())
}
