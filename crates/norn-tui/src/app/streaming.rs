//! Display eligibility for demanded streaming prefixes; the semantic owner retains all bytes.

use unicode_segmentation::UnicodeSegmentation;

/// Withhold the final possible grapheme when a body range has more original bytes.
/// UTF-8-safe page boundaries alone cannot prove an extended grapheme is complete.
#[must_use]
pub(super) fn complete_prefix(original: &str, has_more: bool) -> &str {
    if has_more {
        original
            .grapheme_indices(true)
            .next_back()
            .map_or("", |(last, _)| &original[..last])
    } else {
        original
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn demanded_prefix_keeps_zwj_and_combining_tail_out_of_paint() {
        assert_eq!(complete_prefix("A👩\u{200d}", true), "A");
        assert_eq!(complete_prefix("Ae\u{301}", true), "A");
        assert_eq!(complete_prefix("Ae\u{301}", false), "Ae\u{301}");
    }
}
