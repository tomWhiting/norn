//! Content-block rendering pipeline for tool output.
//!
//! Tool result renderers produce [`ContentBlock`] sequences that describe
//! the *semantic* structure of their output — code with a source path,
//! diffs, diagnostics, or plain text. [`render_blocks`] turns those
//! blocks into ANSI-styled terminal output using the shared
//! [`SyntaxHighlighter`] for code colouring and [`TerminalCaps`] for
//! capability-aware style selection.
//!
//! This separation keeps per-tool renderers thin (field extraction + block
//! tagging) while centralising all visual decisions — colours, line
//! numbers, diff markers — in one place.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::path::Path;

use super::style::colour_for;
use super::syntax::SyntaxHighlighter;
use crate::terminal::caps::TerminalCaps;

/// A semantic block of tool output.
pub enum ContentBlock<'a> {
    /// Source code with a file path for language detection.
    Code {
        /// File path used to select the syntax grammar.
        path: &'a str,
        /// Raw content, optionally with `cat -n` line-number prefixes.
        content: &'a str,
        /// Whether the content carries `N\t` line-number prefixes that
        /// should be styled separately from the code body.
        line_numbered: bool,
    },
    /// A diff: removed lines and added lines from the same file.
    Diff {
        /// File path for language detection on the changed content.
        path: &'a str,
        /// Removed text (the old version).
        removed: &'a str,
        /// Added text (the new version).
        added: &'a str,
    },
    /// A diagnostic message (error, warning, info).
    Diagnostic {
        /// Severity level — `"error"`, `"warning"`, or `"info"`.
        severity: &'a str,
        /// Human-readable message.
        message: &'a str,
    },
    /// Unstyled text passed through verbatim.
    Plain {
        /// Raw text content.
        text: Cow<'a, str>,
    },
}

/// Render a sequence of content blocks into ANSI-styled terminal output.
///
/// Code blocks are syntax-highlighted via `highlighter`; diffs get
/// red/green colouring with syntax highlighting on each side;
/// diagnostics are severity-coloured; plain text passes through.
pub fn render_blocks(
    blocks: &[ContentBlock<'_>],
    highlighter: &SyntaxHighlighter,
    caps: &TerminalCaps,
) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            ContentBlock::Code {
                path,
                content,
                line_numbered,
            } => render_code(&mut out, path, content, *line_numbered, highlighter, caps),
            ContentBlock::Diff {
                path,
                removed,
                added,
            } => render_diff(&mut out, path, removed, added, highlighter, caps),
            ContentBlock::Diagnostic { severity, message } => {
                render_diagnostic(&mut out, severity, message, caps);
            }
            ContentBlock::Plain { text } => out.push_str(text),
        }
    }
    out
}

/// Muted line-number colour (grey).
const LINE_NUM_RGB: termina::style::RgbColor = termina::style::RgbColor::new(100, 100, 100);

