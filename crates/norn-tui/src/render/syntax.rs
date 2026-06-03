//! Syntect-backed syntax highlighting for fenced code blocks.
//!
//! [`SyntaxHighlighter`] owns syntect's default [`SyntaxSet`] (the
//! compressed binary dump shipping ~100 languages) and [`ThemeSet`]. It
//! resolves the grammar by language hint with a fallback to
//! `find_syntax_by_first_line` for unlabeled blocks, then routes each
//! coloured chunk through [`colour_for`] so output adapts to terminal
//! capabilities (24-bit RGB on true-colour terminals; 256-colour palette
//! on baseline terminals).

use std::fmt::Write as _;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use termina::escape::csi::{Csi, Sgr};
use termina::style::{Intensity, RgbColor, Underline};

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

impl SyntaxHighlighter {
    /// Build a highlighter from syntect's bundled defaults.
    ///
    /// Loads the default [`SyntaxSet`] (newline-preserving variant —
    /// required by [`LinesWithEndings`]) and selects the bundled
    /// `base16-ocean.dark` theme, falling back to the first available
    /// theme if the default is missing. Returns a default-constructed
    /// [`Theme`] in the degenerate case where the bundled [`ThemeSet`] is
    /// empty — keeps the API total and avoids panicking in library code.
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
    pub fn highlight(&self, code: &str, language: Option<&str>, caps: &TerminalCaps) -> String {
        let syntax = language
            .and_then(|hint| self.syntaxes.find_syntax_by_token(hint))
            .or_else(|| self.syntaxes.find_syntax_by_first_line(code))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());

        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut output = String::with_capacity(code.len());

        for line in LinesWithEndings::from(code) {
            let ranges = highlighter
                .highlight_line(line, &self.syntaxes)
                .unwrap_or_default();
            for (style, text) in ranges {
                let colour =
                    RgbColor::new(style.foreground.r, style.foreground.g, style.foreground.b);
                output.push_str(&colour_for(colour, caps));
                if style.font_style.contains(FontStyle::BOLD) {
                    let _ = write!(output, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Bold)));
                }
                if style.font_style.contains(FontStyle::ITALIC) {
                    let _ = write!(output, "{}", Csi::Sgr(super::style::italic(caps)));
                }
                if style.font_style.contains(FontStyle::UNDERLINE) {
                    let _ = write!(output, "{}", Csi::Sgr(Sgr::Underline(Underline::Single)));
                }
                output.push_str(text);
                let _ = write!(output, "{}", Csi::Sgr(Sgr::Reset));
            }
        }
        output
    }
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rust_keyword_gets_distinct_foreground_with_true_colour() {
        let caps = {
            let mut c = TerminalCaps::baseline();
            c.true_colour = true;
            c
        };
        let h = SyntaxHighlighter::new();
        let out = h.highlight("fn main() { let x = 1; }\n", Some("rust"), &caps);
        assert!(out.contains("38;2;"), "expected truecolor escape: {out:?}");
    }

    #[test]
    fn baseline_falls_back_to_256_colour_palette() {
        let caps = TerminalCaps::baseline();
        let h = SyntaxHighlighter::new();
        let out = h.highlight("fn main() {}\n", Some("rust"), &caps);
        assert!(out.contains("38;5;"), "expected palette escape: {out:?}");
    }

    #[test]
    fn unknown_language_falls_back_via_first_line() {
        let caps = TerminalCaps::baseline();
        let h = SyntaxHighlighter::new();
        // Shebang triggers first-line detection.
        let out = h.highlight("#!/bin/bash\necho hi\n", None, &caps);
        assert!(!out.is_empty());
    }

    #[test]
    fn plain_text_when_nothing_matches() {
        let caps = TerminalCaps::baseline();
        let h = SyntaxHighlighter::new();
        let out = h.highlight("just some text\n", Some("zzznotalanguage"), &caps);
        assert!(out.contains("just some text"));
    }

    #[test]
    fn each_chunk_terminates_with_reset() {
        let caps = TerminalCaps::baseline();
        let h = SyntaxHighlighter::new();
        let out = h.highlight("fn main() {}\n", Some("rust"), &caps);
        assert!(
            out.contains("\x1b[m") || out.contains("\x1b[0m"),
            "expected SGR reset: {out:?}",
        );
    }
}
