//! Actual kernel clipboard commands and composer state across transport and source failures.

use std::io::{self, Write};

use iridium_editor::cell_layout::CellWrapParameters;
use iridium_editor::document::{CursorState, Selection};
use iridium_editor::editor::{CellInputOptions, CellReplacementCursor};
use iridium_editor::history::UndoNodeInfo;
use iridium_editor::{CommandArgs, EditorKeyResult, Position, Range};

use super::{
    ClipboardCapability, ClipboardOperation, ClipboardTransportStage, ComposerClipboardError,
    ComposerClipboardPreparation, ComposerClipboardUnavailable, ComposerError, ComposerSnapshot,
    CopyPreparation, CopyUnavailable, InputEditor, PreparedComposerClipboard,
    prepare_composer_clipboard, prepare_copy,
};
use crate::input::InputHistory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn options() -> CellInputOptions {
    CellInputOptions {
        wrap: CellWrapParameters::new(24, 4),
        visible_rows: 4,
    }
}

fn fixture(text: &str, cursor: CursorState) -> Result<InputEditor, ComposerError> {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    editor.replace_cells(
        Range::new(Position::zero(), Position::zero()),
        text,
        CellReplacementCursor::Exact(cursor),
    )?;
    Ok(editor)
}

fn command(editor: &mut InputEditor, name: &str) -> TestResult {
    match editor.run_cell_command(name, CommandArgs::NONE, options())? {
        EditorKeyResult::None => Ok(()),
        EditorKeyResult::Clipboard(_)
        | EditorKeyResult::Search(_)
        | EditorKeyResult::HostCommand { .. } => Err(io::Error::other(format!(
            "fixture command {name} unexpectedly requested a host action"
        ))
        .into()),
    }
}

fn operation(
    editor: &mut InputEditor,
    name: &str,
) -> Result<ClipboardOperation, Box<dyn std::error::Error>> {
    match editor.run_cell_command(name, CommandArgs::NONE, options())? {
        EditorKeyResult::Clipboard(operation) => Ok(operation),
        EditorKeyResult::None
        | EditorKeyResult::Search(_)
        | EditorKeyResult::HostCommand { .. } => Err(io::Error::other(format!(
            "fixture command {name} did not return its clipboard request"
        ))
        .into()),
    }
}

fn ready(
    editor: &mut InputEditor,
    name: &str,
) -> Result<PreparedComposerClipboard, Box<dyn std::error::Error>> {
    let operation = operation(editor, name)?;
    let snapshot = editor.snapshot()?;
    match prepare_composer_clipboard(editor, snapshot, operation, ClipboardCapability::Osc52)? {
        ComposerClipboardPreparation::Ready(prepared) => Ok(*prepared),
        ComposerClipboardPreparation::Unavailable(_)
        | ComposerClipboardPreparation::SanitizedCut => {
            Err(io::Error::other("ordinary clipboard fixture was not prepared").into())
        }
    }
}

struct Witness {
    snapshot: ComposerSnapshot,
    nodes: Vec<UndoNodeInfo>,
    recall_count: usize,
}

impl Witness {
    fn capture(editor: &InputEditor) -> Result<Self, ComposerError> {
        Ok(Self {
            snapshot: editor.snapshot()?,
            nodes: editor.kernel().state().history.snapshot().nodes,
            recall_count: editor.history.len(),
        })
    }

