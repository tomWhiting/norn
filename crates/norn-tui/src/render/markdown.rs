//! Streaming markdown renderer with dim-stream + line-pop.
//!
//! Consumes assistant `TextDelta` chunks via [`MarkdownRenderer::feed`]
//! and returns [`FeedOutput`] — a pair of **dim** preview text and
//! **styled** final text. The caller writes dim text immediately for
//! streaming feel, then replaces it with styled text when a complete
//! line (`\n`) arrives (the "line-pop" effect).
//!
//! The dim/styled split follows a hybrid rule:
//! - **Inline content** (prose, bold, italic, inline code): each token
//!   is dim-previewed immediately; the line pops styled on `\n`.
//! - **Code fences**: dim preview is suppressed while inside an
//!   unclosed fence. The entire fence is written styled when it closes.
//!
//! [`MarkdownRenderer::finalize`] drains the buffer at end-of-stream:
//! any dim text on the current line is replaced with the styled
//! version; an unclosed code fence flushes as plain text.

mod dim;
mod emitter;
mod scan;

use pulldown_cmark::{Options, Parser};
use termina::escape::csi::Sgr;
use termina::style::{RgbColor, Underline};

use crate::terminal::caps::TerminalCaps;

use dim::render_dim_preview;
use emitter::Emitter;
use scan::{ScanResult, is_plain_text, scan_pending};

use super::syntax::SyntaxHighlighter;

/// Distinct foreground colour for inline code spans. Chosen so it does
/// not collide with the generating-indicator colour in `fixed_panel`.
///
/// Shared by [`dim`] and [`emitter`] — a private item in this parent
/// module is visible to both descendants via `super::`, so no explicit
/// visibility modifier is needed.
const INLINE_CODE_COLOUR: RgbColor = RgbColor::new(0, 175, 175);

/// Styled prefix written before each line of blockquote content.
///
/// Wraps the U+2502 vertical-bar glyph in dim-on / dim-off SGR so the
/// prefix sits in dim intensity while the quoted content stays at
/// normal intensity. One unit of prefix is emitted per nesting level —
/// the emitter writes `BLOCKQUOTE_PREFIX` `depth` times when opening a
/// quote, restoring the prefix after line breaks, and re-emitting it
/// inside paragraph breaks so multi-paragraph quotes stay correctly
/// aligned.
const BLOCKQUOTE_PREFIX: &str = "\x1b[2m\u{2502} \x1b[22m";

/// `pulldown-cmark` extensions activated for the streaming renderer.
///
/// - `ENABLE_TABLES`: GFM pipe tables. The emitter's table arms
///   (Phase 3 R5) consume the resulting events.
/// - `ENABLE_STRIKETHROUGH`: `~~text~~` parses as
///   [`pulldown_cmark::Tag::Strikethrough`] rather than literal tildes.
/// - `ENABLE_TASKLISTS`: `- [x]` / `- [ ]` emit
///   [`pulldown_cmark::Event::TaskListMarker`].
/// - `ENABLE_MATH`: `$inline$` and `$$display$$` emit
///   [`pulldown_cmark::Event::InlineMath`] / `DisplayMath` rather than
///   plain text with literal `$` delimiters.
/// - `ENABLE_SMART_PUNCTUATION`: straight quotes and dashes are
///   rewritten to curly typographic forms; mirrors what every other
///   markdown viewer does.
const MARKDOWN_OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_MATH)
    .union(Options::ENABLE_SMART_PUNCTUATION);

/// SGR escape that closes an italic span — honours italic-vs-underline
/// fallback so it pairs symmetrically with
/// [`crate::render::style::italic`]. Shared by [`dim`] and [`emitter`].
fn italic_off(caps: &TerminalCaps) -> Sgr {
    if caps.italic_support {
        Sgr::Italic(false)
    } else {
        Sgr::Underline(Underline::None)
    }
}

/// Output from [`MarkdownRenderer::feed`] — separates dim preview
/// from styled final text so the caller can implement line-pop.
///
/// The dispatch layer writes `dim` wrapped in dim SGR for streaming
/// feel, then when `styled` arrives it emits CR + EL to clear the
/// dim text before writing the fully rendered line.
#[derive(Clone, Debug, Default)]
pub struct FeedOutput {
    /// Raw text for immediate dim preview. Empty when inside a code
    /// fence (dim suppressed) or when no new content to preview.
    pub dim: String,
    /// Fully styled text rendered through pulldown-cmark. Non-empty
    /// when a complete line (`\n`) or balanced inline span triggers a
    /// render pass.
    pub styled: String,
    /// Whether dim text was written to the current scroll-region line
    /// before this output was produced. When `true` and `styled` is
    /// non-empty, the caller should emit `\r` + erase-in-line before
    /// writing `styled` to replace the dim preview.
    pub replace_dim: bool,
}

impl FeedOutput {
    fn empty() -> Self {
        Self::default()
    }
}

/// Streaming markdown renderer with dim-stream + line-pop support.
///
/// Line-buffered — accumulates input across [`feed`](Self::feed)
/// calls and returns [`FeedOutput`] with both dim preview and styled
/// text. Inline content is dim-previewed immediately; code fences
/// buffer silently until their closer. Complete lines (`\n`) trigger
/// a render pass through pulldown-cmark, producing styled text that
/// replaces the dim preview.
pub struct MarkdownRenderer {
    caps: TerminalCaps,
    terminal_width: u16,
    highlighter: SyntaxHighlighter,
    pending: String,
    paragraph_pending: bool,
    /// Whether dim preview text has been written to the current
    /// scroll-region line. Set when `feed` returns non-empty `dim`;
    /// cleared when `feed` returns non-empty `styled` (the caller
    /// replaces the dim line).
    dim_active: bool,
}

