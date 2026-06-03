//! Tier 2 (compact) tool renderers.
//!
//! Compact renderers serve tools whose output collapses cleanly into a
//! single header line: [`WriteRenderer`], [`WebSearchRenderer`],
//! [`WebFetchRenderer`], [`LspRenderer`], [`TaskRenderer`],
//! [`SkillRenderer`], and [`ToolSearchRenderer`].
//! Every one returns `None` from [`ToolRenderer::body`] — there is no
//! expanded view, the header is the whole story.
//!
//! The JSON shapes consumed here are produced by the matching tools in
//! `crates/norn/src/tools/`. Field access is uniformly defensive — a
//! missing or mistyped field degrades gracefully rather than panicking,
//! keeping every renderer total.

use serde_json::Value;

use super::helpers::{SPINNER, partial_field, string_field, truncate_preview};
use super::renderer::ToolRenderer;
use crate::terminal::caps::TerminalCaps;

/// `ok` when the result carries no error-severity diagnostic, `error`
/// otherwise. Mirrors `WriteTool`'s own `is_error` computation.
fn ast_status(result: &Value) -> &'static str {
    let has_error = result
        .get("diagnostics")
        .and_then(Value::as_array)
        .is_some_and(|diags| {
            diags
                .iter()
                .any(|d| d.get("severity").and_then(Value::as_str) == Some("error"))
        });
    if has_error { "error" } else { "ok" }
}

/// Length of the array at `key` in `result`, or `0` when absent.
fn array_len(result: &Value, key: &str) -> usize {
    result
        .get(key)
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

// ---------------------------------------------------------------------
// Write
// ---------------------------------------------------------------------

/// Renders `write` tool calls: `+ {path}  {N} lines  ast: ok|error`.
pub struct WriteRenderer;

impl ToolRenderer for WriteRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let path = string_field(args, result, "path");
        let line_count = result
            .get("line_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        format!("+ {path}  {line_count} lines  ast: {}", ast_status(result))
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "path") {
            Some(path) => format!("+ {path}  {SPINNER}"),
            None => format!("+ {SPINNER}"),
        }
    }
}

// ---------------------------------------------------------------------
// WebSearch
// ---------------------------------------------------------------------

/// Renders `web_search` tool calls: `web: {query}  {N} results`.
pub struct WebSearchRenderer;

impl ToolRenderer for WebSearchRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let query = string_field(args, result, "query");
        format!("web: {query}  {} results", array_len(result, "results"))
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "query") {
            Some(query) => format!("web: {query}  {SPINNER}"),
            None => format!("web: {SPINNER}"),
        }
    }
}

// ---------------------------------------------------------------------
// WebFetch
// ---------------------------------------------------------------------

/// Renders `web_fetch` tool calls: `fetch: {url}  {N} bytes`.
pub struct WebFetchRenderer;

impl ToolRenderer for WebFetchRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let url = string_field(args, result, "url");
        let bytes = result.get("bytes").and_then(Value::as_u64).unwrap_or(0);
        format!("fetch: {url}  {bytes} bytes")
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "url") {
            Some(url) => format!("fetch: {url}  {SPINNER}"),
            None => format!("fetch: {SPINNER}"),
        }
    }
}

// ---------------------------------------------------------------------
// LSP
// ---------------------------------------------------------------------

/// Renders `lsp` tool calls: `lsp: {action}  {path}`.
pub struct LspRenderer;

impl ToolRenderer for LspRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let action = string_field(args, result, "action");
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            format!("lsp: {action}")
        } else {
            format!("lsp: {action}  {path}")
        }
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "action") {
            Some(action) => format!("lsp: {action}  {SPINNER}"),
            None => format!("lsp: {SPINNER}"),
        }
    }
}

// ---------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------

/// Renders `task` tool calls: `task: {action} "{description}"  ({status})`.
///
/// Note the upstream tool names the field `description`, not `title`.
pub struct TaskRenderer;

impl ToolRenderer for TaskRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let action = string_field(args, result, "action");
        let task = result.get("task");
        let description = task
            .and_then(|t| t.get("description"))
            .and_then(Value::as_str);
        let status = task.and_then(|t| t.get("status")).and_then(Value::as_str);
        match (description, status) {
            (Some(desc), Some(st)) => {
                format!("task: {action} \"{}\"  ({st})", truncate_preview(desc))
            }
            (Some(desc), None) => format!("task: {action} \"{}\"", truncate_preview(desc)),
            _ => format!("task: {action}"),
        }
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "action") {
            Some(action) => format!("task: {action}  {SPINNER}"),
            None => format!("task: {SPINNER}"),
        }
    }
}

// ---------------------------------------------------------------------
// Skill
// ---------------------------------------------------------------------

/// Renders `skill` tool calls: `skill: {name} loaded`.
pub struct SkillRenderer;

impl ToolRenderer for SkillRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let name = string_field(args, result, "name");
        format!("skill: {name} loaded")
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "name") {
            Some(name) => format!("skill: {name}  {SPINNER}"),
            None => format!("skill: {SPINNER}"),
        }
    }
}

// ---------------------------------------------------------------------
// ToolSearch
// ---------------------------------------------------------------------

/// Renders `tool_search` tool calls: `search tools: {query}  {N} matches`.
pub struct ToolSearchRenderer;

