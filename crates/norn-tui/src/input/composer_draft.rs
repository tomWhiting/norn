//! Owned pending/recoverable drafts; submission recall stays with the active editor.

use super::composer_kernel::ComposerKernel;
use super::composer_transactions::RecallSession;
use super::{ComposerError, ComposerSnapshot, InputEditor};

/// An original draft and its undo/selection state, with no second recall store.
/// Owning this value is not admission, persistence or permission to resend it.
pub struct DetachedComposerDraft {
    kernel: ComposerKernel,
    recall: Option<RecallSession>,
}

impl std::fmt::Debug for DetachedComposerDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DetachedComposerDraft")
            .field("kernel", &self.kernel)
            .field("recalling", &self.recall.is_some())
            .finish()
    }
}

impl DetachedComposerDraft {
    /// Check exact source, content revision and complete selection before admission.
    ///
    /// # Errors
    /// Returns a stale-snapshot error without altering either draft.
    pub fn validate_snapshot(&self, snapshot: &ComposerSnapshot) -> Result<(), ComposerError> {
        snapshot.validate_kernel(self.kernel.editor())
    }
}

impl InputEditor {
    /// Move the exact current draft aside and begin a separately owned blank one.
    /// The recall store stays here; no editing history or file handle is cloned.
    ///
    /// # Errors
    /// A stale snapshot refuses the move before either editor is changed.
    pub fn detach_draft(
        &mut self,
        snapshot: &ComposerSnapshot,
    ) -> Result<DetachedComposerDraft, ComposerError> {
        self.validate_snapshot(snapshot)?;
        let next = ComposerKernel::new(self.kernel().get_config().clone());
        Ok(DetachedComposerDraft {
            kernel: std::mem::replace(&mut self.kernel, next),
            recall: self.recall.take(),
        })
    }

    /// Exchange entire draft owners without losing either document's undo state.
    /// Current editing preferences apply to the newly active draft as well.
    pub fn exchange_draft(&mut self, other: &mut DetachedComposerDraft) {
        let config = self.kernel().get_config().clone();
        std::mem::swap(&mut self.kernel, &mut other.kernel);
        std::mem::swap(&mut self.recall, &mut other.recall);
        self.kernel.set_config(config);
    }

    /// Record an actually accepted detached input in the single recall store.
    /// This neither admits input nor modifies the current draft or its undo tree.
    ///
    /// # Errors
    /// Refuses a foreign snapshot, or reports the recall write failure after validation.
    pub fn record_detached_accepted(
        &mut self,
        draft: &DetachedComposerDraft,
        snapshot: &ComposerSnapshot,
    ) -> Result<(), ComposerError> {
        draft.validate_snapshot(snapshot)?;
        self.record_validated_snapshot(snapshot)
    }
}

#[cfg(test)]
#[path = "composer_draft_tests.rs"]
mod tests;