/// Render a code block with syntax highlighting.
///
/// When `line_numbered` is true, each line is expected to have a `N\t`
/// prefix (the `cat -n` format produced by the read tool). The prefix
/// is rendered in muted grey while the code body is syntax-highlighted.
fn render_code(
    out: &mut String,
    path: &str,
    content: &str,
    line_numbered: bool,
    highlighter: &SyntaxHighlighter,
    caps: &TerminalCaps,
) {
    let lang = lang_from_path(path);
    if !line_numbered {
        // Strip the trailing newline before highlighting — syntect
        // preserves the input newline as part of its last SGR range
        // (text + `\n` + reset), so handing it a trailing `\n` makes
        // the output end with `\n{reset}`. body.lines() in
        // write_tool_result would then split on that embedded `\n`
        // and emit an extra blank `│` line.
        out.push_str(&highlighter.highlight(content.trim_end_matches('\n'), lang, caps));
        out.push('\n');
        return;
    }
    let num_style = colour_for(LINE_NUM_RGB, caps);
    for line in content.lines() {
        if let Some((prefix, code)) = line.split_once('\t') {
            let _ = write!(out, "{num_style}{prefix}\t\x1b[0m");
            // Pass `code` to the highlighter without appending `\n`
            // for the same reason as the non-line-numbered branch.
            out.push_str(&highlighter.highlight(code, lang, caps));
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
}

/// Render a diff with red/green colouring and syntax highlighting.
fn render_diff(
    out: &mut String,
    path: &str,
    removed: &str,
    added: &str,
    highlighter: &SyntaxHighlighter,
    caps: &TerminalCaps,
) {
    let lang = lang_from_path(path);
    let red_fg = colour_for(termina::style::RgbColor::new(220, 50, 47), caps);
    let green_fg = colour_for(termina::style::RgbColor::new(38, 166, 91), caps);

    if !removed.is_empty() {
        for line in removed.lines() {
            // Highlight WITHOUT appending `\n` — syntect's output
            // preserves the input newline followed by an SGR reset,
            // so a trailing `\n` would leave an embedded `\n{reset}`
            // in the highlighted text. body.lines() in
            // write_tool_result would then split on the embedded `\n`
            // and emit an extra blank `│` line between diff lines.
            let highlighted_line = highlighter.highlight(line, lang, caps);
            let _ = write!(out, "\x1b[2m{red_fg}- \x1b[0m{highlighted_line}\x1b[0m");
            out.push('\n');
        }
    }
    if !added.is_empty() {
        for line in added.lines() {
            let highlighted_line = highlighter.highlight(line, lang, caps);
            let _ = write!(out, "\x1b[2m{green_fg}+ \x1b[0m{highlighted_line}\x1b[0m");
            out.push('\n');
        }
    }
}

/// Render a diagnostic with severity colouring.
fn render_diagnostic(out: &mut String, severity: &str, message: &str, caps: &TerminalCaps) {
    let colour = match severity {
        "error" => colour_for(termina::style::RgbColor::new(220, 50, 47), caps),
        "warning" => colour_for(termina::style::RgbColor::new(203, 175, 22), caps),
        _ => colour_for(termina::style::RgbColor::new(131, 148, 150), caps),
    };
    let _ = writeln!(out, "{colour}{severity}\x1b[0m: {message}");
}

/// Extract a language hint from a file path's extension.
///
/// Returns `Some("rust")` for `.rs`, `Some("python")` for `.py`, etc.
/// Syntect's `find_syntax_by_token` recognises these short names.
fn lang_from_path(path: &str) -> Option<&str> {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| match ext {
            "rs" | "rhai" => Some("rust"),
            "py" => Some("python"),
            "js" => Some("javascript"),
            "ts" => Some("typescript"),
            "tsx" => Some("tsx"),
            "jsx" => Some("jsx"),
            "rb" => Some("ruby"),
            "go" => Some("go"),
            "java" => Some("java"),
            "c" | "h" => Some("c"),
            "cpp" | "cc" | "cxx" | "hpp" => Some("c++"),
            "cs" => Some("c#"),
            "swift" => Some("swift"),
            "kt" | "kts" => Some("kotlin"),
            "lua" => Some("lua"),
            "sh" | "bash" | "zsh" => Some("bash"),
            "sql" => Some("sql"),
            "md" | "markdown" => None,
            "json" => Some("json"),
            "yaml" | "yml" => Some("yaml"),
            "toml" => Some("toml"),
            "html" | "htm" => Some("html"),
            "css" => Some("css"),
            "xml" => Some("xml"),
            "zig" => Some("zig"),
            "el" | "lisp" | "cl" => Some("lisp"),
            other => Some(other),
        })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args
)]
mod tests {
    use super::*;

    fn hl() -> SyntaxHighlighter {
        SyntaxHighlighter::new()
    }

    fn caps() -> TerminalCaps {
        TerminalCaps::baseline()
    }

