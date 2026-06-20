//! Pending-buffer scan for the streaming markdown renderer.
//!
//! [`scan_pending`] walks a byte stream looking for unclosed code
//! fences, unclosed inline emphasis markers, likely in-progress tables,
//! and list blocks, returning the earliest position at which it is safe
//! to flush content through pulldown-cmark plus a flag indicating
//! whether the buffer ends inside an unclosed fence (so
//! [`super::MarkdownRenderer::finalize`] can emit the tail verbatim).
//!
//! [`is_plain_text`] is the fast-path predicate used by the parent to
//! short-circuit the markdown parser for unmarked text segments.

mod list;

use list::find_unclosed_list_start;
pub(super) use list::starts_with_list_marker;

/// Which kind of fence is currently open. Backtick fences can only be
/// closed by backtick markers; tilde fences only by tilde markers. The
/// kind is tracked so a mismatched-kind triple-marker line inside a
/// fence is correctly treated as content, not as a closer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FenceKind {
    Backtick,
    Tilde,
}

/// Result of scanning the pending buffer for unclosed markers.
pub(super) struct ScanResult {
    /// Earliest position at which it is safe to flush — content before
    /// this position contains no unclosed inline marker, no open fence,
    /// and no in-progress table.
    pub(super) safe_end: usize,
    /// Whether the buffer ends inside an unclosed code fence. When
    /// `true`, `finalize` flushes the buffer as plain text instead of
    /// invoking the markdown parser.
    pub(super) fence_unclosed: bool,
    /// Whether the buffer currently holds an in-progress pipe table.
    ///
    /// Streaming table previews are suppressed while this is true. A
    /// multi-line dim preview can scroll into permanent history before
    /// the line-pop erase path has a chance to replace it.
    pub(super) table_unclosed: bool,
    /// Whether the buffer currently holds a markdown list block whose
    /// terminator has not yet arrived.
    ///
    /// Lists need block-level buffering for the same reason tables do:
    /// parsing `  - child` without its already-streamed parent line
    /// makes pulldown-cmark treat it as a top-level item, or as
    /// indented code at deeper levels.
    pub(super) list_unclosed: bool,
}

