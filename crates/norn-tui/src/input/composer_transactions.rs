//! Revision-bound host replacement, admission snapshots, completion and reversible recall.

use super::autocomplete::Acceptance;
use super::composer_kernel::ComposerError;
use super::editor::InputEditor;
use iridium_editor::editor::CellReplacementCursor;
use iridium_editor::history::Command;
use iridium_editor::{CursorState, Range};
use std::ops::Range as ByteRange;

#[derive(Clone, PartialEq, Eq)]
struct ComposerStamp {
    document: u64,
    revision: u64,
    cursor: CursorState,
}

impl ComposerStamp {
    fn capture(editor: &InputEditor) -> Self {
        let state = editor.kernel().state();
        Self {
            document: state.document.id(),
            revision: state.document.revision(),
            cursor: state.cursor.clone(),
        }
    }

    fn validate(&self, editor: &InputEditor) -> Result<(), ComposerError> {
        let state = editor.kernel().state();
        if self.document != state.document.id()
            || self.revision != state.document.revision()
            || self.cursor != state.cursor
        {
            return Err(ComposerError::StaleSnapshot {
                document: self.document,
                revision: self.revision,
            });
        }
        Ok(())
    }
}

/// Immutable original draft and exact selection, captured only at a host boundary.
/// This is neither a second editable document nor a runtime admission receipt.
pub struct ComposerSnapshot {
    stamp: ComposerStamp,
    text: String,
    primary_range: ByteRange<usize>,
}

impl std::fmt::Debug for ComposerSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposerSnapshot")
            .field("document", &self.stamp.document)
            .field("revision", &self.stamp.revision)
            .field("bytes", &self.text.len())
            .field("selection_count", &self.stamp.cursor.cursor_count())
            .field("primary_range", &self.primary_range)
            .finish()
    }
}

impl ComposerSnapshot {
    /// Exact original bytes; callers must not emit these as terminal controls.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Whether every original logical line is empty, matching the host's blank-send policy.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.chars().all(|ch| matches!(ch, '\r' | '\n'))
    }
    /// Complete original selection, including anchor direction and extra cursors.
    #[must_use]
    pub fn cursor(&self) -> &CursorState {
        &self.stamp.cursor
    }
    /// Original content revision, scoped to this snapshot's document identity.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.stamp.revision
    }
    /// Original document identity, independent of identical text in another editor.
    #[must_use]
    pub fn document_id(&self) -> u64 {
        self.stamp.document
    }
    /// Original byte range for a single nonempty selection; extras are explicit.
    pub fn selection(&self) -> Result<Option<ByteRange<usize>>, ComposerError> {
        let count = self.stamp.cursor.cursor_count();
        if count != 1 {
            return Err(ComposerError::MultipleSelections { count });
        }
        Ok((!self.primary_range.is_empty()).then(|| self.primary_range.clone()))
    }
}

/// A completion range minted against one actual document revision and caret.
#[derive(Clone, PartialEq, Eq)]
pub struct CompletionContext {
    stamp: ComposerStamp,
    range: ByteRange<usize>,
}

impl std::fmt::Debug for CompletionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletionContext")
            .field("document", &self.stamp.document)
            .field("revision", &self.stamp.revision)
            .field("range", &self.range)
            .finish()
    }
}

impl CompletionContext {
    /// Original trigger start, never recomputed from a later cursor.
    #[must_use]
    pub fn trigger_start_byte(&self) -> usize {
        self.range.start
    }
    /// Original end of the prefix being completed.
    #[must_use]
    pub fn cursor_byte(&self) -> usize {
        self.range.end
    }
}

/// Private retained recall navigation, not a mutable composer text mirror.
pub(super) struct RecallSession {
    index: usize,
    draft: ComposerSnapshot,
}

/// Prepared authoritative clipboard result, private until transport succeeds.
pub struct PreparedComposerCut {
    stamp: ComposerStamp,
    text: String,
    cursor: CursorState,
}

