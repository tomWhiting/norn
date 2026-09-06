//! Source-bound composer clipboard preparation and cut-after-flush through the sole borrowed writer.

use std::fmt;
use std::io::{self, Write};

use iridium_editor::input::keyboard::ClipboardOperation;

use super::{ComposerError, ComposerSnapshot, InputEditor, PreparedComposerCut};
use crate::terminal::clipboard::{
    ClipboardCapability, CopyPreparation, CopyUnavailable, PreparedCopy, prepare_copy,
};

/// A clipboard request that cannot use the currently authorized transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposerClipboardUnavailable {
    /// OSC 52 was unspecified or explicitly disabled.
    Transport(CopyUnavailable),
    /// Norn does not read the host clipboard; terminal Paste events remain supported.
    PasteRead,
}

/// Preparation never changes the document, undo tree or external clipboard.
#[derive(Debug)]
pub(crate) enum ComposerClipboardPreparation {
    /// No transport bytes were prepared.
    Unavailable(ComposerClipboardUnavailable),
    /// Escaping changed copied text, so deleting the original would lose data.
    SanitizedCut,
    /// Exact payload and optional deletion await the existing terminal owner.
    Ready(Box<PreparedComposerClipboard>),
}

/// The stage that failed before a destructive cut could be committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardTransportStage {
    Write,
    Flush,
}

/// Clipboard failures never include original text, commands or encoded payloads.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ComposerClipboardError {
    /// Preparation or pre-transport validation refused the source snapshot.
    #[error("composer clipboard request refused: {0}")]
    Composer(#[from] ComposerError),
    /// A partial write may have occurred; the draft is never deleted on failure.
    #[error("clipboard {stage:?} failed ({kind:?}, OS code {os_code:?}); composer text retained")]
    Transport {
        stage: ClipboardTransportStage,
        kind: io::ErrorKind,
        os_code: Option<i32>,
    },
    /// The terminal write succeeded but the exact edit could not be committed.
    #[error("clipboard sequence sent without acknowledgement; cut not applied: {0}")]
    SentButCutRefused(ComposerError),
}

/// Successful write and flush only; OSC 52 supplies no clipboard acknowledgement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ComposerClipboardSent {
    /// Whether the exact prepared cut was also committed to the composer.
    pub cut_applied: bool,
    /// Original kernel-produced clipboard bytes, including its line-copy semantics.
    pub original_bytes: usize,
    /// Sanitized clipboard payload size, before base64 and OSC framing.
    pub payload_bytes: usize,
    /// Copy may escape controls; a cut with changed content is refused before writing.
    pub content_changed: bool,
}

/// The immutable snapshot and scratch-prepared cut are not an editing authority.
pub(crate) struct PreparedComposerClipboard {
    snapshot: ComposerSnapshot,
    copy: PreparedCopy,
    cut: Option<PreparedComposerCut>,
}

impl fmt::Debug for PreparedComposerClipboard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedComposerClipboard")
            .field("snapshot", &"[redacted]")
            .field("copy", &self.copy)
            .field("cut_prepared", &self.cut.is_some())
            .finish()
    }
}

impl PreparedComposerClipboard {
    /// Borrow the sole existing terminal writer; never open a device or spawn a helper.
    /// Validation precedes all writes, and only successful write plus flush permits
    /// the snapshot-checked, single-transaction cut. Copy never edits the composer.
    pub fn send(
        self,
        editor: &mut InputEditor,
        writer: &mut impl Write,
    ) -> Result<ComposerClipboardSent, ComposerClipboardError> {
        editor.validate_snapshot(&self.snapshot)?;
        writer.write_all(self.copy.as_bytes()).map_err(|source| {
            ComposerClipboardError::Transport {
                stage: ClipboardTransportStage::Write,
                kind: source.kind(),
                os_code: source.raw_os_error(),
            }
        })?;
        writer
            .flush()
            .map_err(|source| ComposerClipboardError::Transport {
                stage: ClipboardTransportStage::Flush,
                kind: source.kind(),
                os_code: source.raw_os_error(),
            })?;
        let cut_applied = self.cut.is_some();
        if let Some(cut) = self.cut {
            editor
                .commit_clipboard_cut(&self.snapshot, cut)
                .map_err(ComposerClipboardError::SentButCutRefused)?;
        }
        Ok(ComposerClipboardSent {
            cut_applied,
            original_bytes: self.copy.original_bytes(),
            payload_bytes: self.copy.sanitized_bytes(),
            content_changed: self.copy.content_changed(),
        })
    }
}

/// Preserve the actual kernel clipboard operation, including whole-line copy,
/// trailing-newline cut and multiple selections. No selection rules are rebuilt here.
pub(crate) fn prepare_composer_clipboard(
    editor: &InputEditor,
    snapshot: ComposerSnapshot,
    operation: ClipboardOperation,
    capability: ClipboardCapability,
) -> Result<ComposerClipboardPreparation, ComposerClipboardError> {
    let (text, command) = match operation {
        ClipboardOperation::Copy(text) => (text, None),
        ClipboardOperation::Cut { text, command } => (text, Some(command)),
        ClipboardOperation::Paste => {
            return Ok(ComposerClipboardPreparation::Unavailable(
                ComposerClipboardUnavailable::PasteRead,
            ));
        }
    };
    let copy = match prepare_copy(capability, &text) {
        CopyPreparation::Unavailable(reason) => {
            return Ok(ComposerClipboardPreparation::Unavailable(
                ComposerClipboardUnavailable::Transport(reason),
            ));
        }
        CopyPreparation::Ready(copy) => copy,
    };
    if command.is_some() && copy.content_changed() {
        return Ok(ComposerClipboardPreparation::SanitizedCut);
    }
    editor.validate_snapshot(&snapshot)?;
    let cut = command
        .as_ref()
        .map(|command| editor.prepare_clipboard_cut(&snapshot, command))
        .transpose()?;
    Ok(ComposerClipboardPreparation::Ready(Box::new(
        PreparedComposerClipboard {
            snapshot,
            copy,
            cut,
        },
    )))
}

#[cfg(test)]
#[path = "composer_clipboard_tests.rs"]
mod tests;
