//! Draft ownership preserves exact editing state and one submission recall authority.

use iridium_editor::cell_layout::CellWrapParameters;
use iridium_editor::editor::CellInputOptions;
use iridium_editor::{CommandArgs, EditorConfig};

use super::*;
use crate::input::InputHistory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn command(editor: &mut InputEditor, name: &str) -> TestResult {
    editor.run_cell_command(
        name,
        CommandArgs::NONE,
        CellInputOptions {
            wrap: CellWrapParameters::new(80, 4),
            visible_rows: 10,
        },
    )?;
    Ok(())
}

fn history(editor: &InputEditor) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(editor.kernel().history_snapshot())
}

#[test]
fn exchanges_preserve_both_undo_trees_selections_and_current_configuration() -> TestResult {
    let mut editor = InputEditor::with_config(
        InputHistory::in_memory(),
        EditorConfig {
            tab_width: 2,
            ..EditorConfig::default()
        },
    );
    editor.paste_cells("original e\u{301}\r\n👩‍💻 text")?;
    command(&mut editor, "cursor.wordLeftSelect")?;
    let original = editor.snapshot()?;
    let original_history = history(&editor)?;
    let mut detached = editor.detach_draft(&original)?;
    detached.validate_snapshot(&original)?;
    assert!(editor.is_empty());
    assert_ne!(editor.snapshot()?.document_id(), original.document_id());
    assert_eq!(editor.kernel().get_config().tab_width, 2);

    editor.paste_cells("new draft")?;
    command(&mut editor, "cursor.wordLeft")?;
    let next = editor.snapshot()?;
    let next_history = history(&editor)?;
    let mut config = editor.kernel().get_config().clone();
    config.tab_width = 8;
    editor.set_config(config);
    editor.exchange_draft(&mut detached);
    editor.validate_snapshot(&original)?;
    assert_eq!(editor.text(), original.text());
    assert_eq!(history(&editor)?, original_history);
    assert_eq!(editor.kernel().get_config().tab_width, 8);
    detached.validate_snapshot(&next)?;

    editor.exchange_draft(&mut detached);
    editor.validate_snapshot(&next)?;
    assert_eq!(editor.text(), next.text());
    assert_eq!(history(&editor)?, next_history);
    assert_eq!(editor.kernel().get_config().tab_width, 8);
    command(&mut editor, "history.undo")?;
    assert!(editor.is_empty());
    editor.exchange_draft(&mut detached);
    command(&mut editor, "history.undo")?;
    assert!(editor.is_empty());
    Ok(())
}

#[test]
fn detached_acceptance_records_only_original_without_modifying_new_draft() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("recall.txt");
    let mut editor = InputEditor::new(InputHistory::load_from(&path));
    editor.paste_cells("accepted α\r\noriginal")?;
    let original = editor.snapshot()?;
    let detached = editor.detach_draft(&original)?;
    editor.paste_cells("still writing")?;
    let next = editor.snapshot()?;
    let next_history = history(&editor)?;
    editor.record_detached_accepted(&detached, &original)?;
    editor.validate_snapshot(&next)?;
    assert_eq!(history(&editor)?, next_history);
    assert_eq!(editor.history.len(), 1);
    assert_eq!(editor.history.entry(0), Some(original.text()));
    let reloaded = InputHistory::load_from(&path);
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded.entry(0), Some(original.text()));
    Ok(())
}

#[test]
fn stale_and_foreign_snapshots_refuse_before_detach_or_recall_append() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.paste_cells("before")?;
    let stale = editor.snapshot()?;
    editor.paste_cells(" after")?;
    let current = editor.snapshot()?;
    let witness = history(&editor)?;
    assert!(editor.detach_draft(&stale).is_err());
    editor.validate_snapshot(&current)?;
    assert_eq!(history(&editor)?, witness);
    let detached = editor.detach_draft(&current)?;
    let next = editor.snapshot()?;
    assert!(editor.record_detached_accepted(&detached, &stale).is_err());
    assert!(editor.record_detached_accepted(&detached, &next).is_err());
    editor.validate_snapshot(&next)?;
    detached.validate_snapshot(&current)?;
    assert!(editor.history.is_empty());
    Ok(())
}

#[test]
fn recall_navigation_keeps_its_saved_draft_across_an_exchange() -> TestResult {
    let mut recall = InputHistory::in_memory();
    recall.append("earlier accepted input")?;
    let mut editor = InputEditor::new(recall);
    editor.paste_cells("original unsent draft")?;
    command(&mut editor, "cursor.wordLeftSelect")?;
    let original = editor.snapshot()?;
    assert!(editor.history_prev()?);
    let recalled = editor.snapshot()?;
    let mut detached = editor.detach_draft(&recalled)?;
    editor.paste_cells("separate next draft")?;
    let next = editor.snapshot()?;
    editor.exchange_draft(&mut detached);
    editor.validate_snapshot(&recalled)?;
    assert!(editor.history_next()?);
    assert_eq!(editor.text(), original.text());
    assert_eq!(editor.kernel().state().cursor, *original.cursor());
    editor.exchange_draft(&mut detached);
    editor.validate_snapshot(&next)?;
    assert_eq!(editor.history.len(), 1);
    Ok(())
}