/// Scan `s` for unclosed code fences, inline emphasis markers, likely
/// in-progress tables, and streaming list blocks.
///
/// Walks the input byte-by-byte tracking four pieces of state:
///
/// 1. Triple-backtick and triple-tilde fences open only when the
///    marker appears at line start. A fence closes when a same-kind
///    triple-marker appears at line start with only whitespace
///    following (per `CommonMark`). Mismatched-kind markers inside a
///    fence are content.
/// 2. Outside a fence, `**` toggles bold; a single `*` toggles italic;
///    `~~` toggles strikethrough. Bold and strike are matched greedily
///    before italic so `***` reads as `**` then `*`, and `~~~` outside
///    a fence reads as `~~` then `~` (the latter is harmless plain
///    text).
/// 3. Table-like pipe rows start a streaming-table buffering window
///    that persists until a blank line, a non-table-like line, or
///    end-of-buffer terminates it. Mirrors the fence-buffer pattern so
///    pulldown-cmark sees complete tables rather than line-fragments,
///    without hiding ordinary prose or shell snippets containing a
///    single pipe.
/// 4. List-marker rows start a list-buffering window that persists until
///    a blank line is followed by a non-list, non-indented line. This
///    keeps nested list context intact while the model streams one line
///    at a time.
///
/// Returns the position of the earliest unclosed marker, the earliest
/// open fence, the earliest table header line, or the earliest open list
/// line — whichever comes first — and a flag indicating whether a fence
/// specifically is the unclosed marker.
pub(super) fn scan_pending(s: &str) -> ScanResult {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut fence_kind: Option<FenceKind> = None;
    let mut fence_open: Option<usize> = None;
    let mut bold_open: Option<usize> = None;
    let mut italic_open: Option<usize> = None;
    let mut strike_open: Option<usize> = None;
    let mut table_start: Option<usize> = None;
    let list_start = find_unclosed_list_start(s);
    let mut line_start: usize = 0;

    while i < n {
        // Triple-marker check — fence open or close. Open only at line
        // start; close requires line start + whitespace-only-after.
        if i + 2 < n
            && (bytes[i] == b'`' || bytes[i] == b'~')
            && bytes[i] == bytes[i + 1]
            && bytes[i] == bytes[i + 2]
        {
            let kind = if bytes[i] == b'`' {
                FenceKind::Backtick
            } else {
                FenceKind::Tilde
            };
            let at_line_start = i == line_start;
            match fence_kind {
                Some(open) if open == kind => {
                    if is_valid_fence_close(bytes, i, n) {
                        fence_kind = None;
                        fence_open = None;
                    }
                    i += 3;
                    continue;
                }
                Some(_) => {
                    // Mismatched kind inside fence — content.
                    i += 3;
                    continue;
                }
                None => {
                    if at_line_start {
                        fence_kind = Some(kind);
                        fence_open = Some(i);
                        i += 3;
                        continue;
                    }
                    // Mid-line triple — fall through to inline-marker
                    // detection (e.g. `~~~` reads as `~~` + `~`).
                }
            }
        }
        if fence_kind.is_some() {
            if bytes[i] == b'\n' {
                line_start = i + 1;
            }
            i += 1;
            continue;
        }
        if i + 1 < n && bytes[i] == b'~' && bytes[i + 1] == b'~' {
            if strike_open.is_some() {
                strike_open = None;
            } else {
                strike_open = Some(i);
            }
            i += 2;
            continue;
        }
        if i + 1 < n && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            if bold_open.is_some() {
                bold_open = None;
            } else {
                bold_open = Some(i);
            }
            i += 2;
            continue;
        }
        if bytes[i] == b'*' {
            if italic_open.is_some() {
                italic_open = None;
            } else {
                italic_open = Some(i);
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'\n' {
            let line = &bytes[line_start..i];
            if is_blank_line(line) {
                table_start = None;
            } else if is_table_like_row(line) {
                table_start.get_or_insert(line_start);
            } else {
                table_start = None;
            }
            line_start = i + 1;
        }
        i += 1;
    }

    if line_start < n {
        let line = &bytes[line_start..n];
        if is_table_like_row(line) {
            table_start.get_or_insert(line_start);
        } else if !is_blank_line(line) {
            table_start = None;
        }
    }

    let mut earliest = n;
    if let Some(p) = fence_open {
        earliest = earliest.min(p);
    }
    if let Some(p) = bold_open {
        earliest = earliest.min(p);
    }
    if let Some(p) = italic_open {
        earliest = earliest.min(p);
    }
    if let Some(p) = strike_open {
        earliest = earliest.min(p);
    }
    if let Some(p) = table_start {
        earliest = earliest.min(p);
    }
    if let Some(p) = list_start {
        earliest = earliest.min(p);
    }

    ScanResult {
        safe_end: earliest,
        fence_unclosed: fence_kind.is_some(),
        table_unclosed: table_start.is_some(),
        list_unclosed: list_start.is_some(),
    }
}

/// A closing code fence must appear at the start of a line with only
/// whitespace after the markers (per `CommonMark`). Triple markers
/// followed by a language hint inside a code block are content, not a
/// closer.
fn is_valid_fence_close(bytes: &[u8], pos: usize, n: usize) -> bool {
    let at_line_start = pos == 0 || bytes[pos - 1] == b'\n';
    if !at_line_start {
        return false;
    }
    let after = pos + 3;
    for &b in &bytes[after..n] {
        if b == b'\n' {
            return true;
        }
        if !b.is_ascii_whitespace() {
            return false;
        }
    }
    true
}

fn is_blank_line(line: &[u8]) -> bool {
    line.iter().all(u8::is_ascii_whitespace)
}

fn is_table_like_row(line: &[u8]) -> bool {
    let trimmed = trim_ascii(line);
    if trimmed.is_empty() || !trimmed.contains(&b'|') {
        return false;
    }
    if trimmed.starts_with(b"|") || trimmed.ends_with(b"|") {
        return true;
    }
    let mut cell_count = 0;
    for cell in trimmed.split(|&b| b == b'|') {
        if trim_ascii(cell).is_empty() {
            return false;
        }
        cell_count += 1;
    }
    cell_count >= 3
}

fn trim_ascii(line: &[u8]) -> &[u8] {
    let start = line
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |idx| idx + 1);
    &line[start..end]
}

