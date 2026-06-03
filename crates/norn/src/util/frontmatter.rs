//! Shared YAML frontmatter splitter.
//!
//! Splits a markdown document with a leading `---`/`---` block into the
//! YAML section and the body, returning borrowed slices when the input is
//! well-formed. Callers (profile loader, rules parser, future skills
//! loader) map [`FrontmatterError`] to their own error types.

/// Frontmatter splitter errors.
#[derive(Debug, thiserror::Error)]
pub enum FrontmatterError {
    /// The document does not begin with a well-formed `---` opening
    /// delimiter (missing entirely, missing trailing newline, or extra
    /// characters after the dashes).
    #[error("{reason}")]
    MissingOpening {
        /// Diagnostic naming the specific failure mode.
        reason: String,
    },

    /// The closing `---` delimiter could not be located before
    /// end-of-input.
    #[error("{reason}")]
    MissingClosing {
        /// Diagnostic naming the specific failure mode.
        reason: String,
    },
}

/// Split markdown content into its YAML frontmatter and body.
///
/// Expects the content to start with `---` followed by a newline (LF or
/// CRLF), then YAML, then `---` on its own line, then the body. Returns
/// `(frontmatter_yaml, body)` as borrowed slices of the input — no
/// allocation occurs on the happy path.
///
/// Edge cases handled verbatim:
/// - CRLF (`\r\n`) line endings on both delimiters.
/// - Empty frontmatter (`---\n---\n...`).
/// - Empty body (`---\nyaml\n---\n` or `---\nyaml\n---` with no trailing newline).
/// - CRLF-terminated closing line (trailing `\r` stripped from yaml slice).
///
/// # Errors
///
/// Returns [`FrontmatterError::MissingOpening`] when the opening `---`
/// delimiter is missing, lacks a trailing newline, or has extra characters
/// on its line. Returns [`FrontmatterError::MissingClosing`] when no
/// closing `\n---` delimiter is found before end-of-input.
pub fn split_frontmatter(content: &str) -> Result<(&str, &str), FrontmatterError> {
    let trimmed = content.trim_start();

    // The content must open with exactly `---` on its own line. Anything
    // else — including `----`, `---foo`, or content with no delimiter at
    // all — is malformed frontmatter.
    if !trimmed.starts_with("---") {
        return Err(FrontmatterError::MissingOpening {
            reason: "missing opening --- delimiter".to_owned(),
        });
    }
    let after_dashes = &trimmed[3..];
    let after_opening = if let Some(rest) = after_dashes.strip_prefix("\r\n") {
        rest
    } else if let Some(rest) = after_dashes.strip_prefix('\n') {
        rest
    } else if after_dashes.is_empty() {
        return Err(FrontmatterError::MissingOpening {
            reason: "missing newline after opening ---".to_owned(),
        });
    } else {
        // E.g. `---foo` or `----` — not a valid opener.
        return Err(FrontmatterError::MissingOpening {
            reason: "unexpected characters after opening ---".to_owned(),
        });
    };

    // Empty frontmatter: closing `---` immediately follows opening.
    if let Some(rest) = after_opening.strip_prefix("---\r\n") {
        return Ok(("", rest));
    }
    if let Some(rest) = after_opening.strip_prefix("---\n") {
        return Ok(("", rest));
    }
    if after_opening == "---" {
        return Ok(("", ""));
    }

    let Some(pos) = after_opening.find("\n---") else {
        return Err(FrontmatterError::MissingClosing {
            reason: "missing closing --- delimiter".to_owned(),
        });
    };
    // Strip trailing `\r` if the closing line was CRLF-terminated upstream.
    let frontmatter = after_opening[..pos].trim_end_matches('\r');
    let rest = &after_opening[pos + 4..]; // skip the "\n---" we found
    let body = if let Some(b) = rest.strip_prefix("\r\n") {
        b
    } else if let Some(b) = rest.strip_prefix('\n') {
        b
    } else {
        rest
    };
    Ok((frontmatter, body))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args
)]
mod tests {
    use super::*;

    /// Covers the basic happy-path, CRLF line endings, empty body, and
    /// empty frontmatter edge cases.
    #[test]
    fn split_frontmatter_handles_basic_crlf_and_empty_edges() {
        let (yaml, body) = split_frontmatter("---\nname: x\n---\nbody text\n").unwrap();
        assert_eq!(yaml, "name: x");
        assert_eq!(body, "body text\n");

        let (yaml, body) = split_frontmatter("---\r\nname: crlf\r\n---\r\nbody\r\n").unwrap();
        assert_eq!(yaml, "name: crlf");
        assert_eq!(body, "body\r\n");

        let (yaml, body) = split_frontmatter("---\nname: x\n---\n").unwrap();
        assert_eq!(yaml, "name: x");
        assert!(body.is_empty());

        let (yaml, body) = split_frontmatter("---\n---\nbody only\n").unwrap();
        assert!(yaml.is_empty());
        assert_eq!(body, "body only\n");
    }

    #[test]
    fn split_frontmatter_missing_closing_returns_error() {
        let err = split_frontmatter("---\nname: x\nbody text\n").unwrap_err();
        match err {
            FrontmatterError::MissingClosing { reason } => {
                assert!(reason.contains("closing"), "got: {reason}");
            }
            FrontmatterError::MissingOpening { .. } => {
                panic!("expected MissingClosing, got MissingOpening")
            }
        }
    }

    #[test]
    fn split_frontmatter_missing_opening_returns_error() {
        let err = split_frontmatter("just markdown, no frontmatter").unwrap_err();
        match err {
            FrontmatterError::MissingOpening { reason } => {
                assert!(reason.contains("opening"), "got: {reason}");
            }
            FrontmatterError::MissingClosing { .. } => {
                panic!("expected MissingOpening, got MissingClosing")
            }
        }
    }

    /// Bare `---` with no newline is opening malformed (covers the
    /// `after_dashes.is_empty()` branch).
    #[test]
    fn split_frontmatter_bare_dashes_only_is_missing_opening() {
        let err = split_frontmatter("---").unwrap_err();
        match err {
            FrontmatterError::MissingOpening { reason } => {
                assert!(reason.contains("newline"), "got: {reason}");
            }
            FrontmatterError::MissingClosing { .. } => {
                panic!("expected MissingOpening for bare dashes")
            }
        }
    }

    /// `----` or `---foo` on the first line is opening malformed.
    #[test]
    fn split_frontmatter_extra_chars_after_dashes_is_missing_opening() {
        let err = split_frontmatter("---foo\nbody\n").unwrap_err();
        match err {
            FrontmatterError::MissingOpening { reason } => {
                assert!(reason.contains("unexpected"), "got: {reason}");
            }
            FrontmatterError::MissingClosing { .. } => {
                panic!("expected MissingOpening for extra characters")
            }
        }
    }

    /// `---\nyaml\n---` with no body and no trailing newline is valid —
    /// hits the `after_opening == "---"` early-return branch for empty
    /// frontmatter and the same shape for non-empty.
    #[test]
    fn split_frontmatter_empty_frontmatter_no_body() {
        let (yaml, body) = split_frontmatter("---\n---").unwrap();
        assert!(yaml.is_empty());
        assert!(body.is_empty());
    }
}
