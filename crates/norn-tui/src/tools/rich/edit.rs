//! `edit` tool renderer.
//!
//! Renders `edit` tool calls across all three outcomes: committed
//! success (unified diff + blast radius), AST-blocked rollback
//! (diagnostics, no diff), and `AllowBrokenAst` override (diff +
//! diagnostics with amber attribution). The JSON shapes consumed here
//! are produced by `crates/norn/src/tools/edit.rs`; field access is
//! defensive so a missing or mistyped field degrades gracefully rather
//! than panicking.

use std::borrow::Cow;
use std::fmt::Write as _;

use serde_json::Value;

use crate::render::content::ContentBlock;
use crate::terminal::caps::TerminalCaps;
use crate::tools::helpers::{
    AMBER, GREEN, RED, SPINNER, colourise_unified_diff, dim, fg, fg_reset, format_duration_ms,
    has_overrides, override_source, partial_field, render_diagnostics, reset, string_field,
};
use crate::tools::renderer::ToolRenderer;

/// Renders `edit` tool calls across all three outcomes: committed
/// success (unified diff + blast radius), AST-blocked rollback
/// (diagnostics, no diff), and `AllowBrokenAst` override (diff +
/// diagnostics with amber attribution).
pub struct EditRenderer;

/// Builds the colourised unified-diff body for an edit from its
/// `old_string`/`new_string` arguments.
fn edit_diff(args: &Value, caps: &TerminalCaps) -> String {
    let old = args.get("old_string").and_then(Value::as_str).unwrap_or("");
    let new = args.get("new_string").and_then(Value::as_str).unwrap_or("");
    let patch = diffy::create_patch(old, new).to_string();
    colourise_unified_diff(&patch, caps)
}

fn diff_stats(result: &Value, caps: &TerminalCaps) -> Option<String> {
    let blast_radius = result.get("blast_radius")?;
    let lines_added = blast_radius
        .get("lines_added")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let lines_removed = blast_radius
        .get("lines_removed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if lines_added == 0 && lines_removed == 0 {
        return None;
    }
    Some(format!(
        "  {}+{lines_added}{} {}-{lines_removed}{} lines",
        fg(GREEN, caps),
        fg_reset(),
        fg(RED, caps),
        fg_reset(),
    ))
}

/// First `blast_radius.containing_symbols` entry, pre-formatted by the
/// upstream tool (for example, `fn target_function`).
fn first_containing_symbol(result: &Value) -> Option<&str> {
    result
        .get("blast_radius")
        .and_then(|b| b.get("containing_symbols"))
        .and_then(Value::as_array)
        .and_then(|symbols| symbols.first())
        .and_then(Value::as_str)
}

