//! Host transactions preserve exact original state across acceptance, rejection and recall.

use crate::input::{Acceptance, ComposerError, InputEditor, InputHistory};
use iridium_editor::cell_layout::CellWrapParameters;
use iridium_editor::editor::{CellInputOptions, CellReplacementCursor};
use iridium_editor::{CommandArgs, CursorState, EditorKeyResult, Position, Range, Selection};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn command(editor: &mut InputEditor, id: &str) -> TestResult {
    assert_eq!(
        editor.run_cell_command(
            id,
            CommandArgs::default(),
            CellInputOptions {
                wrap: CellWrapParameters::new(20, 4),
                visible_rows: 3,
            }
        )?,
        EditorKeyResult::None
    );
    Ok(())
}

fn select(editor: &mut InputEditor, selection: CursorState) -> TestResult {
    editor.replace_cells(
        Range::empty(Position::zero()),
        "",
        CellReplacementCursor::Exact(selection),
    )?;
    Ok(())
}

fn with_history() -> Result<InputEditor, Box<dyn std::error::Error>> {
    let mut history = InputHistory::in_memory();
    history.append("older")?;
    history.append("one\ntwo")?;
    Ok(InputEditor::new(history))
}

#[test]
fn refused_admission_leaves_exact_draft_and_history_until_explicit_acceptance() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("hello\nworld")?;
    select(
        &mut editor,
        CursorState::new(Selection::new(Position::new(1, 4), Position::new(0, 1))),
    )?;
    let snapshot = editor.snapshot()?;
    let node = editor.kernel().current_history_node();
    editor.validate_snapshot(&snapshot)?;
    // A refused destination does not invoke either post-acceptance operation.
    assert_eq!(editor.text(), "hello\nworld");
    assert_eq!(editor.kernel().state().cursor, *snapshot.cursor());
    assert_eq!(editor.kernel().current_history_node(), node);
    assert!(editor.history.is_empty());
    editor.clear_accepted(&snapshot)?;
    editor.record_accepted(&snapshot)?;
    assert!(editor.is_empty());
    assert_eq!(editor.history.len(), 1);
    assert_eq!(editor.history.entry(0), Some("hello\nworld"));
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), snapshot.text());
    assert_eq!(editor.kernel().state().cursor, *snapshot.cursor());
    assert_eq!(
        editor.history.len(),
        1,
        "undo never dispatches or records again"
    );
    Ok(())
}

#[test]
fn accepted_clear_never_erases_a_later_edit_or_an_equal_revision_other_document() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("draft")?;
    let submitted = editor.snapshot()?;
    editor.paste_cells(" newer")?;
    let newer = editor.snapshot()?;
    let node = editor.kernel().current_history_node();
    assert!(matches!(
        editor.clear_accepted(&submitted),
        Err(ComposerError::StaleSnapshot { .. })
    ));
    editor.validate_snapshot(&newer)?;
    assert_eq!(editor.kernel().current_history_node(), node);
    let mut other = InputEditor::new(InputHistory::in_memory());
    other.paste_cells("draft")?;
    assert!(matches!(
        other.clear_accepted(&submitted),
        Err(ComposerError::StaleSnapshot { .. })
    ));
    assert_eq!(other.text(), "draft");
    Ok(())
}

#[test]
fn accepted_history_failure_is_separate_and_submitted_text_is_undo_recoverable() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("history.txt");
    let mut editor = InputEditor::new(InputHistory::load_from(&path));
    editor.paste_cells("accepted original")?;
    let accepted = editor.snapshot()?;
    std::fs::create_dir(&path)?;
    editor.clear_accepted(&accepted)?;
    match editor.record_accepted(&accepted) {
        Err(ComposerError::History {
            path: Some(actual), ..
        }) => assert_eq!(actual, path),
        outcome => {
            return Err(format!("expected located recall-write error, got {outcome:?}").into());
        }
    }
    assert!(editor.is_empty());
    assert!(editor.history.is_empty());
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), "accepted original");
    assert_eq!(editor.kernel().state().cursor, *accepted.cursor());
    Ok(())
}

#[test]
fn secret_accepted_clear_does_not_implicitly_record_input() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("/auth secret")?;
    let accepted = editor.snapshot()?;
    editor.clear_accepted(&accepted)?;
    assert!(editor.history.is_empty());
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), "/auth secret");
    assert!(!format!("{accepted:?}").contains("secret"));
    Ok(())
}

#[test]
fn older_recall_saturates_and_forward_exit_restores_original_reverse_selection() -> TestResult {
    let mut editor = with_history()?;
    editor.paste_cells("my draft")?;
    let mut cursor = CursorState::new(Selection::new(Position::new(0, 7), Position::new(0, 3)));
    cursor.add_cursor(Selection::collapsed(Position::new(0, 1)));
    select(&mut editor, cursor.clone())?;
    let draft = editor.snapshot()?;
    assert!(editor.history_prev()?);
    assert_eq!(editor.text(), "one\ntwo");
    assert_eq!(editor.cursor_position(), (1, 3));
    assert!(editor.history_prev()?);
    assert_eq!(editor.text(), "older");
    assert!(editor.history_prev()?);
    assert_eq!(editor.text(), "older");
    assert!(editor.history_next()?);
    assert_eq!(editor.text(), "one\ntwo");
    assert!(editor.history_next()?);
    assert_eq!(editor.text(), draft.text());
    assert_eq!(editor.kernel().state().cursor, cursor);
    assert!(!editor.history_next()?);
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), "one\ntwo");
    command(&mut editor, "history.redo")?;
    assert_eq!(editor.text(), draft.text());
    assert_eq!(editor.kernel().state().cursor, cursor);
    Ok(())
}

