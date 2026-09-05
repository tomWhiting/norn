//! Syntect-backed syntax highlighting for fenced code blocks.
//!
//! [`SyntaxHighlighter`] owns syntect's default [`SyntaxSet`] (the
//! compressed binary dump shipping ~100 languages) and [`ThemeSet`]. It
//! resolves the grammar by language hint with a fallback to
//! `find_syntax_by_first_line` for unlabeled blocks, then routes each
//! coloured chunk through [`colour_for`] so output adapts to terminal
//! capabilities (24-bit RGB on true-colour terminals; 256-colour palette
//! on baseline terminals).
//! [`SyntaxHighlighter::highlight_spans`] instead returns original-byte styles
//! for retained rendering; it never emits terminal escapes or reloads the bundles.

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use termina::escape::csi::{Csi, Sgr};
use termina::style::{Intensity, RgbColor, Underline};

use super::retained_text::{StyleSpan, TextAttribute, TextStyle};
use super::style::colour_for;
use crate::terminal::caps::TerminalCaps;

/// Default theme name selected when present in the bundled set.
const DEFAULT_THEME: &str = "base16-ocean.dark";

/// Stateless syntax highlighter wrapping syntect's defaults.
pub struct SyntaxHighlighter {
    /// Bundled grammar set (compressed binary dump).
    syntaxes: SyntaxSet,
    /// Resolved theme — never empty; first available theme is used when
    /// the bundled default cannot be located.
    theme: Theme,
}

/// Direct highlighting failures retain byte positions without emitting partial text.
#[derive(Debug, thiserror::Error)]
pub enum SyntaxError {
    /// The bundled grammar could not highlight this source line.
    #[error("syntax highlighting failed at code byte {offset}: {source}")]
    Highlight {
        /// Original code-byte position at the beginning of the failed line.
        offset: usize,
        /// Actual grammar/highlight failure.
        #[source]
        source: syntect::Error,
    },
    /// Highlight tokens did not cover the supplied original bytes exactly.
    #[error("syntax highlight coverage differs from original code at byte {offset}")]
    Coverage {
        /// First byte at which exact coverage was lost.
        offset: usize,
    },
}

impl SyntaxHighlighter {
    /// Build a highlighter from syntect's bundled defaults.
    ///
    /// Loads the default [`SyntaxSet`] (newline-preserving variant —
    /// required by [`LinesWithEndings`]) and selects the bundled
    /// `base16-ocean.dark` theme, falling back to the first available
    /// theme if the default is missing. Returns a default-constructed
    /// [`Theme`] in the degenerate case where the bundled [`ThemeSet`] is
    /// empty — keeps the API total and avoids panicking in library code.
    #[must_use]
    pub fn new() -> Self {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .get(DEFAULT_THEME)
            .or_else(|| themes.themes.values().next())
            .cloned()
            .unwrap_or_default();
        Self { syntaxes, theme }
    }

