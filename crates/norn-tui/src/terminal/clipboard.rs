//! Pure, explicitly permitted OSC 52 copy preparation; no terminal or clipboard I/O.

use std::fmt;

use norn::session_view::DisplayText;
use termina::escape::osc::{Osc, Selection};

/// The person's explicit clipboard transport preference, never a probe result.
/// The frontend configuration owner declares its initial value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardCapability {
    /// No clipboard transport has been selected; offer explicit export instead.
    Unspecified,
    /// The person has disabled terminal clipboard commands.
    Disabled,
    /// The person permits an OSC 52 write to the clipboard selection.
    Osc52,
}

/// Why preparation produced no terminal sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyUnavailable {
    /// Clipboard capability has not been explicitly selected.
    Unspecified,
    /// Terminal clipboard commands have been explicitly disabled.
    Disabled,
}

/// Preparation alone never means bytes were sent or the clipboard accepted them.
#[derive(Debug)]
pub(crate) enum CopyPreparation {
    /// No sequence exists; the frontend may offer an explicit export action.
    Unavailable(CopyUnavailable),
    /// The frame owner may write these bytes after checking its current state.
    Ready(PreparedCopy),
}

/// Sanitized, encoded copy bytes awaiting the sole terminal writer.
///
/// The writer must handle write and flush errors before reporting "sent".
/// OSC 52 has no acknowledgement here, so even a successful flush cannot be
/// reported as confirmed clipboard acceptance. Debug output contains lengths
/// only: base64 is an encoding, not protection for the selected content.
pub(crate) struct PreparedCopy {
    sequence: String,
    original_bytes: usize,
    sanitized_bytes: usize,
    content_changed: bool,
}

impl PreparedCopy {
    /// Bytes prepared for one clipboard-selection OSC 52 command.
    pub fn as_bytes(&self) -> &[u8] {
        self.sequence.as_bytes()
    }

    /// Original selected UTF-8 byte count, before visible control escaping.
    pub const fn original_bytes(&self) -> usize {
        self.original_bytes
    }

    /// Sanitized UTF-8 payload byte count, before OSC 52 base64 encoding.
    pub const fn sanitized_bytes(&self) -> usize {
        self.sanitized_bytes
    }

    /// Whether visible control escaping changed the actual clipboard content.
    /// Destructive cut must be refused when this is true, regardless of length.
    pub const fn content_changed(&self) -> bool {
        self.content_changed
    }
}

impl fmt::Debug for PreparedCopy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCopy")
            .field("sequence", &"[redacted]")
            .field("original_bytes", &self.original_bytes)
            .field("sanitized_bytes", &self.sanitized_bytes)
            .field("content_changed", &self.content_changed)
            .finish()
    }
}