impl ToolRenderer for EditRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        duration_ms: u64,
        caps: &TerminalCaps,
    ) -> String {
        let path = string_field(args, result, "path");
        let kind = result.get("kind").and_then(Value::as_str).unwrap_or("");
        match kind {
            "edit_committed" if has_overrides(result) => {
                let stats = diff_stats(result, caps).unwrap_or_default();
                format!(
                    "{}⚠ ~ {path}{stats}  COMMITTED (AST override: {}){}",
                    fg(AMBER, caps),
                    override_source(result),
                    fg_reset(),
                )
            }
            "edit_committed" => {
                let stats = diff_stats(result, caps).unwrap_or_default();
                format!("~ {path}{stats}  ({})", format_duration_ms(duration_ms))
            }
            "edit_blocked_by_ast" => format!(
                "{}✗ ~ {path}  AST BLOCKED (not committed){}",
                fg(RED, caps),
                fg_reset(),
            ),
            "edit_failed" => {
                let message = result
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("old_string not found in file");
                format!(
                    "{}✗ ~ {path}  edit failed: {message}{}",
                    fg(RED, caps),
                    fg_reset(),
                )
            }
            _ => format!("~ {path}"),
        }
    }

    fn body(&self, args: &Value, result: &Value, caps: &TerminalCaps) -> Option<String> {
        match result.get("kind").and_then(Value::as_str).unwrap_or("") {
            "edit_committed" if has_overrides(result) => {
                let mut out = edit_diff(args, caps);
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                let _ = writeln!(out, "{}── diagnostics ──{}", dim(), reset());
                out.push_str(&render_diagnostics(result, caps, true));
                Some(out)
            }
            "edit_committed" => Some(edit_diff(args, caps)),
            "edit_blocked_by_ast" => {
                // The diff is deliberately omitted — showing a diff of
                // changes that were never committed would mislead.
                let mut out = String::new();
                if let Some(message) = result.get("message").and_then(Value::as_str) {
                    let _ = writeln!(out, "{message}");
                }
                out.push_str(&render_diagnostics(result, caps, false));
                if out.is_empty() { None } else { Some(out) }
            }
            _ => None,
        }
    }

    fn body_blocks<'a>(
        &self,
        args: &'a Value,
        result: &'a Value,
        _caps: &TerminalCaps,
    ) -> Option<Vec<ContentBlock<'a>>> {
        let kind = result.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind != "edit_committed" {
            return None;
        }
        let path = result
            .get("path")
            .or_else(|| args.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let old = args.get("old_string").and_then(Value::as_str).unwrap_or("");
        let new = args.get("new_string").and_then(Value::as_str).unwrap_or("");
        if old.is_empty() && new.is_empty() {
            return None;
        }
        let mut blocks = Vec::with_capacity(2);
        if let Some(symbol) = first_containing_symbol(result) {
            blocks.push(ContentBlock::Plain {
                text: Cow::Owned(format!("{}@@ {symbol} @@\x1b[22m\n", dim())),
            });
        }
        blocks.push(ContentBlock::Diff {
            path,
            removed: old,
            added: new,
        });
        Some(blocks)
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "path") {
            Some(path) => format!("~ {path}  {SPINNER}"),
            None => format!("~ {SPINNER}"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::render::content::render_blocks;
    use crate::render::syntax::SyntaxHighlighter;

    fn caps() -> TerminalCaps {
        TerminalCaps::baseline()
    }

    #[test]
    fn edit_success_header_has_path() {
        let header = EditRenderer.header_line(
            &json!({ "path": "src/a.rs", "old_string": "a", "new_string": "b" }),
            &json!({ "path": "src/a.rs", "kind": "edit_committed", "check_overrides": [] }),
            120,
            &caps(),
        );
        assert!(header.contains("~ src/a.rs"));
    }

    #[test]
    fn edit_committed_header_with_stats() {
        let caps = caps();
        let header = EditRenderer.header_line(
            &json!({ "path": "src/a.rs", "old_string": "a", "new_string": "b" }),
            &json!({
                "path": "src/a.rs",
                "kind": "edit_committed",
                "check_overrides": [],
                "blast_radius": { "lines_added": 5, "lines_removed": 3 },
            }),
            420,
            &caps,
        );
        assert!(header.contains("~ src/a.rs"));
        assert!(header.contains(&format!("{}+5{}", fg(GREEN, &caps), fg_reset())));
        assert!(header.contains(&format!("{}-3{}", fg(RED, &caps), fg_reset())));
        assert!(header.contains(" lines "));
        assert!(header.contains("(0.42s)"));
    }

    #[test]
    fn edit_committed_header_without_stats_when_zero() {
        let header = EditRenderer.header_line(
            &json!({ "path": "src/a.rs", "old_string": "a", "new_string": "b" }),
            &json!({
                "path": "src/a.rs",
                "kind": "edit_committed",
                "check_overrides": [],
                "blast_radius": { "lines_added": 0, "lines_removed": 0 },
            }),
            420,
            &caps(),
        );
        assert_eq!(header, "~ src/a.rs  (0.42s)");
        assert!(!header.contains("+0"));
        assert!(!header.contains("-0"));
        assert!(!header.contains("lines"));
    }

    #[test]
    fn edit_success_body_has_diff() {
        let body = EditRenderer
            .body(
                &json!({
                    "path": "src/a.rs",
                    "old_string": "fn a() {}\n",
                    "new_string": "fn b() {}\n",
                }),
                &json!({
                    "path": "src/a.rs",
                    "kind": "edit_committed",
                    "check_overrides": [],
                    "blast_radius": { "containing_symbols": ["fn a"] },
                }),
                &caps(),
            )
            .unwrap();
        assert!(!body.contains("Containing symbols:"));
        assert!(body.lines().any(|l| l.contains("-fn a")));
        assert!(body.lines().any(|l| l.contains("+fn b")));
    }

    #[test]
    fn edit_committed_body_has_hunk_header_when_symbol_present() {
        let args = json!({
            "path": "src/a.rs",
            "old_string": "fn old_name() {}\n",
            "new_string": "fn target_function() {}\n",
        });
        let result = json!({
            "path": "src/a.rs",
            "kind": "edit_committed",
            "check_overrides": [],
            "blast_radius": { "containing_symbols": ["fn target_function"] },
        });
        let blocks = EditRenderer.body_blocks(&args, &result, &caps()).unwrap();
        let rendered = render_blocks(&blocks, &SyntaxHighlighter::new(), &caps());
        let hunk_index = rendered
            .find("\x1b[2m@@ fn target_function @@\x1b[22m")
            .unwrap_or(usize::MAX);
        let removed_index = rendered.find("- ").unwrap_or(0);
        assert_ne!(
            hunk_index,
            usize::MAX,
            "missing dim hunk header: {rendered:?}"
        );
        assert_ne!(removed_index, 0, "missing removed diff line: {rendered:?}");
        assert!(
            hunk_index < removed_index,
            "hunk header must precede diff: {rendered:?}"
        );
    }

    #[test]
    fn edit_committed_body_no_hunk_header_when_symbols_absent() {
        let args = json!({
            "path": "src/a.rs",
            "old_string": "fn a() {}\n",
            "new_string": "fn b() {}\n",
        });
        for result in [
            json!({
                "path": "src/a.rs",
                "kind": "edit_committed",
                "check_overrides": [],
            }),
            json!({
                "path": "src/a.rs",
                "kind": "edit_committed",
                "check_overrides": [],
                "blast_radius": { "containing_symbols": [] },
            }),
        ] {
            let blocks = EditRenderer.body_blocks(&args, &result, &caps()).unwrap();
            let rendered = render_blocks(&blocks, &SyntaxHighlighter::new(), &caps());
            assert!(!rendered.contains("@@"));
        }
    }

    #[test]
    fn edit_blocked_header_and_body() {
        let result = json!({
            "path": "src/a.rs",
            "kind": "edit_blocked_by_ast",
            "committed": false,
            "message": "edit rejected: staged content has syntax errors",
            "diagnostics": [
                { "code": "syntax-missing", "line": 4, "severity": "error", "message": "missing }" }
            ],
        });
        let header = EditRenderer.header_line(
            &json!({ "path": "src/a.rs", "old_string": "x }", "new_string": "x ;" }),
            &result,
            50,
            &caps(),
        );
        assert!(header.contains("AST BLOCKED"));

        let body = EditRenderer
            .body(
                &json!({ "path": "src/a.rs", "old_string": "x }", "new_string": "x ;" }),
                &result,
                &caps(),
            )
            .unwrap();
        assert!(body.contains("missing }"));
        assert!(body.contains("syntax-missing"));
        // The diff must NOT appear — no `+`/`-` line-prefix sequences.
        assert!(
            !body.contains("\n+") && !body.contains("\n-"),
            "blocked body must not contain a diff: {body:?}",
        );
    }

    #[test]
    fn edit_override_header_and_body() {
        let result = json!({
            "path": "src/a.rs",
            "kind": "edit_committed",
            "committed": true,
            "check_overrides": [
                { "check_name": "ast_validation", "flag": "AllowBrokenAst", "source": "test:override-broken" }
            ],
            "diagnostics": [
                { "code": "syntax-missing", "line": 1, "severity": "error", "message": "missing }" }
            ],
            "blast_radius": { "containing_symbols": ["fn b"], "lines_added": 1, "lines_removed": 1 },
        });
        let header = EditRenderer.header_line(
            &json!({ "path": "src/a.rs", "old_string": "a}", "new_string": "a;" }),
            &result,
            50,
            &caps(),
        );
        assert!(header.contains("AST override: test:override-broken"));
        assert!(header.contains("+1"));
        assert!(header.contains("-1"));

        let args =
            json!({ "path": "src/a.rs", "old_string": "fn a() {}\n", "new_string": "fn b() {}\n" });
        let blocks = EditRenderer.body_blocks(&args, &result, &caps()).unwrap();
        let rendered_blocks = render_blocks(&blocks, &SyntaxHighlighter::new(), &caps());
        assert!(rendered_blocks.contains("\x1b[2m@@ fn b @@\x1b[22m"));

        let body = EditRenderer
            .body(
                &json!({ "path": "src/a.rs", "old_string": "fn a() {}\n", "new_string": "fn b() {}\n" }),
                &result,
                &caps(),
            )
            .unwrap();
        // Body carries BOTH the diff and the diagnostic.
        assert!(body.lines().any(|l| l.contains("+fn b")));
        assert!(body.lines().any(|l| l.contains("-fn a")));
        assert!(body.contains("missing }"));
    }

    #[test]
    fn edit_failed_header_no_body() {
        let result = json!({
            "path": "src/a.rs",
            "kind": "edit_failed",
            "message": "old_string not found in file",
        });
        let header = EditRenderer.header_line(
            &json!({ "path": "src/a.rs", "old_string": "x", "new_string": "y" }),
            &result,
            10,
            &caps(),
        );
        assert!(header.contains("edit failed"));
        assert!(EditRenderer.body(&json!({}), &result, &caps()).is_none(),);
    }

    #[test]
    fn edit_blocked_empty_diags_returns_none_body() {
        assert!(
            EditRenderer
                .body(
                    &json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }),
                    &json!({ "kind": "edit_blocked_by_ast" }),
                    &caps(),
                )
                .is_none(),
        );
    }
}
