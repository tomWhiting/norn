//! Line-ending and trailing-newline preservation for patched files.
//!
//! Patch application works on LF-normalized text (patch text itself is
//! LF-delimited and `str::lines` strips `\r`). To keep patched files
//! byte-faithful outside the hunks, the original file's EOL style and
//! trailing-newline state are captured before application
//! ([`EolInfo::detect`]), the content is normalized to LF for matching
//! ([`EolInfo::normalize`]), and the patched result is re-encoded
//! afterwards ([`EolInfo::restore`]).
//!
//! CRLF mode engages only when the file contains at least one `\r\n` and
//! every `\n` is preceded by `\r` — a file with mixed endings is left in
//! LF processing (matching the previous behaviour) rather than guessing.

/// Captured end-of-line style and trailing-newline state of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EolInfo {
    /// Whether every newline in the original was `\r\n`.
    crlf: bool,
    /// Whether the original ended with a newline.
    trailing_newline: bool,
}

impl EolInfo {
    /// Inspects `original` and records its EOL style and trailing-newline
    /// state.
    pub(super) fn detect(original: &str) -> Self {
        let bytes = original.as_bytes();
        let mut saw_crlf = false;
        let mut saw_bare_lf = false;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                if i > 0 && bytes[i - 1] == b'\r' {
                    saw_crlf = true;
                } else {
                    saw_bare_lf = true;
                }
            }
        }
        Self {
            crlf: saw_crlf && !saw_bare_lf,
            trailing_newline: bytes.last() == Some(&b'\n'),
        }
    }

    /// Returns `original` with `\r\n` collapsed to `\n` when CRLF mode is
    /// active; otherwise returns the input unchanged.
    pub(super) fn normalize(self, original: &str) -> String {
        if self.crlf {
            original.replace("\r\n", "\n")
        } else {
            original.to_string()
        }
    }

    /// Returns a copy with the trailing-newline state overridden. Used when a
    /// unified diff's `\ No newline at end of file` markers declare the new
    /// side's final-newline state explicitly: the patch's declaration takes
    /// precedence over the state captured from the original file.
    pub(super) const fn with_trailing_newline(self, trailing_newline: bool) -> Self {
        Self {
            crlf: self.crlf,
            trailing_newline,
        }
    }

    /// Re-encodes patched LF content back to the original EOL style and
    /// trailing-newline state.
    pub(super) fn restore(self, patched_lf: &str) -> String {
        let mut out = patched_lf.to_string();
        if self.trailing_newline {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
        } else if out.ends_with('\n') {
            out.pop();
        }
        if self.crlf {
            out = out.replace('\n', "\r\n");
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn detects_lf_with_trailing_newline() {
        let info = EolInfo::detect("a\nb\n");
        assert!(!info.crlf);
        assert!(info.trailing_newline);
    }

    #[test]
    fn detects_crlf() {
        let info = EolInfo::detect("a\r\nb\r\n");
        assert!(info.crlf);
        assert!(info.trailing_newline);
    }

    #[test]
    fn mixed_endings_are_not_crlf_mode() {
        let info = EolInfo::detect("a\r\nb\nc\n");
        assert!(!info.crlf);
    }

    #[test]
    fn detects_missing_trailing_newline() {
        let info = EolInfo::detect("a\nb");
        assert!(!info.trailing_newline);
    }

    #[test]
    fn crlf_round_trip_is_byte_identical() {
        let original = "one\r\ntwo\r\nthree\r\n";
        let info = EolInfo::detect(original);
        let normalized = info.normalize(original);
        assert_eq!(normalized, "one\ntwo\nthree\n");
        assert_eq!(info.restore(&normalized), original);
    }

    #[test]
    fn no_trailing_newline_round_trip() {
        let original = "one\ntwo";
        let info = EolInfo::detect(original);
        // Patch application appends a trailing LF; restore strips it back.
        assert_eq!(info.restore("one\ntwo\n"), original);
    }

    #[test]
    fn crlf_without_trailing_newline_round_trip() {
        let original = "one\r\ntwo";
        let info = EolInfo::detect(original);
        let normalized = info.normalize(original);
        assert_eq!(info.restore(&format!("{normalized}\n")), original);
    }

    #[test]
    fn restore_adds_missing_trailing_newline() {
        let info = EolInfo::detect("a\n");
        assert_eq!(info.restore("b"), "b\n");
    }

    #[test]
    fn with_trailing_newline_overrides_detected_state() {
        // Detected: trailing newline present. Override to absent.
        let info = EolInfo::detect("a\nb\n").with_trailing_newline(false);
        assert_eq!(info.restore("a\nB\n"), "a\nB");

        // Detected: trailing newline absent. Override to present.
        let info = EolInfo::detect("a\nb").with_trailing_newline(true);
        assert_eq!(info.restore("a\nB\n"), "a\nB\n");
    }

    #[test]
    fn with_trailing_newline_preserves_crlf_mode() {
        let info = EolInfo::detect("a\r\nb\r\n").with_trailing_newline(false);
        assert_eq!(info.restore("a\nB\n"), "a\r\nB");
    }
}
