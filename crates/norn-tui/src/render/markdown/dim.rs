//! Dim-preview rendering for the streaming markdown renderer.
//!
//! [`render_dim_preview`] walks a pending buffer, strips closed
//! `**bold**` / `*italic*` / `` `code` `` spans and ATX heading markers,
//! and wraps the inner content in the appropriate SGR. The caller wraps
//! the returned string in `\x1b[2m … \x1b[22m` (dim intensity on/off)
//! before writing it to the scroll region.
//!
//! Closed spans render with their attribute; unbalanced markers pass
//! through as literal text so the in-flight state stays visible until
//! the marker closes (at which point the next `\n` flushes the line
//! styled via [`super::MarkdownRenderer::try_flush_styled`]).

use std::fmt::Write as _;

use termina::escape::csi::{Csi, Sgr};
use termina::style::{ColorSpec, Intensity};

use crate::render::style::{colour_for, italic};
use crate::terminal::caps::TerminalCaps;

use super::{INLINE_CODE_COLOUR, italic_off};

/// Render `pending` as a markdown-aware dim preview.
///
/// Strips ATX heading markers (`# `, `## `, …) at line start and bolds
/// the heading text; strips closed `**bold**`, `*italic*`, and
/// `` `inline code` `` spans and applies the matching attribute; leaves
/// unbalanced markers as literal text so the in-flight state stays
/// visible until the marker closes (at which point the next `\n` will
/// flush the line styled via [`super::MarkdownRenderer::try_flush_styled`]).
///
/// The caller wraps the returned string in `\x1b[2m … \x1b[22m` (dim
/// intensity on/off). Inside that wrapper:
///
/// - Italic uses `\x1b[3m … \x1b[23m` — italic toggles independently of
///   intensity, so the wrapped text renders as "dim italic" on every
///   terminal.
/// - Inline code emits a distinct foreground colour then resets — the
///   colour swap does not touch intensity, so the dim wrap is preserved
///   across the span.
/// - Bold uses `\x1b[1m … \x1b[22m\x1b[2m`. ANSI lets only one
///   intensity attribute be active at once: setting bold implicitly
///   cancels dim. Closing the bold span with `\x1b[22m` clears both,
///   so we re-emit `\x1b[2m` to restore dim for any plain text that
///   follows. The bold span itself renders at full bold intensity (not
///   dim) — that's a visible inconsistency with the surrounding dim,
///   but it surfaces structure to the user during the streaming preview
///   and is the only way to combine the two in standard SGR.
pub(super) fn render_dim_preview(pending: &str, caps: &TerminalCaps) -> String {
    let mut out = String::with_capacity(pending.len() + 16);
    let mut first = true;
    for line in pending.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;
        render_dim_line(line, caps, &mut out);
    }
    out
}

/// Render one logical line of the dim preview, peeling off leading
/// blockquote markers and the ATX heading marker, then forwarding the
/// remaining content through [`emit_inline_dim`].
///
/// Blockquote prefixes render as bare U+2502 `\u{2502}` glyphs — the
/// dispatch layer wraps the whole dim output in dim-intensity SGR, so
/// the prefix inherits dim without explicit `\x1b[2m`. The styled-side
/// emitter writes the prefix with explicit dim because its surrounding
/// content is normal intensity.
fn render_dim_line(line: &str, caps: &TerminalCaps, out: &mut String) {
    let (depth, after_quote) = strip_blockquote_markers(line);
    for _ in 0..depth {
        out.push_str("\u{2502} ");
    }
    if let Some(after_marker) = strip_heading_marker(after_quote) {
        let _ = write!(out, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Bold)));
        emit_inline_dim(after_marker, caps, out);
        let _ = write!(out, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Normal)));
        // Restore the outer dim wrapper that `Intensity::Normal` just
        // cancelled; no-op when this is the last byte of the dim output
        // (the dispatch layer's outer `\x1b[22m` cancels it again).
        out.push_str("\x1b[2m");
    } else {
        emit_inline_dim(after_quote, caps, out);
    }
}

/// Peel zero-or-more `>` blockquote markers from the start of `line`.
///
/// `CommonMark` accepts `>` with or without a trailing space; both
/// count as a marker. Returns the marker depth and the slice after the
/// final marker so the caller can continue with heading / inline-dim
/// detection on the quoted content.
fn strip_blockquote_markers(line: &str) -> (usize, &str) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut depth = 0;
    while i < bytes.len() && bytes[i] == b'>' {
        depth += 1;
        i += 1;
        if i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
    }
    (depth, &line[i..])
}

