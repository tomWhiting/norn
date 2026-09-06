//! One Iridium editor plus independent submission recall; terminal geometry belongs to App.

use iridium_editor::cell_layout::{CellColumn, ScreenRow};
use iridium_editor::editor::CellInputOptions;
use iridium_editor::{CommandArgs, Editor, EditorConfig, EditorKeyResult, KeyEvent};

use super::composer_kernel::{ComposerError, ComposerKernel};
use super::composer_transactions::RecallSession;
use super::history::InputHistory;

/// Host facade owning the sole mutable composer document and editing history.
pub struct InputEditor {
    pub(super) kernel: ComposerKernel,
    pub(super) history: InputHistory,
    pub(super) recall: Option<RecallSession>,
}

impl std::fmt::Debug for InputEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputEditor")
            .field("kernel", &self.kernel)
            .field("history", &self.history)
            .field("recalling", &self.recall.is_some())
            .finish()
    }
}

impl InputEditor {
    /// Uses Iridium's declared editing defaults, with no document language.
    #[must_use]
    pub fn new(history: InputHistory) -> Self {
        Self::with_config(history, EditorConfig::default())
    }

    /// Constructs the plain composer using the explicitly resolved configuration.
    #[must_use]
    pub fn with_config(history: InputHistory, config: EditorConfig) -> Self {
        Self {
            kernel: ComposerKernel::new(config),
            history,
            recall: None,
        }
    }

    /// Borrows the exact kernel for cell preparation; no mutable view is exposed.
    #[must_use]
    pub fn kernel(&self) -> &Editor {
        self.kernel.editor()
    }

    /// Original bytes, allocated only when a host operation requests them.
    #[must_use]
    pub fn text(&self) -> String {
        self.kernel().content()
    }

    /// Whether every logical line is empty; spaces remain meaningful draft content.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kernel()
            .state()
            .document
            .rope()
            .chars()
            .all(|ch| matches!(ch, '\r' | '\n'))
    }

    /// Actual logical line count, including an empty final line.
    #[must_use]
    pub fn height(&self) -> usize {
        self.kernel().state().document.line_count()
    }

    /// Primary cursor in logical line and Unicode-scalar coordinates.
    #[must_use]
    pub fn cursor_position(&self) -> (usize, usize) {
        let cursor = self.kernel().cursor();
        (cursor.line, cursor.column)
    }

    /// Original byte offset of the current primary caret.
    pub fn cursor_byte_index(&self) -> Result<usize, ComposerError> {
        self.kernel.cursor_byte_index()
    }

    /// Scalar offset for the existing completion scanner, without copying text.
    pub fn cursor_char_index(&self) -> Result<usize, ComposerError> {
        let byte = self.cursor_byte_index()?;
        Ok(self.kernel().state().document.rope().byte_to_char_idx(byte))
    }

    /// Runs one unclaimed key through Iridium's cell-aware keymap.
    pub fn handle_cell_key(
        &mut self,
        event: &KeyEvent,
        options: CellInputOptions,
    ) -> Result<EditorKeyResult, ComposerError> {
        self.kernel.handle_cell_key(event, options)
    }

    /// Runs an already selected editing command; host results remain explicit.
    pub fn run_cell_command(
        &mut self,
        id: &str,
        args: CommandArgs,
        options: CellInputOptions,
    ) -> Result<EditorKeyResult, ComposerError> {
        self.kernel.run_cell_command(id, args, options)
    }

    /// Inserts one original Paste event as one gesture, never as send keys.
    pub fn paste_cells(&mut self, text: &str) -> Result<(), ComposerError> {
        self.kernel.paste_cells(text)
    }

    /// Applies a pointer using current geometry, never a previously saved hit.
    pub fn set_cell_pointer(
        &mut self,
        row: ScreenRow,
        column: CellColumn,
        extend: bool,
        options: CellInputOptions,
    ) -> Result<(), ComposerError> {
        self.kernel.set_cell_pointer(row, column, extend, options)
    }

    /// Changes editing settings without resetting document, selection or undo.
    pub fn set_config(&mut self, config: EditorConfig) {
        self.kernel.set_config(config);
    }
}