/// Fast check for whether `s` is pure plain text with no markdown
/// constructs and no newlines. Returns `true` when the segment can be
/// written directly to the scroll region without pulldown-cmark.
pub(super) fn is_plain_text(s: &str) -> bool {
    !s.contains('*')
        && !s.contains('`')
        && !s.contains('[')
        && !s.contains('\n')
        && !s.contains('#')
        && !s.contains('~')
        && !s.contains('|')
        && !s.contains('>')
        && !s.contains('$')
        && !s.contains('<')
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn scan_pending_detects_unclosed_fence() {
        let r = scan_pending("text\n```rust\nfn x() {\n");
        assert!(r.fence_unclosed);
        assert_eq!(r.safe_end, "text\n".len());
    }

    #[test]
    fn scan_pending_detects_closed_fence() {
        let s = "before\n```\nbody\n```\nafter";
        let r = scan_pending(s);
        assert!(!r.fence_unclosed);
        assert_eq!(r.safe_end, s.len());
    }

    #[test]
    fn scan_pending_detects_unclosed_bold() {
        let r = scan_pending("hello **world");
        assert!(!r.fence_unclosed);
        assert_eq!(r.safe_end, "hello ".len());
    }

    #[test]
    fn scan_pending_detects_unclosed_italic() {
        let r = scan_pending("hello *world");
        assert!(!r.fence_unclosed);
        assert_eq!(r.safe_end, "hello ".len());
    }

    #[test]
    fn scan_pending_ignores_markers_inside_fence() {
        let s = "```\n**not bold**\n```\nafter";
        let r = scan_pending(s);
        assert!(!r.fence_unclosed);
        assert_eq!(r.safe_end, s.len());
    }

    #[test]
    fn scan_pending_detects_unclosed_tilde_fence() {
        let r = scan_pending("text\n~~~rust\nfn x()\n");
        assert!(r.fence_unclosed);
        assert_eq!(r.safe_end, "text\n".len());
    }

    #[test]
    fn scan_pending_detects_closed_tilde_fence() {
        let s = "text\n~~~\nbody\n~~~\nafter";
        let r = scan_pending(s);
        assert!(!r.fence_unclosed);
        assert_eq!(r.safe_end, s.len());
    }

    #[test]
    fn scan_pending_tilde_does_not_close_backtick_fence() {
        // A backtick fence is not closed by tilde markers — the tildes
        // are content.
        let r = scan_pending("```\nbody\n~~~\nmore content\n");
        assert!(
            r.fence_unclosed,
            "backtick fence still unclosed after tilde line",
        );
    }

    #[test]
    fn scan_pending_backtick_does_not_close_tilde_fence() {
        let r = scan_pending("~~~\nbody\n```\nmore content\n");
        assert!(
            r.fence_unclosed,
            "tilde fence still unclosed after backtick line",
        );
    }

    #[test]
    fn scan_pending_detects_unclosed_strikethrough() {
        // `~~partial` opens a strike marker that never closes — buffer
        // before the marker so the unclosed `~~` rolls forward.
        let r = scan_pending("hello ~~partial");
        assert!(!r.fence_unclosed);
        assert_eq!(r.safe_end, "hello ".len());
    }

    #[test]
    fn scan_pending_closed_strikethrough_is_safe() {
        let s = "delete ~~this~~ text";
        let r = scan_pending(s);
        assert!(!r.fence_unclosed);
        assert_eq!(r.safe_end, s.len());
    }

    #[test]
    fn scan_pending_table_header_line_holds_safe_end() {
        // An obvious pipe-table row opens a table-buffering window:
        // safe_end clamps back to the line start so pulldown-cmark
        // receives the complete table at once.
        let r = scan_pending("intro\n| H1 | H2 |");
        assert_eq!(r.safe_end, "intro\n".len());
        assert!(r.table_unclosed);
    }

    #[test]
    fn scan_pending_single_pipe_text_does_not_open_table() {
        let s = "run cmd | jq";
        let r = scan_pending(s);
        assert_eq!(r.safe_end, s.len());
        assert!(!r.table_unclosed);
    }

    #[test]
    fn scan_pending_single_separator_pipe_row_does_not_hold() {
        let s = "intro\nA | B\n";
        let r = scan_pending(s);
        assert_eq!(r.safe_end, s.len());
        assert!(!r.table_unclosed);
    }

    #[test]
    fn scan_pending_shell_or_pipe_text_does_not_open_table() {
        let s = "cmd || true";
        let r = scan_pending(s);
        assert_eq!(r.safe_end, s.len());
        assert!(!r.table_unclosed);
    }

    #[test]
    fn scan_pending_table_terminates_on_blank_line() {
        // The blank line after the body closes the table; safe_end
        // advances past the whole table.
        let s = "| H |\n| - |\n| x |\n\nafter";
        let r = scan_pending(s);
        assert_eq!(r.safe_end, s.len());
        assert!(!r.table_unclosed);
    }

    #[test]
    fn scan_pending_table_terminates_on_non_pipe_line() {
        let s = "| H |\n| - |\nplain text\n";
        let r = scan_pending(s);
        assert_eq!(r.safe_end, s.len());
        assert!(!r.table_unclosed);
    }

    #[test]
    fn scan_pending_list_holds_safe_end_at_list_start() {
        let s = "intro\n- parent\n  - child\n";
        let r = scan_pending(s);
        assert_eq!(r.safe_end, "intro\n".len());
        assert!(r.list_unclosed);
    }

    #[test]
    fn scan_pending_list_closes_after_blank_then_plain_line() {
        let s = "- parent\n  - child\n\nplain text\n";
        let r = scan_pending(s);
        assert_eq!(r.safe_end, s.len());
        assert!(!r.list_unclosed);
    }

    #[test]
    fn scan_pending_ignores_list_markers_inside_fence() {
        let s = "```\n- not a list\n```\nafter\n";
        let r = scan_pending(s);
        assert_eq!(r.safe_end, s.len());
        assert!(!r.list_unclosed);
    }

    #[test]
    fn is_plain_text_rejects_tilde() {
        assert!(!is_plain_text("a ~~b~~ c"));
    }

    #[test]
    fn is_plain_text_rejects_pipe() {
        assert!(!is_plain_text("a | b"));
    }

    #[test]
    fn is_plain_text_rejects_blockquote_marker() {
        assert!(!is_plain_text("hello > world"));
    }

    #[test]
    fn is_plain_text_rejects_dollar() {
        assert!(!is_plain_text("price $5"));
    }

    #[test]
    fn is_plain_text_rejects_angle() {
        assert!(!is_plain_text("html <br> tag"));
    }

    #[test]
    fn is_plain_text_accepts_plain() {
        assert!(is_plain_text("hello world"));
    }
}
