//! Shared helpers for keeping tool output bounded in model-facing context.
//!
//! Tools may preserve full data out-of-band when that is useful, but any value
//! that is sent back to the model should pass through these helpers or an
//! equivalent tool-specific cap. This prevents one noisy tool result from
//! consuming the entire context window.

use serde_json::{Map, Value, json};

/// Maximum serialized JSON characters allowed inline for a tool result that is
/// about to be appended to provider messages.
pub const MODEL_OUTPUT_INLINE_CHAR_LIMIT: usize = 12_000;

/// Number of leading and trailing characters retained when a structured value
/// exceeds [`MODEL_OUTPUT_INLINE_CHAR_LIMIT`].
const MODEL_OUTPUT_PREVIEW_CHARS: usize = 2_000;

/// Maximum characters retained from a single regex search matching line.
pub const SEARCH_MATCH_LINE_CHAR_LIMIT: usize = 500;

/// Hard cap on search matches/paths even when the model asks for more.
pub const SEARCH_HARD_MAX_RESULTS: u32 = 100;

/// Default number of lines returned by `read` when no limit is supplied.
pub const READ_DEFAULT_LINE_LIMIT: u64 = 200;

/// Hard cap on lines returned by one `read` invocation.
pub const READ_HARD_LINE_LIMIT: u64 = 250;

/// Maximum characters rendered inline by one `read` invocation.
pub const READ_OUTPUT_CHAR_LIMIT: usize = 20_000;

/// Maximum characters retained for one physical line rendered by `read`.
pub const READ_LINE_CHAR_LIMIT: usize = 1_000;

/// Maximum text characters retained for one AST-search capture.
pub const AST_CAPTURE_TEXT_CHAR_LIMIT: usize = 1_000;

/// Return the serialized character count used by the model-facing budget.
#[must_use]
pub fn serialized_char_count(value: &Value) -> usize {
    model_content_string(value).chars().count()
}

/// Return a provider-message-safe representation of `output`.
///
/// Small values are cloned unchanged. Oversized values become a compact JSON
/// object with size metadata and head/tail previews. This is intentionally a
/// pure transformation: it does not discard the caller's original value or write
/// side files. Event stores and action logs can still keep the full value while
/// provider messages receive this bounded projection.
#[must_use]
pub fn cap_model_output(tool_name: &str, tool_call_id: &str, output: &Value) -> Value {
    let serialized = model_content_string(output);
    let original_chars = serialized.chars().count();
    if original_chars <= MODEL_OUTPUT_INLINE_CHAR_LIMIT {
        return output.clone();
    }

    json!({
        "truncated_for_model": true,
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "original_chars": original_chars,
        "inline_char_limit": MODEL_OUTPUT_INLINE_CHAR_LIMIT,
        "message": "Tool output exceeded the model-facing inline budget. Full structured output remains available in the session event store/action log; use narrower follow-up reads/searches instead of relying on the full payload in context.",
        "head": take_chars(&serialized, MODEL_OUTPUT_PREVIEW_CHARS),
        "tail": tail_chars(&serialized, MODEL_OUTPUT_PREVIEW_CHARS),
    })
}

/// Build a bounded preview wrapper for action-log detail/context queries.
///
/// Unlike [`cap_model_output`], this helper names the field being capped so the
/// caller can embed it in a larger response while making the cap explicit.
#[must_use]
pub fn cap_embedded_value(field_name: &str, value: &Value, limit: usize) -> Value {
    let serialized = model_content_string(value);
    let original_chars = serialized.chars().count();
    if original_chars <= limit {
        return value.clone();
    }

    json!({
        "truncated": true,
        "field": field_name,
        "original_chars": original_chars,
        "inline_char_limit": limit,
        "head": take_chars(&serialized, MODEL_OUTPUT_PREVIEW_CHARS.min(limit)),
        "tail": tail_chars(&serialized, MODEL_OUTPUT_PREVIEW_CHARS.min(limit)),
    })
}

/// Truncate `text` to `limit` characters and report whether truncation occurred.
#[must_use]
pub fn truncate_text(text: &str, limit: usize) -> (String, bool, usize) {
    let original_chars = text.chars().count();
    if original_chars <= limit {
        return (text.to_owned(), false, original_chars);
    }
    (take_chars(text, limit), true, original_chars)
}

/// Insert truncation metadata into an object if truncation occurred.
pub fn insert_truncation_metadata(
    object: &mut Map<String, Value>,
    prefix: &str,
    truncated: bool,
    original_chars: usize,
    limit: usize,
) {
    if !truncated {
        return;
    }
    object.insert(format!("{prefix}_truncated"), Value::Bool(true));
    object.insert(format!("{prefix}_original_chars"), json!(original_chars));
    object.insert(format!("{prefix}_char_limit"), json!(limit));
}

fn model_content_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn take_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn tail_chars(text: &str, limit: usize) -> String {
    let mut chars = text.chars().rev().take(limit).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_model_output_leaves_small_values_unchanged() {
        let value = json!({ "ok": true });
        assert_eq!(cap_model_output("x", "call", &value), value);
    }

    #[test]
    fn cap_model_output_replaces_large_values_with_preview() {
        let value = Value::String("a".repeat(MODEL_OUTPUT_INLINE_CHAR_LIMIT + 1));
        let capped = cap_model_output("read", "call-1", &value);
        assert_eq!(capped["truncated_for_model"], true);
        assert_eq!(capped["tool_name"], "read");
        assert_eq!(capped["tool_call_id"], "call-1");
    }

    #[test]
    fn truncate_text_reports_original_size() {
        let (text, truncated, original) = truncate_text("abcdef", 3);
        assert_eq!(text, "abc");
        assert!(truncated);
        assert_eq!(original, 6);
    }
}