    fn unchanged(&self, editor: &InputEditor) -> TestResult {
        editor.validate_snapshot(&self.snapshot)?;
        assert_eq!(editor.text(), self.snapshot.text());
        assert_eq!(editor.kernel().state().cursor, *self.snapshot.cursor());
        assert_eq!(editor.kernel().state().history.snapshot().nodes, self.nodes);
        assert_eq!(editor.history.len(), self.recall_count);
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum TransportMode {
    Complete,
    WriteFailure,
    FlushFailure,
}

struct TestWriter {
    mode: TransportMode,
    bytes: Vec<u8>,
    flushes: usize,
}

impl TestWriter {
    fn new(mode: TransportMode) -> Self {
        Self {
            mode,
            bytes: Vec::new(),
            flushes: 0,
        }
    }
}

impl Write for TestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if matches!(self.mode, TransportMode::WriteFailure) {
            if !self.bytes.is_empty() {
                return Err(io::Error::other("private transport marker"));
            }
            let count = bytes.len().min(2);
            self.bytes.extend_from_slice(&bytes[..count]);
            return Ok(count);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if matches!(self.mode, TransportMode::FlushFailure) {
            return Err(io::Error::other("private transport marker"));
        }
        Ok(())
    }
}

#[test]
fn unavailable_cut_preserves_document_cursor_undo_and_recall() -> TestResult {
    for (capability, reason) in [
        (
            ClipboardCapability::Unspecified,
            CopyUnavailable::Unspecified,
        ),
        (ClipboardCapability::Disabled, CopyUnavailable::Disabled),
    ] {
        let mut editor = fixture("keep this line", CursorState::at(Position::new(0, 3)))?;
        let before = Witness::capture(&editor)?;
        let operation = operation(&mut editor, "clipboard.cut")?;
        assert!(
            matches!(prepare_composer_clipboard(&editor, editor.snapshot()?, operation, capability)?,
            ComposerClipboardPreparation::Unavailable(ComposerClipboardUnavailable::Transport(found))
                if found == reason)
        );
        before.unchanged(&editor)?;
    }
    Ok(())
}

#[test]
fn partial_write_and_flush_failure_never_commit_cut() -> TestResult {
    for (mode, stage, flushes) in [
        (
            TransportMode::WriteFailure,
            ClipboardTransportStage::Write,
            0,
        ),
        (
            TransportMode::FlushFailure,
            ClipboardTransportStage::Flush,
            1,
        ),
    ] {
        let mut editor = fixture(
            "keep selection",
            CursorState::new(Selection::new(Position::new(0, 4), Position::new(0, 13))),
        )?;
        let before = Witness::capture(&editor)?;
        let prepared = ready(&mut editor, "clipboard.cut")?;
        before.unchanged(&editor)?;
        let mut writer = TestWriter::new(mode);
        let error = prepared
            .send(&mut editor, &mut writer)
            .err()
            .ok_or_else(|| io::Error::other("fixture transport failure was accepted"))?;
        assert!(
            matches!(error, ComposerClipboardError::Transport { stage: found, .. } if found == stage)
        );
        assert!(!format!("{error:?} {error}").contains("private transport marker"));
        assert!(!writer.bytes.is_empty());
        assert_eq!(writer.flushes, flushes);
        before.unchanged(&editor)?;
    }
    Ok(())
}

#[test]
fn successful_selected_cut_is_one_undoable_kernel_transaction() -> TestResult {
    let cursor = CursorState::new(Selection::new(Position::new(0, 7), Position::new(0, 2)));
    let mut editor = fixture("a café end", cursor.clone())?;
    let before = Witness::capture(&editor)?;
    let prepared = ready(&mut editor, "clipboard.cut")?;
    before.unchanged(&editor)?;
    let mut writer = TestWriter::new(TransportMode::Complete);
    let sent = prepared.send(&mut editor, &mut writer)?;
    assert!(sent.cut_applied);
    assert!(!sent.content_changed);
    assert_eq!(writer.flushes, 1);
    assert_eq!(editor.text(), "a end");
    assert_eq!(
        editor.kernel().state().history.snapshot().nodes.len(),
        before.nodes.len() + 1
    );
    let after_cursor = editor.kernel().state().cursor.clone();
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), before.snapshot.text());
    assert_eq!(editor.kernel().state().cursor, cursor);
    command(&mut editor, "history.redo")?;
    assert_eq!(editor.text(), "a end");
    assert_eq!(editor.kernel().state().cursor, after_cursor);
    Ok(())
}

#[test]
fn collapsed_last_line_and_empty_document_keep_kernel_copy_cut_semantics() -> TestResult {
    for (original, cursor, copied, remaining) in [
        ("first\nlast", Position::new(1, 2), "last\n", "first"),
        ("first\n", Position::new(1, 0), "\n", "first"),
        ("", Position::zero(), "\n", ""),
    ] {
        let mut editor = fixture(original, CursorState::at(cursor))?;
        let mut copy_writer = TestWriter::new(TransportMode::Complete);
        let before = Witness::capture(&editor)?;
        let copy = ready(&mut editor, "clipboard.copy")?.send(&mut editor, &mut copy_writer)?;
        assert!(!copy.cut_applied);
        before.unchanged(&editor)?;
        let mut cut_writer = TestWriter::new(TransportMode::Complete);
        let sent = ready(&mut editor, "clipboard.cut")?.send(&mut editor, &mut cut_writer)?;
        assert!(sent.cut_applied);
        assert_eq!(sent.original_bytes, copied.len());
        let CopyPreparation::Ready(expected) = prepare_copy(ClipboardCapability::Osc52, copied)
        else {
            return Err(io::Error::other("fixture copied text unexpectedly unavailable").into());
        };
        assert_eq!(copy_writer.bytes, expected.as_bytes());
        assert_eq!(cut_writer.bytes, expected.as_bytes());
        assert_eq!(editor.text(), remaining);
        if original.is_empty() {
            before.unchanged(&editor)?;
        } else {
            command(&mut editor, "history.undo")?;
            assert_eq!(editor.text(), original);
            assert_eq!(editor.kernel().state().cursor, *before.snapshot.cursor());
        }
    }
    Ok(())
}