#[test]
fn empty_history_and_no_navigation_do_not_mutate_text_cursor_or_undo() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("draft")?;
    let original = editor.snapshot()?;
    let node = editor.kernel().current_history_node();
    assert!(!editor.history_prev()?);
    assert!(!editor.history_next()?);
    editor.validate_snapshot(&original)?;
    assert_eq!(editor.kernel().current_history_node(), node);
    Ok(())
}

#[test]
fn clear_cancels_recall_and_later_recall_captures_the_new_draft() -> TestResult {
    let mut editor = with_history()?;
    editor.paste_cells("first draft")?;
    assert!(editor.history_prev()?);
    editor.clear()?;
    editor.paste_cells("second draft")?;
    assert!(editor.history_prev()?);
    assert!(editor.history_next()?);
    assert_eq!(editor.text(), "second draft");
    Ok(())
}

#[test]
fn completion_replaces_only_original_prefix_and_preserves_suffix_and_unicode() -> TestResult {
    for (before, caret, start, replacement, expected) in [
        ("/he", 3, 0, "/help", "/help"),
        ("look at @sr", 11, 8, "src/main.rs", "look at src/main.rs"),
        ("@éx suffix", 3, 0, "éclair", "éclair suffix"),
        ("hi\n/he suffix", 3, 3, "/help", "hi\n/help suffix"),
    ] {
        let mut editor = InputEditor::new(InputHistory::in_memory());
        editor.paste_cells(before)?;
        let line = usize::from(before.contains('\n'));
        select(&mut editor, CursorState::at(Position::new(line, caret)))?;
        let prior = editor.snapshot()?;
        let acceptance = Acceptance {
            context: editor.completion_context(start)?,
            replacement: replacement.to_owned(),
        };
        editor.apply_acceptance(&acceptance)?;
        assert_eq!(editor.text(), expected);
        let accepted = editor.snapshot()?;
        command(&mut editor, "history.undo")?;
        assert_eq!(editor.text(), before);
        assert_eq!(editor.kernel().state().cursor, *prior.cursor());
        command(&mut editor, "history.redo")?;
        assert_eq!(editor.text(), expected);
        assert_eq!(editor.kernel().state().cursor, *accepted.cursor());
    }
    Ok(())
}

#[test]
fn completion_rejects_text_or_cursor_changes_even_when_offsets_still_fit() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("/he")?;
    let acceptance = Acceptance {
        context: editor.completion_context(0)?,
        replacement: "/help".to_owned(),
    };
    command(&mut editor, "cursor.charLeft")?;
    let moved = editor.snapshot()?;
    assert!(matches!(
        editor.apply_acceptance(&acceptance),
        Err(ComposerError::StaleSnapshot { .. })
    ));
    editor.validate_snapshot(&moved)?;
    command(&mut editor, "cursor.charRight")?;
    editor.paste_cells("x")?;
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), "/he");
    let returned = editor.snapshot()?;
    assert!(matches!(
        editor.apply_acceptance(&acceptance),
        Err(ComposerError::StaleSnapshot { .. })
    ));
    editor.validate_snapshot(&returned)?;
    Ok(())
}

#[test]
fn malformed_completion_and_grapheme_interior_do_not_silently_change_input() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("e\u{301}")?;
    let original = editor.snapshot()?;
    let node = editor.kernel().current_history_node();
    assert!(matches!(
        editor.completion_context(9),
        Err(ComposerError::ByteRange { .. })
    ));
    assert!(matches!(
        editor.completion_context(2),
        Err(ComposerError::ByteRange { .. })
    ));
    let acceptance = Acceptance {
        context: editor.completion_context(1)?,
        replacement: "x".to_owned(),
    };
    assert!(editor.apply_acceptance(&acceptance).is_err());
    editor.validate_snapshot(&original)?;
    assert_eq!(editor.kernel().current_history_node(), node);
    select(
        &mut editor,
        CursorState::new(Selection::new(Position::zero(), Position::new(0, 2))),
    )?;
    assert!(matches!(
        editor.completion_context(0),
        Err(ComposerError::ActiveSelection)
    ));
    Ok(())
}

#[test]
fn whole_replacements_and_joins_restore_original_selection_and_undo_branches() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("👩🔬")?;
    let before = editor.snapshot()?;
    editor.replace_cells(
        Range::empty(Position::new(0, 1)),
        "\u{200d}",
        CellReplacementCursor::EndOfReplacement,
    )?;
    assert_eq!(editor.text(), "👩‍🔬");
    assert_eq!(editor.cursor_position(), (0, 3));
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), before.text());
    assert_eq!(editor.kernel().state().cursor, *before.cursor());
    editor.clear()?;
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), before.text());
    assert_eq!(editor.kernel().history_branches().len(), 2);
    Ok(())
}

#[test]
fn rejected_recall_preserves_navigation_and_draft_for_later_recovery() -> TestResult {
    let mut editor = with_history()?;
    editor.paste_cells("draft")?;
    assert!(editor.history_prev()?);
    // Read-only is a kernel invariant normally set by a host configuration.
    // Invalid replacement coordinates instead exercise the public refusal path here.
    let original = editor.snapshot()?;
    let node = editor.kernel().current_history_node();
    assert!(
        editor
            .replace_cells(
                Range::empty(Position::new(99, 0)),
                "x",
                CellReplacementCursor::EndOfReplacement
            )
            .is_err()
    );
    editor.validate_snapshot(&original)?;
    assert_eq!(editor.kernel().current_history_node(), node);
    assert!(editor.history_next()?);
    assert_eq!(editor.text(), "draft");
    Ok(())
}
