//! `apply_patch` tool renderer.
//!
//! Renders `apply_patch` tool calls: a `patch {path}` header with hunk
//! count and a colourised diff body. Mirrors [`super::edit::EditRenderer`]'s
//! AST-blocked and override treatment. The JSON shapes consumed here are
//! produced by `crates/norn/src/tools/patch.rs`; field access is defensive
//! so a missing or mistyped field degrades gracefully rather than
//! panicking.

use std::fmt::Write as _;

use serde_json::Value;

use crate::terminal::caps::TerminalCaps;
use crate::tools::helpers::{
    AMBER, RED, SPINNER, colourise_unified_diff, dim, fg, fg_reset, format_duration_ms,
    has_overrides, override_source, render_diagnostics, reset,
};
use crate::tools::renderer::ToolRenderer;

/// Renders `apply_patch` tool calls: a `patch {path}` header with hunk
/// count and a colourised diff body. Mirrors [`super::edit::EditRenderer`]'s
/// AST-blocked and override treatment.
pub struct ApplyPatchRenderer;

impl ToolRenderer for ApplyPatchRenderer {
    fn header_line(
        &self,
        _args: &Value,
        result: &Value,
        duration_ms: u64,
        caps: &TerminalCaps,
    ) -> String {
        let kind = result.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind == "patch_blocked_by_ast" {
            // Reuses the Edit rollback header shape.
            return format!(
                "{}✗ patch  AST BLOCKED (not committed){}",
                fg(RED, caps),
                fg_reset(),
            );
        }
        let files = result.get("files_modified").and_then(Value::as_array);
        let first = files
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .unwrap_or("");
        let count = files.map_or(0, Vec::len);
        let total_hunks = result
            .get("hunks_applied")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let path_part = if count > 1 {
            format!("{first} (+{} more)", count - 1)
        } else {
            first.to_string()
        };
        let header = format!(
            "patch {path_part}  {total_hunks} hunks  ({})",
            format_duration_ms(duration_ms)
        );
        if has_overrides(result) {
            // Parallels EditRenderer's override treatment — a committed
            // patch carrying an AllowBrokenAst override gets the same
            // amber attribution.
            format!(
                "{}⚠ {header}  (AST override: {}){}",
                fg(AMBER, caps),
                override_source(result),
                fg_reset(),
            )
        } else {
            header
        }
    }

    fn body(&self, args: &Value, result: &Value, caps: &TerminalCaps) -> Option<String> {
        if result.get("kind").and_then(Value::as_str) == Some("patch_blocked_by_ast") {
            let mut out = String::new();
            if let Some(message) = result.get("message").and_then(Value::as_str) {
                let _ = writeln!(out, "{message}");
            }
            out.push_str(&render_diagnostics(result, caps, false));
            return if out.is_empty() { None } else { Some(out) };
        }
        // `args.patch` is the raw unified-diff text the model supplied.
        let patch = args.get("patch").and_then(Value::as_str).unwrap_or("");
        if patch.is_empty() {
            return None;
        }
        let mut out = colourise_unified_diff(patch, caps);
        if has_overrides(result) {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            let _ = writeln!(out, "{}── diagnostics ──{}", dim(), reset());
            out.push_str(&render_diagnostics(result, caps, true));
        }
        Some(out)
    }

    fn streaming_header(&self, _name: &str, _partial_args: &str, _caps: &TerminalCaps) -> String {
        // The patch argument is the full diff text — too large to
        // preview meaningfully, so only the spinner is shown.
        format!("patch  {SPINNER}")
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

    fn true_caps() -> TerminalCaps {
        let mut c = TerminalCaps::baseline();
        c.true_colour = true;
        c
    }

    #[test]
    fn apply_patch_header_has_path_and_hunks() {
        let header = ApplyPatchRenderer.header_line(
            &json!({ "patch": "" }),
            &json!({
                "kind": "patch_committed",
                "files_modified": ["src/a.rs"],
                "hunks_applied": 3,
                "check_overrides": [],
            }),
            200,
            &caps(),
        );
        assert!(header.contains("patch src/a.rs"));
        assert!(header.contains("3 hunks"));
    }

    #[test]
    fn apply_patch_body_colours_added_and_removed_lines() {
        let patch = "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n-old line\n+new line\n";
        let body = ApplyPatchRenderer
            .body(
                &json!({ "patch": patch }),
                &json!({
                    "kind": "patch_committed",
                    "files_modified": ["x.rs"],
                    "hunks_applied": 1,
                    "check_overrides": [],
                }),
                &true_caps(),
            )
            .unwrap();
        assert!(
            body.contains("38;2;80;180;80"),
            "expected green SGR for an added line: {body:?}",
        );
        assert!(
            body.contains("38;2;200;80;80"),
            "expected red SGR for a removed line: {body:?}",
        );
    }

    #[test]
    fn apply_patch_blocked_reuses_rollback_shape() {
        let result = json!({
            "kind": "patch_blocked_by_ast",
            "message": "apply_patch rejected: staged content has syntax errors",
            "diagnostics": [
                { "code": "syntax-error", "line": 2, "severity": "error", "message": "bad token" }
            ],
        });
        let header = ApplyPatchRenderer.header_line(&json!({}), &result, 10, &caps());
        assert!(header.contains("AST BLOCKED"));
        let body = ApplyPatchRenderer
            .body(&json!({}), &result, &caps())
            .unwrap();
        assert!(body.contains("bad token"));
    }

    #[test]
    fn apply_patch_multi_file_header() {
        let header = ApplyPatchRenderer.header_line(
            &json!({ "patch": "" }),
            &json!({
                "kind": "patch_committed",
                "files_modified": ["src/a.rs", "src/b.rs", "src/c.rs"],
                "hunks_applied": 5,
                "check_overrides": [],
            }),
            100,
            &caps(),
        );
        assert!(header.contains("src/a.rs (+2 more)"), "got: {header:?}");
        assert!(header.contains("5 hunks"));
    }
}