#[test]
fn multiple_selections_use_authoritative_compound_cut_and_restore_all_cursors() -> TestResult {
    let mut cursors = CursorState::new(Selection::new(Position::new(0, 0), Position::new(0, 2)));
    cursors.add_cursor(Selection::new(Position::new(0, 8), Position::new(0, 6)));
    let mut editor = fixture("ab cd ef", cursors.clone())?;
    let mut writer = TestWriter::new(TransportMode::Complete);
    let sent = ready(&mut editor, "clipboard.cut")?.send(&mut editor, &mut writer)?;
    assert_eq!(sent.original_bytes, "ab\nef".len());
    assert_eq!(writer.bytes, b"\x1b]52;c;YWIKZWY=\x1b\\");
    assert_eq!(editor.text(), " cd ");
    command(&mut editor, "history.undo")?;
    assert_eq!(editor.text(), "ab cd ef");
    assert_eq!(editor.kernel().state().cursor, cursors);
    Ok(())
}

#[test]
fn sanitized_cut_is_refused_while_copy_preserves_the_original() -> TestResult {
    let original = "private\x1b[31m\u{202e}";
    let mut editor = fixture(original, CursorState::at(Position::zero()))?;
    let before = Witness::capture(&editor)?;
    let operation = operation(&mut editor, "clipboard.cut")?;
    assert!(matches!(
        prepare_composer_clipboard(
            &editor,
            editor.snapshot()?,
            operation,
            ClipboardCapability::Osc52
        )?,
        ComposerClipboardPreparation::SanitizedCut
    ));
    before.unchanged(&editor)?;
    let copy = ready(&mut editor, "clipboard.copy")?;
    let debug = format!("{copy:?}");
    assert!(!debug.contains("private"));
    assert!(!debug.contains("cHJpdmF0ZQ"));
    let mut writer = TestWriter::new(TransportMode::Complete);
    let sent = copy.send(&mut editor, &mut writer)?;
    assert!(sent.content_changed);
    assert!(!sent.cut_applied);
    assert_eq!(writer.flushes, 1);
    assert!(sent.payload_bytes > sent.original_bytes);
    before.unchanged(&editor)?;
    Ok(())
}

#[test]
fn stale_revision_and_cursor_only_movement_refuse_before_any_transport() -> TestResult {
    for edit_text in [false, true] {
        let mut editor = fixture("draft", CursorState::at(Position::new(0, 2)))?;
        let prepared = ready(&mut editor, "clipboard.cut")?;
        if edit_text {
            editor.paste_cells("new")?;
        } else {
            command(&mut editor, "cursor.charLeft")?;
        }
        let after_change = Witness::capture(&editor)?;
        let mut writer = TestWriter::new(TransportMode::Complete);
        assert!(matches!(
            prepared.send(&mut editor, &mut writer),
            Err(ComposerClipboardError::Composer(
                ComposerError::StaleSnapshot { .. }
            ))
        ));
        assert!(writer.bytes.is_empty());
        assert_eq!(writer.flushes, 0);
        after_change.unchanged(&editor)?;
    }
    Ok(())
}

#[test]
fn foreign_document_with_identical_text_and_cursor_cannot_receive_prepared_cut() -> TestResult {
    let mut original = fixture("same", CursorState::at(Position::zero()))?;
    let mut other = fixture("same", CursorState::at(Position::zero()))?;
    let prepared = ready(&mut original, "clipboard.cut")?;
    let first = Witness::capture(&original)?;
    let second = Witness::capture(&other)?;
    let mut writer = TestWriter::new(TransportMode::Complete);
    assert!(matches!(
        prepared.send(&mut other, &mut writer),
        Err(ComposerClipboardError::Composer(
            ComposerError::StaleSnapshot { .. }
        ))
    ));
    assert!(writer.bytes.is_empty());
    assert_eq!(writer.flushes, 0);
    first.unchanged(&original)?;
    second.unchanged(&other)?;
    Ok(())
}

#[test]
fn paste_request_does_not_read_or_edit_the_composer() -> TestResult {
    let editor = fixture("retained", CursorState::at(Position::new(0, 3)))?;
    let before = Witness::capture(&editor)?;
    assert!(matches!(
        prepare_composer_clipboard(
            &editor,
            editor.snapshot()?,
            ClipboardOperation::Paste,
            ClipboardCapability::Osc52
        )?,
        ComposerClipboardPreparation::Unavailable(ComposerClipboardUnavailable::PasteRead)
    ));
    before.unchanged(&editor)?;
    Ok(())
}