    #[test]
    fn code_block_produces_highlighted_output() {
        let blocks = vec![ContentBlock::Code {
            path: "main.rs",
            content: "fn main() {}\n",
            line_numbered: false,
        }];
        let rendered = render_blocks(&blocks, &hl(), &caps());
        assert!(!rendered.is_empty());
        assert!(rendered.contains("fn"));
        assert!(rendered.contains('\x1b'));
    }

    #[test]
    fn line_numbered_code_splits_prefix_from_body() {
        let blocks = vec![ContentBlock::Code {
            path: "lib.rs",
            content: "1\tuse std::io;\n2\t\n3\tfn foo() {}\n",
            line_numbered: true,
        }];
        let rendered = render_blocks(&blocks, &hl(), &caps());
        assert!(rendered.contains("1\t"));
        assert!(rendered.contains("fn"));
        assert!(rendered.contains('\x1b'));
    }

    #[test]
    fn diff_block_has_plus_minus_markers() {
        let blocks = vec![ContentBlock::Diff {
            path: "main.rs",
            removed: "let x = 1;",
            added: "let x = 2;",
        }];
        let rendered = render_blocks(&blocks, &hl(), &caps());
        assert!(rendered.contains('-'));
        assert!(rendered.contains('+'));
    }

    #[test]
    fn diff_block_emits_one_newline_per_line_with_no_blanks() {
        // Regression: syntect preserved the input newline inside its
        // SGR-bracketed range, so the highlighted output ended in
        // `\n{reset}`. The render previously appended a second `\n`,
        // leaving an embedded blank line that `body.lines()` in
        // write_tool_result split into ghost `│` rows between every
        // real diff line.
        let blocks = vec![ContentBlock::Diff {
            path: "main.rs",
            removed: "let a = 1;\nlet b = 2;",
            added: "let a = 9;\nlet b = 8;",
        }];
        let rendered = render_blocks(&blocks, &hl(), &caps());
        assert!(
            !rendered.contains("\n\n"),
            "diff body must have no embedded blank lines: {rendered:?}",
        );
        // Two removed + two added → four lines, four newlines exactly.
        let newline_count = rendered.bytes().filter(|&b| b == b'\n').count();
        assert_eq!(newline_count, 4, "got: {rendered:?}");
    }

    #[test]
    fn line_numbered_code_emits_one_newline_per_line_with_no_blanks() {
        // Same regression as diff_block_emits_one_newline_per_line —
        // the line-numbered code path used to append a second newline
        // after each highlighted code line, producing blank `│` rows
        // between every content line in the read tool's output.
        let blocks = vec![ContentBlock::Code {
            path: "lib.rs",
            content: "1\tuse std::io;\n2\tuse std::fs;\n3\tfn main() {}\n",
            line_numbered: true,
        }];
        let rendered = render_blocks(&blocks, &hl(), &caps());
        assert!(
            !rendered.contains("\n\n"),
            "line-numbered body must have no embedded blank lines: {rendered:?}",
        );
        // Three content lines → three newlines exactly.
        let newline_count = rendered.bytes().filter(|&b| b == b'\n').count();
        assert_eq!(newline_count, 3, "got: {rendered:?}");
    }

    #[test]
    fn diagnostic_block_contains_severity() {
        let blocks = vec![ContentBlock::Diagnostic {
            severity: "error",
            message: "type mismatch",
        }];
        let rendered = render_blocks(&blocks, &hl(), &caps());
        assert!(rendered.contains("error"));
        assert!(rendered.contains("type mismatch"));
    }

    #[test]
    fn plain_block_passes_through() {
        let blocks = vec![ContentBlock::Plain {
            text: Cow::Borrowed("hello world"),
        }];
        let rendered = render_blocks(&blocks, &hl(), &caps());
        assert_eq!(rendered, "hello world");
    }

    #[test]
    fn lang_from_path_maps_common_extensions() {
        assert_eq!(lang_from_path("main.rs"), Some("rust"));
        assert_eq!(lang_from_path("app.py"), Some("python"));
        assert_eq!(lang_from_path("index.tsx"), Some("tsx"));
        assert_eq!(lang_from_path("Makefile"), None);
    }
}