impl MarkdownRenderer {
    /// Construct a renderer for a terminal with the given capabilities
    /// and column width. A fresh [`SyntaxHighlighter`] is built
    /// internally — callers do not need to provide one.
    pub fn new(caps: TerminalCaps, terminal_width: u16) -> Self {
        Self {
            caps,
            terminal_width,
            highlighter: SyntaxHighlighter::new(),
            pending: String::new(),
            paragraph_pending: false,
            dim_active: false,
        }
    }

    /// Whether dim preview text is currently live on the scroll-region
    /// line. The tick handler uses this to erase dim text before a panel
    /// redraw that might clamp the cursor (which would commit the dim
    /// text into permanent scrollback via `\r\n`).
    #[must_use]
    pub fn is_dim_active(&self) -> bool {
        self.dim_active
    }

    /// Reset the dim-active flag and return its prior value.
    ///
    /// Used by the dispatch layer's [`clear_dim_state`] helper before a
    /// non-markdown-stream write (tool result, error line) to break the
    /// dim cycle that would otherwise leave a ghost behind and let the
    /// next tick destroy real content during its erase-then-repaint
    /// pass. The returned bool follows the same gated-erase contract as
    /// [`FeedOutput::replace_dim`]: `true` means dim text was live on
    /// the current line and the caller must erase
    /// `state.dim_wrapped_lines` rows before its next write; `false`
    /// means no erase is needed and `dim_wrapped_lines` is already 0.
    ///
    /// The renderer's pending markdown buffer is intentionally left
    /// untouched — only the live-on-screen flag is cleared. The next
    /// `feed` or `finalize` can still drain the pending buffer
    /// normally. This separates terminal-state reset from
    /// parser-state reset.
    ///
    /// [`clear_dim_state`]: crate::app::helpers::clear_dim_state
    pub fn clear_dim(&mut self) -> bool {
        let was_active = self.dim_active;
        self.dim_active = false;
        was_active
    }

    /// Re-render the current dim preview from the pending buffer.
    ///
    /// Returns the same dim string that the last `feed` would have
    /// produced, without changing internal state. The tick handler calls
    /// this to repaint dim text after a panel redraw that may have
    /// erased it.
    #[must_use]
    pub fn current_dim_preview(&self) -> String {
        if self.pending.is_empty() || scan_pending(&self.pending).fence_unclosed {
            return String::new();
        }
        render_dim_preview(&self.pending, &self.caps)
    }

    /// Accept a streamed text chunk and return dim + styled output.
    ///
    /// The dim preview is built from the **entire current pending
    /// buffer**, not just the new chunk — closed `**bold**` /
    /// `*italic*` / `` `code` `` spans and ATX heading markers need
    /// full-line context to be detected, so the caller always replaces
    /// the previous dim line with the freshly rendered view.
    /// `replace_dim` is set whenever a previous dim was active so the
    /// dispatch layer can issue `\r\x1b[2K` before repainting; on the
    /// first dim chunk of a line it stays `false` and the dim text is
    /// appended at the cursor.
    ///
    /// When a complete line (`\n`) flushes through pulldown-cmark,
    /// `styled` is non-empty and the dispatch layer writes it after
    /// the `\r\x1b[2K` clear so the dim preview is "popped" to its
    /// styled form.
    ///
    /// Inside an unclosed code fence, dim preview is suppressed — the
    /// entire fence is rendered styled when the closing marker arrives.
    pub fn feed(&mut self, chunk: &str) -> FeedOutput {
        self.pending.push_str(chunk);
        self.repair_embedded_fence_close();
        let styled = self.try_flush_styled();
        let had_dim = self.dim_active;

        let in_fence = scan_pending(&self.pending).fence_unclosed;
        let dim = if in_fence || self.pending.is_empty() {
            String::new()
        } else {
            render_dim_preview(&self.pending, &self.caps)
        };

        self.dim_active = !dim.is_empty();

        FeedOutput {
            replace_dim: had_dim,
            styled,
            dim,
        }
    }

    /// Flush whatever remains in the buffer at end-of-stream.
    ///
    /// An unclosed code fence is emitted verbatim per R6 acceptance —
    /// highlighting partial code without a closing fence would be
    /// misleading. Otherwise the buffer is parsed normally;
    /// pulldown-cmark treats unmatched inline markers as literal text.
    /// `replace_dim` is set when dim preview was active so the caller
    /// clears it before writing the final styled output.
    pub fn finalize(&mut self) -> FeedOutput {
        if self.pending.is_empty() {
            return FeedOutput::empty();
        }
        let scan = scan_pending(&self.pending);
        let segment = std::mem::take(&mut self.pending);
        let styled = if scan.fence_unclosed {
            segment
        } else {
            self.render_segment(&segment)
        };
        let replace = self.dim_active;
        self.dim_active = false;
        FeedOutput {
            dim: String::new(),
            styled,
            replace_dim: replace,
        }
    }

