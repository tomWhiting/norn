//! List-aware dim preview for buffered streaming markdown.
//!
//! The final render still goes through pulldown-cmark. This preview is
//! intentionally conservative: it strips list markers for the common
//! ordered/unordered shapes while leaving continuation lines readable.

use std::fmt::Write as _;

use crate::terminal::caps::TerminalCaps;

use super::{emit_inline_dim, strip_blockquote_markers};

pub(in crate::render::markdown) fn render_list_dim_preview(
    pending: &str,
    caps: &TerminalCaps,
) -> String {
    let mut out = String::with_capacity(pending.len() + 16);
    let mut first = true;
    for line in pending.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;
        render_list_dim_line(line, caps, &mut out);
    }
    out
}

fn render_list_dim_line(line: &str, caps: &TerminalCaps, out: &mut String) {
    let (quote_depth, after_quote) = strip_blockquote_markers(line);
    for _ in 0..quote_depth {
        out.push_str("\u{2502} ");
    }
    let Some(item) = parse_list_item(after_quote) else {
        emit_inline_dim(after_quote, caps, out);
        return;
    };
    for _ in 0..item.depth {
        out.push_str("  ");
    }
    match item.marker {
        ListMarker::Unordered => {
            out.push(bullet_for_depth(item.depth));
            out.push(' ');
        }
        ListMarker::Ordered(marker) => {
            let _ = write!(out, "{marker} ");
        }
    }
    emit_inline_dim(item.content, caps, out);
}

struct ListItem<'a> {
    depth: usize,
    marker: ListMarker<'a>,
    content: &'a str,
}

enum ListMarker<'a> {
    Unordered,
    Ordered(&'a str),
}

fn parse_list_item(line: &str) -> Option<ListItem<'_>> {
    let indent = line.as_bytes().iter().take_while(|b| **b == b' ').count();
    let rest = &line[indent..];
    parse_unordered(rest)
        .or_else(|| parse_ordered(rest))
        .map(|(marker, content)| ListItem {
            depth: depth_for_indent(indent, matches!(marker, ListMarker::Ordered(_))),
            marker,
            content,
        })
}

fn parse_unordered(rest: &str) -> Option<(ListMarker<'_>, &str)> {
    let bytes = rest.as_bytes();
    if !matches!(bytes.first(), Some(b'-' | b'+' | b'*')) {
        return None;
    }
    if !bytes.get(1).is_none_or(u8::is_ascii_whitespace) {
        return None;
    }
    Some((ListMarker::Unordered, rest.get(2..).unwrap_or_default()))
}

fn parse_ordered(rest: &str) -> Option<(ListMarker<'_>, &str)> {
    let bytes = rest.as_bytes();
    let digits = bytes
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .take(10)
        .count();
    if digits == 0 || digits > 9 || !matches!(bytes.get(digits), Some(b'.' | b')')) {
        return None;
    }
    if !bytes.get(digits + 1).is_none_or(u8::is_ascii_whitespace) {
        return None;
    }
    let marker_end = digits + 1;
    let marker = &rest[..marker_end];
    let content_start = marker_end + 1;
    Some((
        ListMarker::Ordered(marker),
        rest.get(content_start..).unwrap_or_default(),
    ))
}

fn depth_for_indent(indent: usize, ordered: bool) -> usize {
    let unit = if ordered { 3 } else { 2 };
    indent / unit
}

fn bullet_for_depth(depth: usize) -> char {
    match depth {
        0 => '\u{2022}',
        1 => '\u{25E6}',
        2 => '\u{25AA}',
        _ => '\u{2023}',
    }
}
