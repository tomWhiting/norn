//! Shared text-shaping helpers used across fixed-panel renderers.
//!
//! Lives outside [`super::style`] (which deals with SGR escapes / colour
//! mapping) so the autocomplete popup, the streaming indicator's
//! tool-in-flight mode, and the future activity log can all pull their
//! width-truncation utility from one place.

use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

/// Truncate `text` to at most `width` display columns.
///
/// Walks character-by-character accumulating the Unicode display width
/// and stops at the first character that would exceed `width`. Returns
/// the resulting prefix as an owned `String` so the caller can render
/// it without further processing. Zero-width characters never push the
/// accumulator over the limit and therefore survive.
pub(crate) fn truncate_to_width(text: &str, width: u16) -> String {
    if text.width() <= usize::from(width) {
        return text.to_owned();
    }
    let limit = usize::from(width);
    let mut acc = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if used + ch_width > limit {
            break;
        }
        used += ch_width;
        acc.push(ch);
    }
    acc
}

/// Truncate `text` to at most `width` display columns, appending a
/// trailing single-codepoint Unicode ellipsis (`…`) when truncation
/// occurred.
///
/// The ellipsis itself occupies one display column, so the returned
/// string fits in `width` columns total. When `width < 1`, returns an
/// empty string — no room for either content or the ellipsis. When the
/// untruncated text already fits, returns it unchanged with no
/// ellipsis appended.
pub(crate) fn truncate_with_ellipsis(text: &str, width: u16) -> String {
    if text.width() <= usize::from(width) {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let body = truncate_to_width(text, width.saturating_sub(1));
    let mut out = body;
    out.push('\u{2026}');
    out
}

/// Group a token count with comma thousands separators.
///
/// `12345` → `"12,345"`. Counts of three digits or fewer pass through
/// unchanged. Used by both the status bar and the usage-summary
/// formatter so the two surfaces stay consistent.
pub(crate) fn format_count(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    if len <= 3 {
        return s;
    }
    let mut result = String::with_capacity(len + len / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn truncate_to_width_passes_through_when_short_enough() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
    }

    #[test]
    fn truncate_to_width_stops_at_width_boundary() {
        assert_eq!(truncate_to_width("hello world", 5), "hello");
    }

    #[test]
    fn truncate_to_width_handles_wide_chars() {
        // CJK characters are width-2; "你好" is 4 columns total.
        assert_eq!(truncate_to_width("你好", 4), "你好");
        assert_eq!(truncate_to_width("你好", 3), "你");
        assert_eq!(truncate_to_width("你好", 2), "你");
        assert_eq!(truncate_to_width("你好", 1), "");
    }

    #[test]
    fn truncate_to_width_zero_returns_empty() {
        assert_eq!(truncate_to_width("hello", 0), "");
    }

    #[test]
    fn truncate_with_ellipsis_appends_when_truncated() {
        // "hello world" is 11 cols; width 7 → "hello " + … = 7 cols.
        let out = truncate_with_ellipsis("hello world", 7);
        assert!(out.ends_with('\u{2026}'), "got {out:?}");
        assert_eq!(out.chars().count(), 7);
    }

    #[test]
    fn truncate_with_ellipsis_passes_through_when_short_enough() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn truncate_with_ellipsis_width_zero_returns_empty() {
        assert_eq!(truncate_with_ellipsis("hello", 0), "");
    }

    #[test]
    fn truncate_with_ellipsis_width_one_returns_single_ellipsis() {
        let out = truncate_with_ellipsis("hello", 1);
        assert_eq!(out, "\u{2026}");
    }

    #[test]
    fn format_count_passthrough_for_small_values() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(1), "1");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn format_count_adds_commas_at_thousands() {
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(12_345), "12,345");
        assert_eq!(format_count(1_000_000), "1,000,000");
        assert_eq!(format_count(999_999_999), "999,999,999");
    }
}