    /// Insert a synthetic `\n` before any triple-backtick or
    /// triple-tilde sequence that appears mid-line while a fence of the
    /// same kind is open.
    ///
    /// LLMs frequently emit the last line of a code block and the
    /// closing fence in a single token — for example `}\u{60}\u{60}\u{60}`. Without
    /// this repair, the closing marker is treated as content (it is
    /// not at line start), the fence never closes through `pulldown-cmark`,
    /// and every subsequent paragraph renders inside the code block until
    /// the model emits another bare-line fence marker. Splitting `}\u{60}\u{60}\u{60}\n`
    /// into `}\n\u{60}\u{60}\u{60}\n` restores `CommonMark` fence semantics so the
    /// close fires when expected.
    ///
    /// The state machine tracks the open fence kind (backtick or
    /// tilde) and `at_line_start` independently of [`scan_pending`] —
    /// the repair must work for tilde-opened fences even before R2's
    /// tilde awareness lands in `scan_pending`. Insertions are
    /// collected and applied in reverse order so earlier byte offsets
    /// remain valid as the buffer grows.
    fn repair_embedded_fence_close(&mut self) {
        let bytes = self.pending.as_bytes();
        let n = bytes.len();
        let mut insertions: Vec<usize> = Vec::new();
        let mut open_marker: Option<u8> = None;
        let mut at_line_start = true;
        let mut i = 0;
        while i < n {
            if i + 2 < n
                && (bytes[i] == b'`' || bytes[i] == b'~')
                && bytes[i] == bytes[i + 1]
                && bytes[i] == bytes[i + 2]
            {
                let kind = bytes[i];
                match open_marker {
                    Some(open) if open == kind => {
                        if !at_line_start {
                            insertions.push(i);
                        }
                        open_marker = None;
                        i += 3;
                        at_line_start = false;
                        continue;
                    }
                    Some(_) => {}
                    None => {
                        if at_line_start {
                            open_marker = Some(kind);
                            i += 3;
                            at_line_start = false;
                            continue;
                        }
                    }
                }
            }
            at_line_start = bytes[i] == b'\n';
            i += 1;
        }
        for pos in insertions.into_iter().rev() {
            self.pending.insert(pos, '\n');
        }
    }

    /// Flush styled output at line boundaries only.
    ///
    /// Styled output is produced only when a `\n` is present in the
    /// pending buffer — this is the "line-pop" trigger. Content
    /// without `\n` stays pending and is dim-previewed by the caller.
    fn try_flush_styled(&mut self) -> String {
        let Some(nl_pos) = self.pending.rfind('\n') else {
            return String::new();
        };
        let line_end = nl_pos + 1;
        let ScanResult { safe_end, .. } = scan_pending(&self.pending[..line_end]);
        if safe_end > 0 {
            let to_render = self.pending[..safe_end].to_string();
            self.pending.drain(..safe_end);
            return self.render_segment(&to_render);
        }
        String::new()
    }

    fn render_segment(&mut self, source: &str) -> String {
        if source.is_empty() {
            return String::new();
        }
        if source.chars().all(char::is_whitespace) {
            self.paragraph_pending = false;
            return source.to_string();
        }
        if is_plain_text(source) {
            if self.paragraph_pending {
                self.paragraph_pending = false;
                return format!("\n\n{source}");
            }
            return source.to_string();
        }
        let collapsed = collapse_blank_lines(source);
        let parse_source: &str = match collapsed.as_ref() {
            Some(owned) => owned,
            None => source,
        };
        let mut output = String::new();
        if self.paragraph_pending {
            output.push_str("\n\n");
            self.paragraph_pending = false;
        }
        let parser = Parser::new_ext(parse_source, MARKDOWN_OPTIONS);
        let mut emitter = Emitter::new(&self.caps, self.terminal_width, &self.highlighter);
        for event in parser {
            emitter.handle(event);
        }
        let (segment, pending) = emitter.into_output();
        output.push_str(&segment);
        self.paragraph_pending = pending;
        output
    }
}