impl ToolRenderer for ToolSearchRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let query = string_field(args, result, "query");
        format!(
            "search tools: {query}  {} matches",
            array_len(result, "results")
        )
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "query") {
            Some(query) => format!("search tools: {query}  {SPINNER}"),
            None => format!("search tools: {SPINNER}"),
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

    // --- Write -------------------------------------------------------

    #[test]
    fn write_header_includes_path_and_line_count() {
        let header = WriteRenderer.header_line(
            &json!({ "path": "src/a.rs", "content": "fn main() {}\n" }),
            &json!({
                "path": "src/a.rs",
                "bytes_written": 13,
                "line_count": 1,
                "length_limit": null,
                "diagnostics": [],
                "check_overrides": [],
            }),
            0,
            &caps(),
        );
        assert!(header.contains("+ src/a.rs"));
        assert!(header.contains("1 lines"));
        assert!(header.contains("ast: ok"));
    }

    #[test]
    fn write_header_reports_ast_error() {
        let header = WriteRenderer.header_line(
            &json!({ "path": "src/a.rs", "content": "fn (" }),
            &json!({
                "path": "src/a.rs",
                "line_count": 1,
                "diagnostics": [
                    { "code": "syntax", "line": 1, "severity": "error", "message": "bad" }
                ],
                "check_overrides": [],
            }),
            0,
            &caps(),
        );
        assert!(header.contains("ast: error"), "got: {header:?}");
    }

    #[test]
    fn write_body_is_none() {
        assert!(
            WriteRenderer
                .body(&json!({}), &json!({}), &caps())
                .is_none()
        );
    }

    #[test]
    fn write_streaming_header_shows_spinner() {
        assert_eq!(
            WriteRenderer.streaming_header("write", "{\"path\":\"a.rs\"}", &caps()),
            "+ a.rs  ⟳",
        );
        assert_eq!(
            WriteRenderer.streaming_header("write", "{\"pa", &caps()),
            "+ ⟳",
        );
    }

    // --- WebSearch ---------------------------------------------------

    #[test]
    fn web_search_header_includes_query_and_count() {
        let header = WebSearchRenderer.header_line(
            &json!({ "query": "rust async" }),
            &json!({
                "query": "rust async",
                "results": [{ "title": "a" }, { "title": "b" }],
                "formatted": "...",
            }),
            0,
            &caps(),
        );
        assert!(header.contains("web: rust async"));
        assert!(header.contains("2 results"));
    }

    #[test]
    fn web_search_body_is_none() {
        assert!(
            WebSearchRenderer
                .body(&json!({}), &json!({}), &caps())
                .is_none()
        );
    }

    // --- WebFetch ----------------------------------------------------

    #[test]
    fn web_fetch_header_includes_url_and_length() {
        let header = WebFetchRenderer.header_line(
            &json!({ "url": "https://example.com" }),
            &json!({
                "url": "https://example.com",
                "format": "markdown",
                "bytes": 4096,
                "truncated": false,
                "content_type": "text/html",
                "content": "...",
            }),
            0,
            &caps(),
        );
        assert!(header.contains("fetch: https://example.com"));
        assert!(header.contains("4096 bytes"));
    }

    #[test]
    fn web_fetch_body_is_none() {
        assert!(
            WebFetchRenderer
                .body(&json!({}), &json!({}), &caps())
                .is_none()
        );
    }

    // --- LSP ---------------------------------------------------------

    #[test]
    fn lsp_header_includes_action_and_path() {
        let header = LspRenderer.header_line(
            &json!({ "action": "definition", "path": "src/a.rs", "line": 10, "column": 4 }),
            &json!({ "action": "definition", "locations": [] }),
            0,
            &caps(),
        );
        assert!(header.contains("lsp: definition"));
        assert!(header.contains("src/a.rs"));
    }

    // --- Task --------------------------------------------------------

    #[test]
    fn task_header_includes_action_and_title() {
        let header = TaskRenderer.header_line(
            &json!({ "action": "create" }),
            &json!({
                "action": "create",
                "task": { "id": "t1", "description": "wire the loop", "status": "pending" },
            }),
            0,
            &caps(),
        );
        assert!(header.contains("task: create"));
        assert!(header.contains("wire the loop"));
        assert!(header.contains("(pending)"));
    }

    #[test]
    fn task_header_without_description_is_bare() {
        let header = TaskRenderer.header_line(
            &json!({ "action": "list" }),
            &json!({ "action": "list", "tasks": [] }),
            0,
            &caps(),
        );
        assert_eq!(header, "task: list");
    }

    // --- Skill -------------------------------------------------------

    #[test]
    fn skill_header_names_the_skill() {
        let header = SkillRenderer.header_line(
            &json!({ "name": "messaging" }),
            &json!({ "name": "messaging", "path": "/skills/messaging", "content": "..." }),
            0,
            &caps(),
        );
        assert_eq!(header, "skill: messaging loaded");
    }

    // --- ToolSearch --------------------------------------------------

    #[test]
    fn tool_search_header_includes_query_and_match_count() {
        let header = ToolSearchRenderer.header_line(
            &json!({ "query": "slack" }),
            &json!({ "results": [{ "name": "a", "description": "", "score": 1 }] }),
            0,
            &caps(),
        );
        assert!(header.contains("search tools: slack"));
        assert!(header.contains("1 matches"));
    }

    #[test]
    fn all_compact_renderers_return_none_body() {
        let empty = json!({});
        assert!(LspRenderer.body(&empty, &empty, &caps()).is_none());
        assert!(TaskRenderer.body(&empty, &empty, &caps()).is_none());
        assert!(SkillRenderer.body(&empty, &empty, &caps()).is_none());
        assert!(ToolSearchRenderer.body(&empty, &empty, &caps()).is_none());
    }
}