    /// Highlight original code as direct styles using this already-loaded grammar/theme owner.
    /// Ranges refer to unescaped input bytes; the display adapter escapes controls afterward.
    /// Newlines and parser state are preserved across the complete supplied code block.
    ///
    /// # Errors
    /// Propagates grammar errors and refuses any token coverage mismatch.
    pub fn highlight_spans(
        &self,
        code: &str,
        language: Option<&str>,
    ) -> Result<Vec<StyleSpan>, SyntaxError> {
        let syntax = language
            .and_then(|hint| self.syntaxes.find_syntax_by_token(hint))
            .or_else(|| self.syntaxes.find_syntax_by_first_line(code))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut spans: Vec<StyleSpan> = Vec::new();
        let mut offset = 0;
        for line in LinesWithEndings::from(code) {
            let tokens = highlighter
                .highlight_line(line, &self.syntaxes)
                .map_err(|source| SyntaxError::Highlight { offset, source })?;
            for (style, text) in tokens {
                let end = offset
                    .checked_add(text.len())
                    .ok_or(SyntaxError::Coverage { offset })?;
                if code.get(offset..end) != Some(text) {
                    return Err(SyntaxError::Coverage { offset });
                }
                if end == offset {
                    continue;
                }
                let mut direct = TextStyle {
                    foreground: Some([style.foreground.r, style.foreground.g, style.foreground.b]),
                    ..TextStyle::default()
                };
                for (syntax, attribute) in [
                    (FontStyle::BOLD, TextAttribute::Bold),
                    (FontStyle::ITALIC, TextAttribute::Italic),
                    (FontStyle::UNDERLINE, TextAttribute::Underline),
                ] {
                    if style.font_style.contains(syntax) {
                        direct.attributes = direct.attributes.with(attribute);
                    }
                }
                match spans.last_mut() {
                    Some(previous) if previous.style == direct => previous.range.end = end,
                    _ => spans.push(StyleSpan {
                        range: offset..end,
                        style: direct,
                    }),
                }
                offset = end;
            }
        }
        if offset != code.len() {
            return Err(SyntaxError::Coverage { offset });
        }
        Ok(spans)
    }

    /// Highlight `code` using `language` as a hint.
    ///
    /// Resolution order:
    /// 1. `find_syntax_by_token(language)` when `language` is `Some`.
    /// 2. `find_syntax_by_first_line(code)` when the hint is absent or
    ///    unrecognised.
    /// 3. `find_syntax_plain_text()` — always present; renders without
    ///    syntactic styling.
    ///
    /// Returned string contains ANSI SGR escapes for foreground colour
    /// (via [`colour_for`], which routes through the capability-aware
    /// palette) and font-style attributes. Each emitted chunk ends with
    /// an SGR reset so colour state cannot leak past the highlighted
    /// region.
    ///
    /// # Errors
    /// Returns the original grammar error and code-line offset without partial output.
    pub fn highlight(
        &self,
        code: &str,
        language: Option<&str>,
        caps: &TerminalCaps,
    ) -> Result<String, SyntaxError> {
        let syntax = language
            .and_then(|hint| self.syntaxes.find_syntax_by_token(hint))
            .or_else(|| self.syntaxes.find_syntax_by_first_line(code))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());

        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut output = String::with_capacity(code.len());
        let mut offset = 0;

