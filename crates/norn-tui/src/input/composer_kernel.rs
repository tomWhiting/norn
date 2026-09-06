//! Typed adapter to Iridium cell input; no host text buffer or terminal ownership.

use iridium_editor::cell_layout::{CellColumn, ScreenRow};
use iridium_editor::editor::{CellInputError, CellInputOptions, CellReplacementCursor};
use iridium_editor::{
    CommandArgs, Editor, EditorConfig, EditorKeyResult, KeyEvent, Position, Range,
};
use std::ops::Range as ByteRange;
use std::path::PathBuf;

/// Refused composer operations retain coordinates and their original typed cause.
#[derive(Debug, thiserror::Error)]
pub enum ComposerError {
    /// Iridium refused the operation before publishing an edit.
    #[error("composer {operation}: {source}")]
    Cell {
        /// Host operation name, never original content.
        operation: &'static str,
        /// Original kernel error.
        #[source]
        source: CellInputError,
    },
    /// The current logical cursor does not resolve in its actual document.
    #[error("composer position {position:?} does not resolve")]
    Position {
        /// Refused kernel position.
        position: Position,
    },
    /// Byte endpoints must be ordered, exact and present; ICC checks graphemes next.
    #[error("composer byte range {range:?} does not resolve in {bytes} bytes")]
    ByteRange {
        /// Exact refused range.
        range: ByteRange<usize>,
        /// Actual document byte length.
        bytes: usize,
    },
    /// Content or complete cursor state changed after a host snapshot.
    #[error("composer snapshot for document {document} revision {revision} is stale")]
    StaleSnapshot {
        /// Original document identity.
        document: u64,
        /// Original content revision.
        revision: u64,
    },
    /// A host operation cannot silently discard extra selections.
    #[error("composer operation requires one selection; found {count}")]
    MultipleSelections {
        /// Actual selection count.
        count: usize,
    },
    /// Completion needs a caret, not implicit replacement of an active selection.
    #[error("composer completion requires a collapsed selection")]
    ActiveSelection,
    /// Recall's selected entry does not exist.
    #[error("composer recall entry {index} does not exist")]
    HistoryEntry {
        /// Requested chronological entry index.
        index: usize,
    },
    /// Runtime acceptance and recall storage are separate outcomes.
    #[error("accepted input could not be recorded in recall history {path:?}: {source}")]
    History {
        /// Exact configured backing path, or none for in-memory history.
        path: Option<PathBuf>,
        /// Original append failure.
        #[source]
        source: std::io::Error,
    },
    /// An authoritative clipboard command failed when applied to a private copy.
    #[error("composer clipboard preparation: {source}")]
    Clipboard {
        /// Original reversible-command failure.
        #[source]
        source: iridium_editor::IridiumError,
    },
}

/// Sole mutable editor; callers cannot obtain a mutable kernel reference.
pub(super) struct ComposerKernel {
    editor: Editor,
}

impl std::fmt::Debug for ComposerKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let document = &self.editor.state().document;
        f.debug_struct("ComposerKernel")
            .field("document", &document.id())
            .field("revision", &document.revision())
            .field("bytes", &document.byte_count())
            .field(
                "selection_count",
                &self.editor.state().cursor.cursor_count(),
            )
            .finish()
    }
}

impl ComposerKernel {
    pub(super) fn new(mut config: EditorConfig) -> Self {
        // D6 keeps the existing plain composer without a highlighted current row.
        config.highlight_current_line = false;
        let mut editor = Editor::new(config);
        editor.clear_language();
        let mut theme = editor.get_theme().clone();
        theme.editor.foreground.a = 0.0;
        theme.editor.background.a = 0.0;
        editor.set_theme(theme);
        Self { editor }
    }

    pub(super) fn editor(&self) -> &Editor {
        &self.editor
    }

    pub(super) fn cursor_byte_index(&self) -> Result<usize, ComposerError> {
        let position = self.editor.cursor();
        self.editor
            .state()
            .document
            .position_to_offset(position)
            .ok_or(ComposerError::Position { position })
    }

    pub(super) fn whole_range(&self) -> Result<Range, ComposerError> {
        self.byte_range(0..self.editor.state().document.byte_count())
    }

    pub(super) fn byte_range(&self, bytes: ByteRange<usize>) -> Result<Range, ComposerError> {
        let document = &self.editor.state().document;
        let failure = || ComposerError::ByteRange {
            range: bytes.clone(),
            bytes: document.byte_count(),
        };
        if bytes.start > bytes.end || bytes.end > document.byte_count() {
            return Err(failure());
        }
        let start = document
            .offset_to_position(bytes.start)
            .ok_or_else(failure)?;
        let end = document.offset_to_position(bytes.end).ok_or_else(failure)?;
        if document.position_to_offset(start) != Some(bytes.start)
            || document.position_to_offset(end) != Some(bytes.end)
        {
            return Err(failure());
        }
        Ok(Range::new(start, end))
    }

    pub(super) fn replace_cells(
        &mut self,
        range: Range,
        replacement: &str,
        cursor: CellReplacementCursor,
    ) -> Result<(), ComposerError> {
        self.editor
            .replace_cell_range(range, replacement, cursor)
            .map_err(|source| ComposerError::Cell {
                operation: "replacement",
                source,
            })
    }

    pub(super) fn handle_cell_key(
        &mut self,
        event: &KeyEvent,
        options: CellInputOptions,
    ) -> Result<EditorKeyResult, ComposerError> {
        self.editor
            .handle_cell_key(event, options)
            .map_err(|source| ComposerError::Cell {
                operation: "key",
                source,
            })
    }

    pub(super) fn run_cell_command(
        &mut self,
        id: &str,
        args: CommandArgs,
        options: CellInputOptions,
    ) -> Result<EditorKeyResult, ComposerError> {
        self.editor
            .run_cell_command(id, args, options)
            .map_err(|source| ComposerError::Cell {
                operation: "command",
                source,
            })
    }

    pub(super) fn paste_cells(&mut self, text: &str) -> Result<(), ComposerError> {
        self.editor
            .paste_cells(text)
            .map_err(|source| ComposerError::Cell {
                operation: "paste",
                source,
            })
    }

    pub(super) fn set_cell_pointer(
        &mut self,
        row: ScreenRow,
        column: CellColumn,
        extend: bool,
        options: CellInputOptions,
    ) -> Result<(), ComposerError> {
        self.editor
            .set_cell_pointer(row, column, extend, options)
            .map_err(|source| ComposerError::Cell {
                operation: "pointer",
                source,
            })
    }

    pub(super) fn set_config(&mut self, config: EditorConfig) {
        self.editor.set_config(config);
    }
}

#[cfg(test)]
#[path = "composer_kernel_tests.rs"]
mod tests;