/// Prepare original selected content only when OSC 52 is explicitly permitted.
///
/// The caller supplies the selection owner's freshly validated original bytes,
/// never rendered rows, styled spans or terminal cells. Consequently this path
/// introduces no soft-wrap newlines or UI decoration. Existing hard newlines,
/// tabs, Unicode and source markup survive. Other C0/C1 and bidi controls become
/// visible Unicode escapes through the shared `DisplayText` policy; they cannot
/// remain executable control bytes. Original export is a separate operation.
///
/// Unspecified and disabled preferences return before inspecting the content
/// or constructing an OSC sequence. This function neither reads nor writes a
/// terminal, probes its capabilities, or reads the clipboard or environment.
pub(crate) fn prepare_copy(
    capability: ClipboardCapability,
    original_selection: &str,
) -> CopyPreparation {
    match capability {
        ClipboardCapability::Unspecified => {
            CopyPreparation::Unavailable(CopyUnavailable::Unspecified)
        }
        ClipboardCapability::Disabled => CopyPreparation::Unavailable(CopyUnavailable::Disabled),
        ClipboardCapability::Osc52 => {
            let sanitized = DisplayText::new(original_selection);
            CopyPreparation::Ready(PreparedCopy {
                sequence: Osc::SetSelection(Selection::CLIPBOARD, sanitized.as_str()).to_string(),
                original_bytes: original_selection.len(),
                sanitized_bytes: sanitized.as_str().len(),
                content_changed: sanitized.as_str() != original_selection,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{
        ClipboardCapability, CopyPreparation, CopyUnavailable, PreparedCopy, prepare_copy,
    };

    fn prepared(original: &str) -> Result<PreparedCopy, io::Error> {
        match prepare_copy(ClipboardCapability::Osc52, original) {
            CopyPreparation::Ready(copy) => Ok(copy),
            CopyPreparation::Unavailable(reason) => Err(io::Error::other(format!(
                "explicit OSC 52 fixture preparation unavailable: {reason:?}"
            ))),
        }
    }

    #[test]
    fn unspecified_and_disabled_capabilities_produce_no_sequence() {
        let original = "private text\x1b]52;c;ZXZpbA==\x07";
        assert!(matches!(
            prepare_copy(ClipboardCapability::Unspecified, original),
            CopyPreparation::Unavailable(CopyUnavailable::Unspecified)
        ));
        assert!(matches!(
            prepare_copy(ClipboardCapability::Disabled, original),
            CopyPreparation::Unavailable(CopyUnavailable::Disabled)
        ));
    }

    #[test]
    fn explicit_copy_encodes_the_clipboard_target_and_string_terminator() -> Result<(), io::Error> {
        let copy = prepared("copied text")?;
        assert_eq!(copy.as_bytes(), b"\x1b]52;c;Y29waWVkIHRleHQ=\x1b\\");
        assert_eq!(copy.original_bytes(), 11);
        assert_eq!(copy.sanitized_bytes(), 11);
        assert!(!copy.content_changed());
        Ok(())
    }

    #[test]
    fn unicode_hard_newlines_and_tabs_are_preserved() -> Result<(), io::Error> {
        let original = "café\n👩‍💻\t終\n";
        let copy = prepared(original)?;
        assert_eq!(
            copy.as_bytes(),
            b"\x1b]52;c;Y2Fmw6kK8J+RqeKAjfCfkrsJ57WCCg==\x1b\\"
        );
        assert_eq!(copy.original_bytes(), original.len());
        assert_eq!(copy.sanitized_bytes(), original.len());
        Ok(())
    }

    #[test]
    fn original_source_markup_is_not_replaced_with_rendered_decoration() -> Result<(), io::Error> {
        let original = "```rust\nlet café = \"👩‍💻\";\n```";
        let copy = prepared(original)?;
        assert_eq!(
            copy.as_bytes(),
            b"\x1b]52;c;YGBgcnVzdApsZXQgY2Fmw6kgPSAi8J+RqeKAjfCfkrsiOwpgYGA=\x1b\\"
        );
        assert_eq!(copy.sanitized_bytes(), original.len());
        Ok(())
    }

    #[test]
    fn terminal_and_directional_controls_are_visible_data_inside_base64() -> Result<(), io::Error> {
        let original = "a\x1b]52;c;ZXZpbA==\x07b\x1b[31mred\x1b[0m\0\u{009b}\u{202e}\r\n";
        let sanitized =
            "a\\u{1b}]52;c;ZXZpbA==\\u{7}b\\u{1b}[31mred\\u{1b}[0m\\u{0}\\u{9b}\\u{202e}\\u{d}\n";
        let copy = prepared(original)?;
        assert_eq!(
            copy.as_bytes(),
            b"\x1b]52;c;YVx1ezFifV01MjtjO1pYWnBiQT09XHV7N31iXHV7MWJ9WzMxbXJlZFx1ezFifVswbVx1ezB9XHV7OWJ9XHV7MjAyZX1cdXtkfQo=\x1b\\"
        );
        assert_eq!(copy.original_bytes(), original.len());
        assert_eq!(copy.sanitized_bytes(), sanitized.len());
        assert!(copy.content_changed());
        let payload = copy
            .as_bytes()
            .strip_prefix(b"\x1b]52;c;")
            .and_then(|sequence| sequence.strip_suffix(b"\x1b\\"))
            .ok_or_else(|| {
                io::Error::other("OSC 52 control fixture has invalid command framing")
            })?;
        assert!(!payload.contains(&0x1b));
        assert!(!copy.as_bytes().contains(&0x07));
        Ok(())
    }

    #[test]
    fn preparation_debug_does_not_expose_original_or_encoded_content() -> Result<(), io::Error> {
        let copy = prepared("copied text")?;
        let diagnostic = format!("{copy:?}");
        assert!(diagnostic.contains("[redacted]"));
        assert!(!diagnostic.contains("copied text"));
        assert!(!diagnostic.contains("Y29waWVkIHRleHQ="));
        assert!(diagnostic.contains("original_bytes: 11"));
        assert!(diagnostic.contains("sanitized_bytes: 11"));
        Ok(())
    }
}