/// If `line` starts with an ATX heading marker (1–6 `#` chars followed
/// by a space), return the slice after the marker and its space.
/// Returns `None` for everything else.
fn strip_heading_marker(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut hash_count = 0;
    while hash_count < bytes.len() && bytes[hash_count] == b'#' {
        hash_count += 1;
    }
    if !(1..=6).contains(&hash_count) {
        return None;
    }
    if bytes.get(hash_count) != Some(&b' ') {
        return None;
    }
    Some(&line[hash_count + 1..])
}

/// Walk `content` and append a styled dim representation to `out`.
///
/// Closed `**bold**`, `~~strike~~`, `*italic*`, and `` `code` `` spans
/// have their markers stripped and the inner content wrapped in the
/// appropriate SGR. Empty spans (`**`/`* *` with nothing between) are
/// passed through as literal text — markdown forbids them and they're
/// invariably mid-stream state, not finished syntax.
///
/// Strike uses `\x1b[9m … \x1b[29m` — the crossed-out attribute toggles
/// independently of intensity, so the dim wrap is preserved across the
/// span (same property as italic).
fn emit_inline_dim(content: &str, caps: &TerminalCaps, out: &mut String) {
    let bytes = content.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut plain_start = 0;
    while i < n {
        if i + 1 < n
            && bytes[i] == b'*'
            && bytes[i + 1] == b'*'
            && let Some(close) = find_marker(bytes, i + 2, b"**")
            && close > i + 2
        {
            flush_plain(content, plain_start, i, out);
            let _ = write!(out, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Bold)));
            out.push_str(&content[i + 2..close]);
            let _ = write!(out, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Normal)));
            out.push_str("\x1b[2m");
            i = close + 2;
            plain_start = i;
            continue;
        }
        if i + 1 < n
            && bytes[i] == b'~'
            && bytes[i + 1] == b'~'
            && let Some(close) = find_marker(bytes, i + 2, b"~~")
            && close > i + 2
        {
            flush_plain(content, plain_start, i, out);
            out.push_str("\x1b[9m");
            out.push_str(&content[i + 2..close]);
            out.push_str("\x1b[29m");
            i = close + 2;
            plain_start = i;
            continue;
        }
        if bytes[i] == b'*'
            && let Some(close) = find_marker(bytes, i + 1, b"*")
            && close > i + 1
        {
            flush_plain(content, plain_start, i, out);
            let _ = write!(out, "{}", Csi::Sgr(italic(caps)));
            out.push_str(&content[i + 1..close]);
            let _ = write!(out, "{}", Csi::Sgr(italic_off(caps)));
            i = close + 1;
            plain_start = i;
            continue;
        }
        if bytes[i] == b'`'
            && let Some(close) = find_marker(bytes, i + 1, b"`")
            && close > i + 1
        {
            flush_plain(content, plain_start, i, out);
            out.push_str(&colour_for(INLINE_CODE_COLOUR, caps));
            out.push_str(&content[i + 1..close]);
            let _ = write!(out, "{}", Csi::Sgr(Sgr::Foreground(ColorSpec::Reset)));
            i = close + 1;
            plain_start = i;
            continue;
        }
        i += 1;
    }
    flush_plain(content, plain_start, n, out);
}

fn flush_plain(content: &str, start: usize, end: usize, out: &mut String) {
    if start < end {
        out.push_str(&content[start..end]);
    }
}