/// Collapse runs of three or more consecutive `\n` into exactly `\n\n`.
///
/// LLM output occasionally pads sections with extra blank lines; `pulldown-cmark`
/// treats more than one blank line as a single paragraph break anyway,
/// but keeping the extras in the source means downstream consumers see
/// jarring vertical gaps in the dim preview. Pre-collapsing matches the
/// rendered output to the parser's interpretation. Returns `None` when
/// the source has no such run so the caller can avoid an allocation.
fn collapse_blank_lines(source: &str) -> Option<String> {
    let mut max_run = 0usize;
    let mut current = 0usize;
    for ch in source.chars() {
        if ch == '\n' {
            current += 1;
            if current > max_run {
                max_run = current;
            }
        } else {
            current = 0;
        }
    }
    if max_run < 3 {
        return None;
    }
    let mut out = String::with_capacity(source.len());
    let mut run = 0usize;
    for ch in source.chars() {
        if ch == '\n' {
            run += 1;
            if run <= 2 {
                out.push('\n');
            }
        } else {
            run = 0;
            out.push(ch);
        }
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::render::style::colour_for;

    fn caps_with(mutate: impl FnOnce(&mut TerminalCaps)) -> TerminalCaps {
        let mut caps = TerminalCaps::baseline();
        mutate(&mut caps);
        caps
    }

    fn collect_styled(r: &mut MarkdownRenderer, input: &str) -> String {
        let out = r.feed(input);
        let tail = r.finalize();
        format!("{}{}", out.styled, tail.styled)
    }

    #[test]
    fn bold_emits_bold_sgr() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let full = collect_styled(&mut r, "**bold**");
        assert!(full.contains("\x1b[1m"), "expected bold SGR: {full:?}");
        assert!(full.contains("bold"));
    }

    #[test]
    fn italic_emits_italic_sgr_when_supported() {
        let caps = caps_with(|c| c.italic_support = true);
        let mut r = MarkdownRenderer::new(caps, 80);
        let out = collect_styled(&mut r, "*emph*");
        assert!(out.contains("\x1b[3m"), "expected italic SGR: {out:?}");
    }

    #[test]
    fn italic_falls_back_to_underline() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "*emph*");
        assert!(out.contains("\x1b[4m"), "expected underline SGR: {out:?}");
    }

    #[test]
    fn inline_code_uses_distinct_foreground() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "call `func` here");
        let expected = colour_for(INLINE_CODE_COLOUR, &TerminalCaps::baseline());
        assert!(
            out.contains(&expected),
            "expected inline-code colour: {out:?}"
        );
        assert!(out.contains("func"));
    }

    #[test]
    fn nested_bold_italic_emits_both_attributes() {
        let caps = caps_with(|c| c.italic_support = true);
        let mut r = MarkdownRenderer::new(caps, 80);
        let out = collect_styled(&mut r, "***both***");
        assert!(out.contains("\x1b[1m"), "expected bold SGR: {out:?}");
        assert!(out.contains("\x1b[3m"), "expected italic SGR: {out:?}");
    }

    #[test]
    fn heading_emits_bold_and_text() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "# Title\n");
        assert!(out.contains("\x1b[1m"), "expected bold SGR: {out:?}");
        assert!(out.contains("Title"));
    }

    #[test]
    fn unordered_list_uses_bullet_and_indent() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "- outer\n  - nested\n");
        // R9 cycles bullet glyphs by depth: outer = •, nested = ◦.
        assert!(out.contains("\u{2022} outer"), "got: {out:?}");
        assert!(out.contains("  \u{25E6} nested"), "got: {out:?}");
    }

    #[test]
    fn ordered_list_numbers_sequentially() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "1. first\n2. second\n3. third\n");
        assert!(out.contains("1. first"), "got: {out:?}");
        assert!(out.contains("2. second"), "got: {out:?}");
        assert!(out.contains("3. third"), "got: {out:?}");
    }

    #[test]
    fn horizontal_rule_spans_terminal_width() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 12);
        let out = collect_styled(&mut r, "---\n");
        assert!(out.contains(&"─".repeat(12)), "got: {out:?}");
    }

    #[test]
    fn link_falls_back_to_bracketed_text() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "see [docs](https://x/y) please");
        assert!(out.contains("docs (https://x/y)"), "got: {out:?}");
    }

    #[test]
    fn link_emits_osc8_when_supported() {
        let caps = caps_with(|c| c.osc_hyperlinks = true);
        let mut r = MarkdownRenderer::new(caps, 80);
        let out = collect_styled(&mut r, "[docs](https://x)");
        assert!(out.contains("\x1b]8;;https://x"), "got: {out:?}");
        assert!(out.contains("docs"));
    }

    #[test]
    fn fenced_code_block_emits_foreground_escape() {
        let caps = caps_with(|c| c.true_colour = true);
        let mut r = MarkdownRenderer::new(caps, 80);
        let out = collect_styled(&mut r, "```rust\nfn main() {}\n```\n");
        assert!(out.contains("38;2;"), "expected truecolor escape: {out:?}");
    }

    #[test]
    fn fenced_code_block_baseline_uses_palette() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "```rust\nfn main() {}\n```\n");
        assert!(out.contains("38;5;"), "expected palette escape: {out:?}");
    }

    #[test]
    fn unlabeled_code_block_falls_back_via_first_line() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "```\n#!/bin/bash\necho hi\n```\n");
        assert!(!out.is_empty());
        assert!(out.contains("echo") && out.contains("hi"));
    }

    #[test]
    fn code_fence_split_across_chunks_buffers_until_close() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let first = r.feed("```rust\nfn main() {\n");
        let second = r.feed("}\n```\n");
        let tail = r.finalize();
        let combined = format!("{}{}{}", first.styled, second.styled, tail.styled);
        assert!(
            !first.styled.contains("fn main"),
            "fence should buffer until close, got: {:?}",
            first.styled,
        );
        assert!(
            combined.contains("fn") && combined.contains("main"),
            "expected highlighted code: {combined:?}"
        );
        assert!(
            combined.contains("38;5;"),
            "expected palette escape: {combined:?}"
        );
    }

    #[test]
    fn unclosed_fence_on_finalize_renders_plain() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let first = r.feed("```rust\nfn main() {\n");
        let tail = r.finalize();
        let combined = format!("{}{}", first.styled, tail.styled);
        assert!(combined.contains("fn main"), "got: {combined:?}");
        assert!(
            !combined.contains("38;5;"),
            "expected no highlight: {combined:?}"
        );
    }

    #[test]
    fn split_bold_across_chunks() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let first = r.feed("**bold");
        assert!(
            first.styled.is_empty(),
            "buffered before close: {:?}",
            first.styled,
        );
        let second = r.feed(" text**");
        let tail = r.finalize();
        let combined = format!("{}{}{}", first.styled, second.styled, tail.styled);
        assert!(
            combined.contains("\x1b[1m"),
            "expected bold SGR: {combined:?}"
        );
        assert!(combined.contains("bold text"), "got: {combined:?}");
    }

    #[test]
    fn split_italic_across_chunks() {
        let caps = caps_with(|c| c.italic_support = true);
        let mut r = MarkdownRenderer::new(caps, 80);
        let first = r.feed("*emph");
        assert!(first.styled.is_empty(), "buffered: {:?}", first.styled);
        let second = r.feed(" word*");
        let tail = r.finalize();
        let combined = format!("{}{}{}", first.styled, second.styled, tail.styled);
        assert!(
            combined.contains("\x1b[3m"),
            "expected italic SGR: {combined:?}"
        );
        assert!(combined.contains("emph word"), "got: {combined:?}");
    }

    #[test]
    fn plain_text_before_open_marker_dims_immediately() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = r.feed("plain **bold");
        assert!(
            out.dim.contains("plain"),
            "leading plain text must dim-preview: {:?}",
            out.dim,
        );
        assert!(
            out.styled.is_empty(),
            "no styled without newline: {:?}",
            out.styled,
        );
        let second = r.feed(" text**\n");
        assert!(
            second.styled.contains("bold text"),
            "styled on newline: {:?}",
            second.styled,
        );
        assert!(second.replace_dim, "must replace dim");
    }

    #[test]
    fn unclosed_marker_buffers_styled_until_finalize() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("**");
        for _ in 0..50 {
            let out = r.feed("x");
            assert!(
                out.styled.is_empty(),
                "unclosed bold must not produce styled, got: {:?}",
                out.styled,
            );
            assert!(
                !out.dim.is_empty(),
                "unclosed bold should dim-preview new chunks",
            );
        }
        let tail = r.finalize();
        assert!(
            tail.styled.contains("**") && tail.styled.contains('x'),
            "finalize must flush unclosed marker as literal text: {:?}",
            tail.styled,
        );
    }

    #[test]
    fn empty_feed_returns_empty() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = r.feed("");
        assert!(out.dim.is_empty() && out.styled.is_empty());
        let tail = r.finalize();
        assert!(tail.dim.is_empty() && tail.styled.is_empty());
    }

    #[test]
    fn finalize_with_no_pending_returns_empty() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("hello\n");
        let _ = r.finalize();
        let tail = r.finalize();
        assert!(tail.styled.is_empty());
    }

    #[test]
    fn single_paragraph_has_no_trailing_newline() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "Hello world.");
        assert!(
            !out.ends_with('\n'),
            "streaming output must not end with paragraph newline: {out:?}",
        );
        assert!(out.contains("Hello world."));
    }

    #[test]
    fn paragraph_break_carried_across_segments() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let first = r.feed("Intro text.\n\n");
        let second = r.feed("```rust\nfn main() {}\n```\n");
        let tail = r.finalize();
        let combined = format!("{}{}{}", first.styled, second.styled, tail.styled);
        assert!(
            combined.contains("Intro text.\n\n"),
            "paragraph break before code block must be preserved: {combined:?}",
        );
    }

    #[test]
    fn paragraph_break_before_plain_text_across_segments() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let first = r.feed("First paragraph.\n\n");
        let second = r.feed("Second paragraph.");
        let tail = r.finalize();
        let combined = format!("{}{}{}", first.styled, second.styled, tail.styled);
        assert!(
            combined.contains("\n\n"),
            "paragraph break between segments must be preserved: {combined:?}",
        );
        assert!(combined.contains("First paragraph."));
        assert!(combined.contains("Second paragraph."));
    }

    #[test]
    fn two_paragraphs_separated_by_double_newline() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "First.\n\nSecond.");
        assert!(
            out.contains("First.\n\nSecond."),
            "paragraphs must be separated by blank line: {out:?}",
        );
    }

    #[test]
    fn streaming_tokens_produce_no_spurious_newlines() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let mut combined = String::new();
        for token in ["Hello ", "world", "."] {
            let out = r.feed(token);
            combined.push_str(&out.styled);
        }
        combined.push_str(&r.finalize().styled);
        assert!(
            !combined.contains('\n'),
            "plain streaming tokens must not produce newlines: {combined:?}",
        );
    }

    #[test]
    fn dim_preview_returned_for_inline_tokens() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = r.feed("Hello ");
        assert_eq!(out.dim, "Hello ", "inline token should dim-preview");
        assert!(out.styled.is_empty(), "no styled yet — no newline");
        assert!(!out.replace_dim, "no prior dim to replace");
    }

    #[test]
    fn dim_replaced_on_newline_with_styled() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("Hello ");
        let out = r.feed("world.\n");
        assert!(out.replace_dim, "must signal dim replacement");
        assert!(!out.styled.is_empty(), "styled output on newline");
        assert!(out.styled.contains("Hello "), "styled has full line");
    }

    #[test]
    fn dim_suppressed_inside_code_fence() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("```rust\n");
        let mid = r.feed("fn main() {}\n");
        assert!(mid.dim.is_empty(), "no dim inside fence: {:?}", mid.dim);
    }

    #[test]
    fn finalize_replaces_dim_with_styled() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("partial");
        let tail = r.finalize();
        assert!(tail.replace_dim, "finalize must signal dim replacement");
        assert!(tail.styled.contains("partial"));
    }

    #[test]
    fn feed_returns_stripped_dim_for_heading_chunk() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = r.feed("# Title");
        assert!(
            !out.dim.contains('#'),
            "heading marker must not appear in dim preview: {:?}",
            out.dim,
        );
        assert!(out.dim.contains("Title"));
        assert!(out.styled.is_empty(), "no \\n yet → no styled");
    }

    #[test]
    fn feed_returns_full_pending_dim_on_each_chunk() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let first = r.feed("a **bo");
        assert!(first.dim.contains("**bo"), "unclosed: {:?}", first.dim);

        let second = r.feed("ld**");
        assert!(
            !second.dim.contains("**"),
            "closed bold markers must be stripped from repainted dim: {:?}",
            second.dim,
        );
        assert!(
            second.dim.contains("bold"),
            "bold content must remain: {:?}",
            second.dim,
        );
        assert!(second.replace_dim, "must signal repaint of prior dim");
    }

    #[test]
    fn clear_dim_returns_true_when_dim_was_active() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("Hello ");
        assert!(r.is_dim_active(), "dim should be active after a chunk");
        let was_active = r.clear_dim();
        assert!(was_active, "clear_dim must report the prior dim_active");
        assert!(!r.is_dim_active(), "dim_active must be false after clear");
    }

    #[test]
    fn clear_dim_returns_false_when_dim_was_inactive() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        assert!(!r.is_dim_active(), "dim starts inactive");
        let was_active = r.clear_dim();
        assert!(
            !was_active,
            "clear_dim must report false when nothing was live",
        );
        assert!(!r.is_dim_active());
    }

    #[test]
    fn strikethrough_emits_sgr_9_and_29() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "say ~~goodbye~~ now\n");
        assert!(out.contains("\x1b[9m"), "expected SGR 9: {out:?}");
        assert!(out.contains("\x1b[29m"), "expected SGR 29 close: {out:?}");
        assert!(out.contains("goodbye"), "content preserved: {out:?}");
        assert!(!out.contains("~~"), "strike markers stripped: {out:?}");
    }

    #[test]
    fn strikethrough_inside_bold_emits_both_sgr() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "**~~deleted bold~~**\n");
        assert!(out.contains("\x1b[1m"), "expected bold SGR: {out:?}");
        assert!(out.contains("\x1b[9m"), "expected strike SGR: {out:?}");
        assert!(out.contains("deleted bold"), "content preserved: {out:?}");
    }

    #[test]
    fn checked_task_list_item_uses_checked_box_glyph() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "- [x] Done\n");
        assert!(
            out.contains("\u{2611} Done"),
            "expected checked-box glyph followed by item text: {out:?}",
        );
        assert!(
            !out.contains("• [x]"),
            "raw bullet + bracket form must be replaced: {out:?}",
        );
    }

    #[test]
    fn unchecked_task_list_item_uses_unchecked_box_glyph() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "- [ ] Todo\n");
        assert!(
            out.contains("\u{2610} Todo"),
            "expected unchecked-box glyph followed by item text: {out:?}",
        );
    }

    #[test]
    fn mixed_task_and_regular_list_items_render_correctly() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "- [x] One\n- Two\n- [ ] Three\n");
        assert!(out.contains("\u{2611} One"), "checked first item: {out:?}");
        assert!(out.contains("• Two"), "regular item keeps bullet: {out:?}");
        assert!(
            out.contains("\u{2610} Three"),
            "unchecked last item: {out:?}",
        );
    }

    #[test]
    fn inline_math_renders_with_inline_code_colour() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "energy is $E = mc^2$ exactly\n");
        let expected = colour_for(INLINE_CODE_COLOUR, &TerminalCaps::baseline());
        assert!(
            out.contains(&expected),
            "expected inline-code colour around math: {out:?}",
        );
        assert!(
            out.contains("E = mc^2"),
            "math source preserved verbatim: {out:?}",
        );
        assert!(
            !out.contains("$E = mc^2$"),
            "dollar delimiters stripped: {out:?}",
        );
    }

    #[test]
    fn display_math_indents_and_renders_with_colour() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "intro\n\n$$x = y^2$$\n");
        let expected = colour_for(INLINE_CODE_COLOUR, &TerminalCaps::baseline());
        assert!(
            out.contains(&expected),
            "expected inline-code colour around display math: {out:?}",
        );
        assert!(out.contains("  "), "expected 2-space indent: {out:?}");
        assert!(out.contains("x = y^2"), "math source preserved: {out:?}");
    }

    #[test]
    fn inline_html_renders_dim() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "before <br> after\n");
        assert!(out.contains("\x1b[2m<br>\x1b[22m"), "got: {out:?}");
        assert!(out.contains("before"), "got: {out:?}");
        assert!(out.contains("after"), "got: {out:?}");
    }

    #[test]
    fn block_html_renders_dim() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "<div>content</div>\n");
        assert!(out.contains("\x1b[2m"), "expected dim SGR: {out:?}");
        assert!(out.contains("<div>"), "raw HTML preserved: {out:?}");
        assert!(out.contains("</div>"), "raw HTML preserved: {out:?}");
        assert!(out.contains("\x1b[22m"), "dim closed: {out:?}");
    }

    #[test]
    fn fence_repair_splits_embedded_close_backtick() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("```rust\nfn x() {}```\nafter\n");
        let _ = r.finalize();
        // After repair the fence closes properly so "after" is plain
        // text, not buffered as code — pending must drain to empty.
        assert!(
            !r.is_dim_active() && r.pending.is_empty(),
            "fence must close after repair",
        );
    }

    #[test]
    fn fence_repair_no_op_when_no_open_fence() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        // Triple backticks at line start — opens a fence, never closes.
        let _ = r.feed("```");
        // Pending should still be exactly "```", no synthetic \n added.
        assert_eq!(r.pending, "```");
    }

    #[test]
    fn fence_repair_handles_tilde_fence() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("~~~rust\nfn x() {}~~~\nafter\n");
        let _ = r.finalize();
        // After repair, the tilde fence closes and "after" arrives as
        // plain text — pending must drain.
        assert!(r.pending.is_empty(), "tilde fence must close after repair");
    }

    #[test]
    fn fence_repair_does_not_close_mismatched_kind() {
        // A backtick fence is NOT closed by tilde markers.
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("```\nfn x() {}~~~\n");
        // Tilde inside backtick fence is content — no insertion fires.
        // The buffer remains as fed (the only \n's are the originals).
        assert_eq!(
            r.pending.matches('\n').count(),
            2,
            "no synthetic newline inserted for mismatched marker: {:?}",
            r.pending,
        );
    }

    #[test]
    fn fence_repair_inserts_only_when_fence_is_open() {
        // Triple backticks outside any fence are not closing anything —
        // no insertion. Feeds without a trailing `\n` so that
        // `try_flush_styled` keeps the buffer intact and we observe the
        // repair's effect (or non-effect) directly on `pending`.
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("text ``` more");
        assert_eq!(r.pending, "text ``` more");
    }

    #[test]
    fn nested_unordered_lists_cycle_bullet_glyphs() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "- d0\n  - d1\n    - d2\n      - d3\n        - d4\n");
        assert!(out.contains("\u{2022} d0"), "depth 0 = •: {out:?}");
        assert!(out.contains("\u{25E6} d1"), "depth 1 = ◦: {out:?}");
        assert!(out.contains("\u{25AA} d2"), "depth 2 = ▪: {out:?}");
        assert!(out.contains("\u{2023} d3"), "depth 3 = ‣: {out:?}");
        assert!(out.contains("\u{2023} d4"), "depth 4+ = ‣: {out:?}");
    }

    #[test]
    fn ordered_lists_keep_numbers_at_every_depth() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "1. a\n   1. b\n      1. c\n");
        assert!(out.contains("1. a"), "got: {out:?}");
        assert!(out.contains("1. b"), "got: {out:?}");
        assert!(out.contains("1. c"), "got: {out:?}");
        assert!(
            !out.contains("\u{2022}") && !out.contains("\u{25E6}"),
            "ordered lists must not emit bullet glyphs: {out:?}",
        );
    }

    #[test]
    fn image_with_alt_renders_dim_placeholder() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "before ![a cat](https://x/y.png) after\n");
        assert!(
            out.contains("\x1b[2m[image: a cat]\x1b[22m"),
            "got: {out:?}"
        );
        assert!(out.contains("before"), "got: {out:?}");
        assert!(out.contains("after"), "got: {out:?}");
    }

    #[test]
    fn image_without_alt_renders_bare_placeholder() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "![](https://x/y.png)\n");
        assert!(out.contains("\x1b[2m[image]\x1b[22m"), "got: {out:?}");
        assert!(
            !out.contains("[image: ]"),
            "empty alt must collapse: {out:?}",
        );
    }

    #[test]
    fn three_consecutive_blank_lines_collapse_to_one_break() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "First.\n\n\n\nSecond.\n");
        assert!(out.contains("First."));
        assert!(out.contains("Second."));
        assert!(
            !out.contains("\n\n\n"),
            "runs of 3+ newlines must collapse: {out:?}",
        );
    }

    #[test]
    fn collapse_blank_lines_preserves_double_newline() {
        let collapsed = collapse_blank_lines("a\n\nb");
        assert!(collapsed.is_none(), "no collapse needed for run of 2");
    }

    #[test]
    fn collapse_blank_lines_handles_unicode() {
        let collapsed = collapse_blank_lines("α\n\n\n\nβ").unwrap();
        assert_eq!(collapsed, "α\n\nβ");
    }

    #[test]
    fn blockquote_single_level_emits_dim_prefix_and_content() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "> quoted text\n");
        assert!(
            out.contains("\x1b[2m\u{2502} \x1b[22m"),
            "expected dim │ prefix: {out:?}",
        );
        assert!(out.contains("quoted text"), "got: {out:?}");
    }

    #[test]
    fn blockquote_nested_emits_two_prefix_units() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "> > nested deep\n");
        let prefix = "\x1b[2m\u{2502} \x1b[22m";
        assert!(
            out.contains(&format!("{prefix}{prefix}")),
            "expected two prefix units in sequence: {out:?}",
        );
        assert!(out.contains("nested deep"), "got: {out:?}");
    }

    #[test]
    fn blockquote_inline_formatting_renders_inside_prefix() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "> **bold** word\n");
        assert!(
            out.contains("\x1b[2m\u{2502} \x1b[22m"),
            "expected dim prefix: {out:?}",
        );
        assert!(out.contains("\x1b[1m"), "expected bold SGR: {out:?}");
        assert!(out.contains("bold"), "bold content: {out:?}");
        assert!(out.contains("word"), "trailing prose: {out:?}");
    }

    #[test]
    fn blockquote_paragraph_break_keeps_prefix_on_blank_line() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "> p1\n>\n> p2\n");
        let prefix = "\x1b[2m\u{2502} \x1b[22m";
        assert!(out.contains("p1"), "first paragraph: {out:?}");
        assert!(out.contains("p2"), "second paragraph: {out:?}");
        // The separator between paragraphs is "\n{prefix}\n{prefix}"
        // so the empty line carries a prefix.
        assert!(
            out.contains(&format!("\n{prefix}\n{prefix}")),
            "expected prefixed blank-line break: {out:?}",
        );
    }

    #[test]
    fn blockquote_bold_spanning_soft_break_stays_bold() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "> **line one\n> continued**\n");
        assert!(out.contains("line one"), "got: {out:?}");
        assert!(out.contains("continued"), "got: {out:?}");
        // The prefix on the second line ends in \x1b[22m, which would
        // cancel the open bold. The fix re-emits \x1b[1m after the
        // prefix so "continued" renders bold.
        let prefix = "\x1b[2m\u{2502} \x1b[22m";
        let last_prefix = out.rfind(prefix).unwrap();
        let after = &out[last_prefix + prefix.len()..];
        assert!(
            after.contains("\x1b[1m"),
            "bold must be re-established after the soft-break prefix: {after:?}",
        );
        assert!(
            after.contains("continued"),
            "continued must follow the restored bold: {after:?}",
        );
    }

    #[test]
    fn blockquote_italic_spanning_soft_break_is_unaffected() {
        // Italic uses SGR 3/23, which the prefix's SGR 2/22 never
        // touches, so italic survives the break without special
        // handling. This pins that we did NOT regress it.
        let caps = caps_with(|c| c.italic_support = true);
        let mut r = MarkdownRenderer::new(caps, 80);
        let out = collect_styled(&mut r, "> *line one\n> continued*\n");
        assert!(out.contains("\x1b[3m"), "italic SGR present: {out:?}");
        assert!(out.contains("line one") && out.contains("continued"));
    }

    #[test]
    fn blockquote_followed_by_plain_text_separates_with_blank_line() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = collect_styled(&mut r, "> quoted\n\nafter\n");
        assert!(out.contains("quoted"));
        assert!(out.contains("after"));
        // Quote ends, paragraph_pending fires at depth 0 -> "\n\n".
        assert!(
            out.contains("\n\nafter") || out.contains("\n\n"),
            "expected paragraph separation after quote: {out:?}",
        );
    }

    #[test]
    fn blockquote_dim_preview_shows_prefix_for_leading_marker() {
        // Feed a partial blockquote line — no \n yet, so the styled
        // path stays empty and we observe the dim preview directly.
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = r.feed("> quoted");
        assert!(
            out.dim.contains('\u{2502}'),
            "dim preview must show │ prefix for `> ` line: {:?}",
            out.dim,
        );
        assert!(
            !out.dim.contains('>'),
            "blockquote marker stripped from dim: {:?}",
            out.dim,
        );
        assert!(
            out.dim.contains("quoted"),
            "content preserved: {:?}",
            out.dim
        );
    }

    #[test]
    fn table_basic_renders_with_unicode_borders() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 40);
        let out = collect_styled(&mut r, "| H1 | H2 |\n| -- | -- |\n| a  | b  |\n");
        assert!(out.contains("H1"), "header cell preserved: {out:?}");
        assert!(out.contains("H2"), "header cell preserved: {out:?}");
        assert!(out.contains('a'), "body cell preserved: {out:?}");
        assert!(out.contains('b'), "body cell preserved: {out:?}");
        // UTF8_FULL_CONDENSED preset uses box-drawing characters; at
        // minimum a vertical bar between cells.
        assert!(
            out.contains('\u{2502}') || out.contains('\u{2500}'),
            "expected Unicode box-drawing borders: {out:?}",
        );
    }

    #[test]
    fn table_header_renders_bold() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 40);
        let out = collect_styled(
            &mut r,
            "| Name | Value |\n| ---- | ----- |\n| x    | 1     |\n",
        );
        assert!(
            out.contains("\x1b[1m") || out.contains("\u{1b}[1m"),
            "expected bold SGR for header cells: {out:?}",
        );
    }

    #[test]
    fn table_with_inline_formatting_preserves_styles() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 40);
        let out = collect_styled(&mut r, "| Col |\n| --- |\n| **bold** |\n| `code` |\n");
        // bold marker survives into the rendered cell content
        assert!(out.contains("\x1b[1m"), "bold SGR in body cell: {out:?}");
        assert!(out.contains("bold"), "cell text preserved: {out:?}");
        assert!(out.contains("code"), "inline code text preserved: {out:?}");
    }

    #[test]
    fn table_with_alignment_renders_without_panic() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 40);
        let out = collect_styled(&mut r, "| L | C | R |\n| :- | :-: | -: |\n| a | b | c |\n");
        assert!(out.contains('a') && out.contains('b') && out.contains('c'));
    }

    #[test]
    fn table_narrow_width_does_not_overflow() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 20);
        let out = collect_styled(&mut r, "| Column |\n| ------ |\n| narrowed |\n");
        // Each line of the rendered table must fit within set_width.
        for line in out.lines() {
            // Strip ANSI for width check.
            let stripped: String = line.chars().filter(|&c| c != '\x1b').collect();
            // Allow some slack for SGR escapes and box-drawing.
            assert!(
                stripped.chars().count() <= 24,
                "line exceeds clamp: {line:?}",
            );
        }
    }

    #[test]
    fn table_single_column_renders_without_panic() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 40);
        let out = collect_styled(&mut r, "| Only |\n| ---- |\n| one  |\n");
        assert!(out.contains("Only"));
        assert!(out.contains("one"));
    }

    #[test]
    fn blockquote_dim_preview_nested_shows_two_prefix_units() {
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let out = r.feed("> > deep");
        assert!(
            out.dim.matches('\u{2502}').count() >= 2,
            "expected two │ glyphs in nested dim preview: {:?}",
            out.dim,
        );
    }

    #[test]
    fn clear_dim_preserves_pending_buffer_for_next_finalize() {
        // Reset the dim flag but leave the partial markdown buffer
        // intact — a subsequent finalize must still flush "Hello " as
        // the styled tail.
        let mut r = MarkdownRenderer::new(TerminalCaps::baseline(), 80);
        let _ = r.feed("Hello ");
        let _ = r.clear_dim();
        let tail = r.finalize();
        assert!(
            tail.styled.contains("Hello "),
            "pending buffer must survive clear_dim and reach finalize: {:?}",
            tail.styled,
        );
    }
}
