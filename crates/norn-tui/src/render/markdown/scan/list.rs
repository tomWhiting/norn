//! List-block detection for the streaming markdown scanner.
//!
//! `pulldown-cmark` needs the parent list item in the same parse window
//! to classify nested rows. This scanner keeps a list block pending
//! until a real block boundary is visible instead of flushing each line
//! independently.

use super::{FenceKind, is_blank_line};

/// Return whether the first non-blank source line starts a top-level
/// markdown list item.
pub(in crate::render::markdown) fn starts_with_list_marker(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut line_start = 0usize;

    while line_start < bytes.len() {
        let (line_end, next_start) = line_bounds(bytes, line_start);
        let line = trim_trailing_carriage_return(&bytes[line_start..line_end]);
        if is_blank_line(line) {
            line_start = next_start;
            continue;
        }
        return is_list_marker_line(line, true);
    }

    false
}

/// Return the byte offset of an open markdown list block, if `source`
/// ends while that list still needs future context.
pub(super) fn find_unclosed_list_start(source: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut line_start = 0usize;
    let mut fence_kind: Option<FenceKind> = None;
    let mut list_start: Option<usize> = None;
    let mut blank_after_list = false;

    while line_start < bytes.len() {
        let (line_end, next_start) = line_bounds(bytes, line_start);
        let line = &bytes[line_start..line_end];
        let fence_opener = fence_kind
            .is_none()
            .then(|| opening_fence_kind(line))
            .flatten();

        if list_start.is_some() && blank_after_list && fence_opener.is_some() {
            list_start = None;
            blank_after_list = false;
        }
        update_fence(&mut fence_kind, line, fence_opener);
        if fence_kind.is_some() || is_fence_closer(line) {
            line_start = next_start;
            continue;
        }

        if let Some(start) = list_start {
            if is_blank_line(line) {
                blank_after_list = true;
            } else if is_list_marker_line(line, false) || is_indented_continuation(line) {
                blank_after_list = false;
            } else if blank_after_list {
                list_start = None;
                blank_after_list = false;
            } else {
                list_start = Some(start);
            }
        } else if is_list_marker_line(line, true) {
            list_start = Some(line_start);
        }

        line_start = next_start;
    }

    list_start
}

fn line_bounds(bytes: &[u8], start: usize) -> (usize, usize) {
    let rel_end = bytes[start..]
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(bytes.len() - start);
    let end = start + rel_end;
    let next = if end < bytes.len() { end + 1 } else { end };
    (end, next)
}

fn update_fence(kind: &mut Option<FenceKind>, line: &[u8], opener: Option<FenceKind>) {
    match (*kind, opener) {
        (Some(open), _) if is_closing_fence(line, open) => *kind = None,
        (None, Some(open)) => *kind = Some(open),
        _ => {}
    }
}

fn opening_fence_kind(line: &[u8]) -> Option<FenceKind> {
    let trimmed = trim_leading_spaces(line);
    if leading_spaces(line) > 3 || trimmed.len() < 3 {
        return None;
    }
    match &trimmed[..3] {
        b"```" => Some(FenceKind::Backtick),
        b"~~~" => Some(FenceKind::Tilde),
        _ => None,
    }
}

fn is_closing_fence(line: &[u8], kind: FenceKind) -> bool {
    let trimmed = trim_leading_spaces(line);
    if leading_spaces(line) > 3 || trimmed.len() < 3 {
        return false;
    }
    let marker = match kind {
        FenceKind::Backtick => b'`',
        FenceKind::Tilde => b'~',
    };
    trimmed.starts_with(&[marker, marker, marker])
        && trimmed[3..].iter().all(u8::is_ascii_whitespace)
}

fn is_fence_closer(line: &[u8]) -> bool {
    is_closing_fence(line, FenceKind::Backtick) || is_closing_fence(line, FenceKind::Tilde)
}

fn is_list_marker_line(line: &[u8], top_level: bool) -> bool {
    let indent = leading_spaces(line);
    if top_level && indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    is_unordered_marker(rest) || is_ordered_marker(rest)
}

fn is_unordered_marker(rest: &[u8]) -> bool {
    matches!(rest.first(), Some(b'-' | b'+' | b'*'))
        && rest.get(1).is_none_or(u8::is_ascii_whitespace)
}

fn is_ordered_marker(rest: &[u8]) -> bool {
    let digits = rest
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .take(10)
        .count();
    if digits == 0 || digits > 9 {
        return false;
    }
    matches!(rest.get(digits), Some(b'.' | b')'))
        && rest.get(digits + 1).is_none_or(u8::is_ascii_whitespace)
}

fn is_indented_continuation(line: &[u8]) -> bool {
    leading_spaces(line) > 0
}

fn leading_spaces(line: &[u8]) -> usize {
    line.iter().take_while(|b| **b == b' ').count()
}

fn trim_leading_spaces(line: &[u8]) -> &[u8] {
    &line[leading_spaces(line)..]
}

fn trim_trailing_carriage_return(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}