/// Search `bytes[from..]` for the first occurrence of `marker`.
fn find_marker(bytes: &[u8], from: usize, marker: &[u8]) -> Option<usize> {
    let mlen = marker.len();
    if mlen == 0 || from + mlen > bytes.len() {
        return None;
    }
    let mut i = from;
    while i + mlen <= bytes.len() {
        if &bytes[i..i + mlen] == marker {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn strip_heading_marker_one_hash() {
        assert_eq!(strip_heading_marker("# Title"), Some("Title"));
    }

    #[test]
    fn strip_heading_marker_six_hashes() {
        assert_eq!(strip_heading_marker("###### h6"), Some("h6"));
    }

    #[test]
    fn strip_heading_marker_no_space_rejects() {
        assert_eq!(strip_heading_marker("#Title"), None);
    }

    #[test]
    fn strip_heading_marker_seven_hashes_rejects() {
        assert_eq!(strip_heading_marker("####### too deep"), None);
    }

    #[test]
    fn strip_heading_marker_plain_text_returns_none() {
        assert_eq!(strip_heading_marker("hello"), None);
    }

    #[test]
    fn strip_heading_marker_hash_only_returns_none() {
        assert_eq!(strip_heading_marker("##"), None);
    }

    #[test]
    fn dim_preview_strips_heading_marker_and_bolds() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("# Title", &caps);
        assert!(
            !out.contains('#'),
            "ATX heading marker must not leak into dim preview: {out:?}"
        );
        assert!(
            out.contains("Title"),
            "heading text must be present: {out:?}"
        );
        assert!(out.contains("\x1b[1m"), "expected bold SGR: {out:?}");
    }

    #[test]
    fn dim_preview_strips_closed_bold_markers() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("a **bold** b", &caps);
        assert!(
            !out.contains("**"),
            "bold markers must be stripped: {out:?}"
        );
        assert!(out.contains("bold"), "bold content must remain: {out:?}");
        assert!(out.contains("\x1b[1m"), "expected bold SGR: {out:?}");
        assert!(
            out.contains("a "),
            "leading plain text must survive: {out:?}"
        );
        assert!(
            out.contains(" b"),
            "trailing plain text must survive: {out:?}"
        );
    }

    #[test]
    fn dim_preview_strips_closed_italic_markers() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("a *em* b", &caps);
        assert!(out.contains("em"), "italic content must remain: {out:?}");
        assert!(out.contains("a "));
        assert!(out.contains(" b"));
    }

    #[test]
    fn dim_preview_strips_closed_inline_code_markers() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("call `func` here", &caps);
        assert!(!out.contains('`'), "backticks must be stripped: {out:?}");
        assert!(
            out.contains("func"),
            "inline-code content must remain: {out:?}"
        );
        let inline_code = colour_for(INLINE_CODE_COLOUR, &caps);
        assert!(
            out.contains(&inline_code),
            "expected inline-code colour: {out:?}"
        );
    }

    #[test]
    fn dim_preview_unclosed_bold_passes_through() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("**partial", &caps);
        assert!(
            out.contains("**partial"),
            "unclosed bold must pass through literally: {out:?}"
        );
        assert!(
            !out.contains("\x1b[1m"),
            "no bold SGR for unclosed marker: {out:?}"
        );
    }

    #[test]
    fn dim_preview_unclosed_italic_passes_through() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("*partial", &caps);
        assert!(
            out.contains("*partial"),
            "unclosed italic must pass through literally: {out:?}"
        );
    }

    #[test]
    fn dim_preview_unclosed_inline_code_passes_through() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("partial `code", &caps);
        assert!(
            out.contains("partial `code"),
            "unclosed code must pass through literally: {out:?}"
        );
    }

    #[test]
    fn dim_preview_reestablishes_dim_after_bold_span() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("a **b** c", &caps);
        let bold_off = "\x1b[22m";
        let dim_on = "\x1b[2m";
        assert!(
            out.contains(&format!("{bold_off}{dim_on}")),
            "expected `Intensity::Normal` immediately followed by dim re-establish: {out:?}",
        );
    }

    #[test]
    fn dim_preview_heading_with_inline_bold() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("# Title with **bold** word", &caps);
        assert!(!out.contains('#'), "heading marker stripped: {out:?}");
        assert!(!out.contains("**"), "inline bold markers stripped: {out:?}");
        assert!(out.contains("Title with "));
        assert!(out.contains("bold"));
        assert!(out.contains(" word"));
    }

    #[test]
    fn dim_preview_strips_closed_strikethrough_markers() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("a ~~deleted~~ b", &caps);
        assert!(!out.contains("~~"), "strike markers stripped: {out:?}");
        assert!(out.contains("deleted"), "strike content remains: {out:?}");
        assert!(out.contains("\x1b[9m"), "expected SGR 9: {out:?}");
        assert!(out.contains("\x1b[29m"), "expected SGR 29 close: {out:?}");
    }

    #[test]
    fn dim_preview_unclosed_strikethrough_passes_through() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("~~partial", &caps);
        assert!(
            out.contains("~~partial"),
            "unclosed strike passes through literally: {out:?}",
        );
        assert!(
            !out.contains("\x1b[9m"),
            "no SGR 9 for unclosed marker: {out:?}",
        );
    }

    #[test]
    fn dim_preview_empty_pending_returns_empty() {
        let caps = TerminalCaps::baseline();
        let out = render_dim_preview("", &caps);
        assert!(out.is_empty(), "empty pending yields empty dim: {out:?}");
    }
}
