//! Shared helpers for keeping tool output bounded in model-facing context.
//!
//! Tools may preserve full data out-of-band when that is useful, but any value
//! that is sent back to the model should pass through these helpers or an
//! equivalent tool-specific cap. This prevents one noisy tool result from
//! consuming the entire context window.

use serde_json::{Map, Value, json};

/// Absolute maximum serialized JSON characters retained in one persisted
/// model-facing tool result after the generic emergency cap.
pub const MODEL_OUTPUT_INLINE_CHAR_LIMIT: usize = 64_000;

/// Number of leading and trailing characters retained when a structured value
/// exceeds [`MODEL_OUTPUT_INLINE_CHAR_LIMIT`].
const MODEL_OUTPUT_PREVIEW_CHARS: usize = 4_000;

/// Maximum characters retained from a single regex search matching line.
pub const SEARCH_MATCH_LINE_CHAR_LIMIT: usize = 500;

/// Hard cap on search matches/paths even when the model asks for more.
pub const SEARCH_HARD_MAX_RESULTS: u32 = 100;

/// Default number of lines returned by `read` when no limit is supplied.
pub const READ_DEFAULT_LINE_LIMIT: u64 = 200;

/// Hard cap on lines returned by one `read` invocation.
pub const READ_HARD_LINE_LIMIT: u64 = 250;

/// Fallback characters rendered inline by one `read` invocation when no model
/// context window is known.
pub const READ_OUTPUT_CHAR_LIMIT: usize = 32_000;

/// Smallest model-derived read character budget.
pub const READ_MIN_OUTPUT_CHAR_LIMIT: usize = 8_000;

/// Largest default read character budget derived from model context.
pub const READ_MAX_OUTPUT_CHAR_LIMIT: usize = 32_000;

/// Absolute read character ceiling, including explicit `limit` requests.
pub const READ_HARD_OUTPUT_CHAR_LIMIT: usize = 64_000;

/// Converts a token context window into a conservative character read budget.
const READ_CONTEXT_WINDOW_DIVISOR: u64 = 8;

/// Maximum characters retained for one physical line rendered by `read`.
pub const READ_LINE_CHAR_LIMIT: usize = 1_000;

/// Maximum text characters retained for one AST-search capture.
pub const AST_CAPTURE_TEXT_CHAR_LIMIT: usize = 1_000;

/// Runtime budget for text a tool may return into model-facing context.
///
/// Embedders install this on [`ToolContext`](crate::tool::context::ToolContext)
/// after model/profile resolution. The defaults are conservative so tools stay
/// bounded even when no model metadata exists for a local/custom backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolOutputBudget {
    /// Default read line window when the model omits `limit`.
    pub read_default_line_limit: u64,
    /// Maximum read line window accepted from an explicit `limit`.
    pub read_hard_line_limit: u64,
    /// Default read character budget.
    pub read_output_char_limit: usize,
    /// Maximum read character budget accepted from an explicit request.
    pub read_hard_output_char_limit: usize,
    /// Maximum characters retained from a single physical line.
    pub read_line_char_limit: usize,
    /// Generic emergency cap for one serialized tool result.
    pub model_output_inline_char_limit: usize,
}

impl ToolOutputBudget {
    /// Build a budget from an optional model context window in tokens.
    ///
    /// The window chooses a default read size, then clamps it so a very large
    /// model does not imply unbounded reads and a small model gets small chunks.
    #[must_use]
    pub fn for_context_window(context_window_tokens: Option<u64>) -> Self {
        let derived = context_window_tokens
            .and_then(|tokens| usize::try_from(tokens / READ_CONTEXT_WINDOW_DIVISOR).ok())
            .map_or(READ_OUTPUT_CHAR_LIMIT, |chars| {
                chars.clamp(READ_MIN_OUTPUT_CHAR_LIMIT, READ_MAX_OUTPUT_CHAR_LIMIT)
            });

        Self {
            read_default_line_limit: READ_DEFAULT_LINE_LIMIT,
            read_hard_line_limit: READ_HARD_LINE_LIMIT,
            read_output_char_limit: derived,
            read_hard_output_char_limit: READ_HARD_OUTPUT_CHAR_LIMIT,
            read_line_char_limit: READ_LINE_CHAR_LIMIT,
            model_output_inline_char_limit: MODEL_OUTPUT_INLINE_CHAR_LIMIT,
        }
    }
}

impl Default for ToolOutputBudget {
    fn default() -> Self {
        Self::for_context_window(None)
    }
}

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

    let mut capped = Map::new();
    preserve_identity_fields(output, &mut capped);
    capped.insert("truncated_for_model".to_owned(), Value::Bool(true));
    capped.insert("tool_name".to_owned(), json!(tool_name));
    capped.insert("tool_call_id".to_owned(), json!(tool_call_id));
    capped.insert("original_chars".to_owned(), json!(original_chars));
    capped.insert(
        "inline_char_limit".to_owned(),
        json!(MODEL_OUTPUT_INLINE_CHAR_LIMIT),
    );
    capped.insert(
        "message".to_owned(),
        json!(
            "Tool output exceeded the model-facing inline budget and was \
             replaced with a bounded head/tail sample. Use narrower read, \
             search, head, tail, or grep-style follow-ups instead of \
             relying on the full payload in context."
        ),
    );
    capped.insert(
        "head".to_owned(),
        json!(take_chars(&serialized, MODEL_OUTPUT_PREVIEW_CHARS)),
    );
    capped.insert(
        "tail".to_owned(),
        json!(tail_chars(&serialized, MODEL_OUTPUT_PREVIEW_CHARS)),
    );
    Value::Object(capped)
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

fn preserve_identity_fields(output: &Value, capped: &mut Map<String, Value>) {
    let Some(object) = output.as_object() else {
        return;
    };
    for key in [
        "error",
        "follow_ups",
        "path",
        "kind",
        "output_path",
        "output_redirected",
        "exit_code",
        "timed_out",
        "warnings",
    ] {
        if let Some(value) = object.get(key) {
            capped.insert(key.to_owned(), value.clone());
        }
    }
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
    fn read_budget_scales_with_context_window_inside_bounds() {
        let small = ToolOutputBudget::for_context_window(Some(64_000));
        assert_eq!(small.read_output_char_limit, READ_MIN_OUTPUT_CHAR_LIMIT);

        let large = ToolOutputBudget::for_context_window(Some(250_000));
        assert_eq!(large.read_output_char_limit, 31_250);

        let huge = ToolOutputBudget::for_context_window(Some(1_000_000));
        assert_eq!(huge.read_output_char_limit, READ_MAX_OUTPUT_CHAR_LIMIT);
    }

    #[test]
    fn cap_model_output_preserves_follow_ups_and_error() {
        let value = json!({
            "error": { "kind": "execution_failed", "message": "bad" },
            "follow_ups": [{ "action": "next" }],
            "content": "a".repeat(MODEL_OUTPUT_INLINE_CHAR_LIMIT + 1),
        });
        let capped = cap_model_output("read", "call-1", &value);
        assert_eq!(capped["truncated_for_model"], true);
        assert_eq!(capped["error"]["message"], "bad");
        assert_eq!(capped["follow_ups"][0]["action"], "next");
    }

    #[test]
    fn truncate_text_reports_original_size() {
        let (text, truncated, original) = truncate_text("abcdef", 3);
        assert_eq!(text, "abc");
        assert!(truncated);
        assert_eq!(original, 6);
    }
}
