//! `read` tool renderer.
//!
//! Renders `read` tool calls: a `> {path}` header with an optional line
//! range. The body carries the line-numbered file content but is hidden
//! by default — the consumer decides whether to show it. The JSON
//! shapes consumed here are produced by `crates/norn/src/tools/read.rs`;
//! field access is defensive so a missing or mistyped field degrades
//! gracefully rather than panicking.

use serde_json::Value;

use crate::render::content::ContentBlock;
use crate::terminal::caps::TerminalCaps;
use crate::tools::helpers::{
    RED, SPINNER, fg, fg_reset, format_duration_ms, partial_field, string_field,
};
use crate::tools::renderer::ToolRenderer;

/// Renders `read` tool calls: a `> {path}` header with an optional line
/// range. The body carries the line-numbered file content but is hidden
/// by default — the consumer decides whether to show it.
pub struct ReadRenderer;

impl ToolRenderer for ReadRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        duration_ms: u64,
        caps: &TerminalCaps,
    ) -> String {
        let path = string_field(args, result, "path");
        let duration = format_duration_ms(duration_ms);
        match result.get("kind").and_then(Value::as_str).unwrap_or("text") {
            "binary" => {
                let size = result
                    .get("size_bytes")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                format!("> {path}  [binary, {size} bytes]")
            }
            "image" => format!("> {path}  [image]"),
            "io_error" => {
                // The `error` field is the typed payload object
                // ({kind, message, ...}); a bare string is the legacy
                // gating form, kept as a fallback.
                let error_field = result.get("error");
                let error = error_field
                    .and_then(|v| v.get("message"))
                    .and_then(Value::as_str)
                    .or_else(|| error_field.and_then(Value::as_str))
                    .unwrap_or("");
                format!("{}✗ > {path}  {error}{}", fg(RED, caps), fg_reset())
            }
            _ => {
                let offset = args.get("offset").and_then(Value::as_u64);
                let limit = args.get("limit").and_then(Value::as_u64);
                match (offset, limit) {
                    (Some(offset), Some(limit)) => {
                        let end = offset.saturating_add(limit).saturating_sub(1);
                        format!("> {path}  lines {offset}-{end}  ({duration})")
                    }
                    _ => format!("> {path}  ({duration})"),
                }
            }
        }
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        // The file's contents are not surfaced in the scroll region —
        // the header (path + line range) is the whole render. Hiding
        // the body keeps reads from flooding the user's view; the
        // content remains available to the model via the tool result.
        None
    }

    fn body_blocks<'a>(
        &self,
        _args: &'a Value,
        _result: &'a Value,
        _caps: &TerminalCaps,
    ) -> Option<Vec<ContentBlock<'a>>> {
        None
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "path") {
            Some(path) => format!("> {path}  {SPINNER}"),
            None => format!("> {SPINNER}"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;

    fn caps() -> TerminalCaps {
        TerminalCaps::baseline()
    }

    #[test]
    fn read_header_with_range() {
        let header = ReadRenderer.header_line(
            &json!({ "path": "src/a.rs", "offset": 10, "limit": 50 }),
            &json!({ "path": "src/a.rs", "kind": "text", "content": "" }),
            12,
            &caps(),
        );
        assert!(header.contains("> src/a.rs"));
        assert!(header.contains("lines 10-59"), "got: {header:?}");
    }

    #[test]
    fn read_header_without_range_omits_lines() {
        let header = ReadRenderer.header_line(
            &json!({ "path": "src/a.rs" }),
            &json!({ "path": "src/a.rs", "kind": "text", "content": "" }),
            12,
            &caps(),
        );
        assert!(header.contains("> src/a.rs"));
        assert!(!header.contains("lines"), "got: {header:?}");
    }

    #[test]
    fn read_body_is_hidden_for_all_kinds() {
        // The file's contents are not surfaced in the scroll region —
        // the header is the whole render. body() returns None
        // regardless of kind so the body never floods the view.
        for result in [
            json!({ "path": "src/a.rs", "kind": "text", "content": "1\tfn main() {}\n" }),
            json!({ "path": "img.png", "kind": "image" }),
            json!({ "path": "data.bin", "kind": "binary", "size_bytes": 4096 }),
            json!({ "path": "empty.rs", "kind": "text", "content": "" }),
        ] {
            assert!(
                ReadRenderer
                    .body(&json!({ "path": "x" }), &result, &caps())
                    .is_none(),
                "body must be None: {result:?}",
            );
            assert!(
                ReadRenderer
                    .body_blocks(&json!({ "path": "x" }), &result, &caps())
                    .is_none(),
                "body_blocks must be None: {result:?}",
            );
        }
    }

    #[test]
    fn read_binary_and_io_error_headers() {
        let binary = ReadRenderer.header_line(
            &json!({ "path": "data.bin" }),
            &json!({ "path": "data.bin", "kind": "binary", "size_bytes": 4096 }),
            10,
            &caps(),
        );
        assert!(binary.contains("[binary, 4096 bytes]"));

        // Typed payload form — what the read tool emits.
        let io_err = ReadRenderer.header_line(
            &json!({ "path": "gone.rs" }),
            &json!({
                "path": "gone.rs",
                "kind": "io_error",
                "error": { "kind": "io", "message": "No such file" },
            }),
            10,
            &caps(),
        );
        assert!(io_err.contains("No such file"), "{io_err}");

        // Legacy bare-string form still renders.
        let io_err_legacy = ReadRenderer.header_line(
            &json!({ "path": "gone.rs" }),
            &json!({ "path": "gone.rs", "kind": "io_error", "error": "No such file" }),
            10,
            &caps(),
        );
        assert!(io_err_legacy.contains("No such file"), "{io_err_legacy}");
    }
}