impl std::fmt::Debug for PreparedComposerCut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedComposerCut")
            .field("document", &self.stamp.document)
            .field("revision", &self.stamp.revision)
            .field("bytes", &self.text.len())
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl InputEditor {
    /// Captures exact original text and selections without mutating editor or history.
    pub fn snapshot(&self) -> Result<ComposerSnapshot, ComposerError> {
        let state = self.kernel().state();
        let range = state.cursor.primary.range();
        let start =
            state
                .document
                .position_to_offset(range.start)
                .ok_or(ComposerError::Position {
                    position: range.start,
                })?;
        let end = state
            .document
            .position_to_offset(range.end)
            .ok_or(ComposerError::Position {
                position: range.end,
            })?;
        Ok(ComposerSnapshot {
            stamp: ComposerStamp::capture(self),
            text: self.text(),
            primary_range: start..end,
        })
    }

    /// Checks source, content revision and all selections before a host effect.
    pub fn validate_snapshot(&self, snapshot: &ComposerSnapshot) -> Result<(), ComposerError> {
        snapshot.stamp.validate(self)
    }

    /// Original-range replacement through ICC's single reversible transaction.
    pub fn replace_cells(
        &mut self,
        range: Range,
        text: &str,
        cursor: CellReplacementCursor,
    ) -> Result<(), ComposerError> {
        self.kernel.replace_cells(range, text, cursor)?;
        self.recall = None;
        Ok(())
    }

    /// Replaces exact snapshot bytes only if its entire original state is still current.
    pub fn replace_snapshot_range(
        &mut self,
        snapshot: &ComposerSnapshot,
        range: ByteRange<usize>,
        text: &str,
    ) -> Result<(), ComposerError> {
        self.validate_snapshot(snapshot)?;
        let range = self.kernel.byte_range(range)?;
        self.replace_cells(range, text, CellReplacementCursor::EndOfReplacement)
    }

    /// Explicit clear is undoable; failure leaves recall and editor unchanged.
    pub fn clear(&mut self) -> Result<(), ComposerError> {
        let range = self.kernel.whole_range()?;
        self.replace_cells(range, "", CellReplacementCursor::EndOfReplacement)
    }

    /// Clears the accepted original draft, refusing to erase any newer user edit.
    /// The caller already owns runtime acceptance; this result cannot undo admission.
    pub fn clear_accepted(&mut self, snapshot: &ComposerSnapshot) -> Result<(), ComposerError> {
        self.validate_snapshot(snapshot)?;
        self.clear()
    }

    /// Records an already accepted non-secret input once at the caller's receipt boundary.
    /// Never dispatches, clears or pretends a recall-write failure rejected admission.
    pub fn record_accepted(&mut self, snapshot: &ComposerSnapshot) -> Result<(), ComposerError> {
        if snapshot.stamp.document != self.kernel().state().document.id() {
            return Err(ComposerError::StaleSnapshot {
                document: snapshot.stamp.document,
                revision: snapshot.stamp.revision,
            });
        }
        self.history
            .append(snapshot.text())
            .map_err(|source| ComposerError::History {
                path: self.history.path(),
                source,
            })
    }

    /// Mints a completion range; a non-caret selection is explicitly unavailable.
    pub fn completion_context(
        &self,
        trigger_start_byte: usize,
    ) -> Result<CompletionContext, ComposerError> {
        let state = self.kernel().state();
        let count = state.cursor.cursor_count();
        if count != 1 {
            return Err(ComposerError::MultipleSelections { count });
        }
        if !state.cursor.primary.is_collapsed() {
            return Err(ComposerError::ActiveSelection);
        }
        let range = trigger_start_byte..self.cursor_byte_index()?;
        self.kernel.byte_range(range.clone())?;
        Ok(CompletionContext {
            stamp: ComposerStamp::capture(self),
            range,
        })
    }

    /// Applies the selected original prefix transaction, refusing stale state explicitly.
    pub fn apply_acceptance(&mut self, acceptance: &Acceptance) -> Result<(), ComposerError> {
        acceptance.context.stamp.validate(self)?;
        let range = self.kernel.byte_range(acceptance.context.range.clone())?;
        self.replace_cells(
            range,
            &acceptance.replacement,
            CellReplacementCursor::EndOfReplacement,
        )
    }

    /// Recalls an older entry in one undo gesture, preserving the initial draft selection.
    pub fn history_prev(&mut self) -> Result<bool, ComposerError> {
        if self.history.is_empty() {
            return Ok(false);
        }
        let index = self
            .recall
            .as_ref()
            .map_or(self.history.len() - 1, |session| {
                session.index.saturating_sub(1)
            });
        let draft = if self.recall.is_none() {
            Some(self.snapshot()?)
        } else {
            None
        };
        let range = self.kernel.whole_range()?;
        let text = self
            .history
            .entry(index)
            .ok_or(ComposerError::HistoryEntry { index })?;
        self.kernel
            .replace_cells(range, text, CellReplacementCursor::EndOfReplacement)?;
        if let Some(draft) = draft {
            self.recall = Some(RecallSession { index, draft });
        } else if let Some(session) = &mut self.recall {
            session.index = index;
        }
        Ok(true)
    }

    /// Moves forward through recall, then restores exact pre-navigation bytes and selection.
    pub fn history_next(&mut self) -> Result<bool, ComposerError> {
        let Some(session) = &self.recall else {
            return Ok(false);
        };
        let range = self.kernel.whole_range()?;
        let next = session.index + 1;
        if next < self.history.len() {
            let text = self
                .history
                .entry(next)
                .ok_or(ComposerError::HistoryEntry { index: next })?;
            self.kernel
                .replace_cells(range, text, CellReplacementCursor::EndOfReplacement)?;
            if let Some(session) = &mut self.recall {
                session.index = next;
            }
        } else {
            self.kernel.replace_cells(
                range,
                session.draft.text(),
                CellReplacementCursor::Exact(session.draft.stamp.cursor.clone()),
            )?;
            self.recall = None;
        }
        Ok(true)
    }

    /// Preflights the kernel's authoritative Cut command on private, explicit-gesture copies.
    /// It never infers line-copy or multiple-selection semantics from visible text.
    pub fn prepare_clipboard_cut(
        &self,
        snapshot: &ComposerSnapshot,
        command: &Command,
    ) -> Result<PreparedComposerCut, ComposerError> {
        self.validate_snapshot(snapshot)?;
        let mut document = self.kernel().state().document.clone();
        let mut cursor = self.kernel().state().cursor.clone();
        command
            .apply(&mut document, &mut cursor)
            .map_err(|source| ComposerError::Clipboard { source })?;
        Ok(PreparedComposerCut {
            stamp: snapshot.stamp.clone(),
            text: document.text(),
            cursor,
        })
    }

    /// Commits after successful clipboard transport; stale preparation never erases newer input.
    pub fn commit_clipboard_cut(
        &mut self,
        snapshot: &ComposerSnapshot,
        prepared: PreparedComposerCut,
    ) -> Result<(), ComposerError> {
        self.validate_snapshot(snapshot)?;
        prepared.stamp.validate(self)?;
        let range = self.kernel.whole_range()?;
        self.replace_cells(
            range,
            &prepared.text,
            CellReplacementCursor::Exact(prepared.cursor),
        )
    }
}

#[cfg(test)]
#[path = "composer_transactions_tests.rs"]
mod tests;