        for line in LinesWithEndings::from(code) {
            let ranges = highlighter
                .highlight_line(line, &self.syntaxes)
                .map_err(|source| SyntaxError::Highlight { offset, source })?;
            for (style, text) in ranges {
                let colour =
                    RgbColor::new(style.foreground.r, style.foreground.g, style.foreground.b);
                output.push_str(&colour_for(colour, caps));
                if style.font_style.contains(FontStyle::BOLD) {
                    output.push_str(&Csi::Sgr(Sgr::Intensity(Intensity::Bold)).to_string());
                }
                if style.font_style.contains(FontStyle::ITALIC) {
                    output.push_str(&Csi::Sgr(super::style::italic(caps)).to_string());
                }
                if style.font_style.contains(FontStyle::UNDERLINE) {
                    output.push_str(&Csi::Sgr(Sgr::Underline(Underline::Single)).to_string());
                }
                output.push_str(text);
                output.push_str(&Csi::Sgr(Sgr::Reset).to_string());
            }
            offset += line.len();
        }
        Ok(output)
    }
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A real grammar with an unresolved push, failing only when `!` is parsed.
    pub(crate) fn failing_highlighter()
    -> Result<SyntaxHighlighter, syntect::parsing::ParseSyntaxError> {
        let syntax = syntect::parsing::SyntaxDefinition::load_from_str(
            "name: failure\nscope: source.failure\nfile_extensions: [failure]\ncontexts:\n  main:\n    - match: '!'\n      push: missing-context\n",
            true,
            None,
        )?;
        let mut builder = syntect::parsing::SyntaxSetBuilder::new();
        builder.add_plain_text_syntax();
        builder.add(syntax);
        let mut highlighter = SyntaxHighlighter::new();
        highlighter.syntaxes = builder.build();
        Ok(highlighter)
    }

    #[test]
    fn grammar_failure_keeps_typed_cause_and_line_offset_without_partial_success()
    -> Result<(), Box<dyn std::error::Error>> {
        let highlighter = failing_highlighter()?;
        let original = "safe\n!\nfollowing\n";
        let error = highlighter
            .highlight(original, Some("failure"), &TerminalCaps::baseline())
            .err()
            .ok_or("malformed grammar unexpectedly produced successful ANSI output")?;
        assert!(std::error::Error::source(&error).is_some());
        assert!(matches!(
            error,
            SyntaxError::Highlight {
                offset: 5,
                source: syntect::Error::ParsingError(
                    syntect::parsing::ParsingError::UnresolvedContextReference(_)
                ),
            }
        ));
        assert_eq!(original, "safe\n!\nfollowing\n");
        Ok(())
    }

    #[test]
    fn rust_keyword_gets_distinct_foreground_with_true_colour()
    -> Result<(), Box<dyn std::error::Error>> {
        let caps = {
            let mut c = TerminalCaps::baseline();
            c.true_colour = true;
            c
        };
        let h = SyntaxHighlighter::new();
        let out = h.highlight("fn main() { let x = 1; }\n", Some("rust"), &caps)?;
        assert!(out.contains("38;2;"), "expected truecolor escape: {out:?}");
        Ok(())
    }

    #[test]
    fn baseline_falls_back_to_256_colour_palette() -> Result<(), Box<dyn std::error::Error>> {
        let caps = TerminalCaps::baseline();
        let h = SyntaxHighlighter::new();
        let out = h.highlight("fn main() {}\n", Some("rust"), &caps)?;
        assert!(out.contains("38;5;"), "expected palette escape: {out:?}");
        Ok(())
    }

    #[test]
    fn unknown_language_falls_back_via_first_line() -> Result<(), Box<dyn std::error::Error>> {
        let caps = TerminalCaps::baseline();
        let h = SyntaxHighlighter::new();
        // Shebang triggers first-line detection.
        let out = h.highlight("#!/bin/bash\necho hi\n", None, &caps)?;
        assert!(!out.is_empty());
        Ok(())
    }

    #[test]
    fn plain_text_when_nothing_matches() -> Result<(), Box<dyn std::error::Error>> {
        let caps = TerminalCaps::baseline();
        let h = SyntaxHighlighter::new();
        let out = h.highlight("just some text\n", Some("zzznotalanguage"), &caps)?;
        assert!(out.contains("just some text"));
        Ok(())
    }

    #[test]
    fn each_chunk_terminates_with_reset() -> Result<(), Box<dyn std::error::Error>> {
        let caps = TerminalCaps::baseline();
        let h = SyntaxHighlighter::new();
        let out = h.highlight("fn main() {}\n", Some("rust"), &caps)?;
        assert!(
            out.contains("\x1b[m") || out.contains("\x1b[0m"),
            "expected SGR reset: {out:?}",
        );
        Ok(())
    }

    #[test]
    fn direct_spans_cover_unescaped_code_and_keep_unknown_syntax_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let highlighter = SyntaxHighlighter::new();
        for (code, language) in [
            ("fn main() { let value = \"é🙂\"; }\n", Some("rust")),
            ("literal\ttext\u{1b}\n", Some("unknown-language")),
            ("#!/bin/bash\necho hi\n", None),
            ("", None),
        ] {
            let spans = highlighter.highlight_spans(code, language)?;
            let mut end = 0;
            for span in &spans {
                assert_eq!(span.range.start, end);
                assert!(span.range.start < span.range.end);
                assert!(code.get(span.range.clone()).is_some());
                assert!(span.style.foreground.is_some());
                end = span.range.end;
            }
            assert_eq!(end, code.len());
        }
        Ok(())
    }
}
