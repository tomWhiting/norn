//! Tier 2 (compact) tool renderers.
//!
//! Compact renderers serve tools whose output collapses cleanly into a
//! single header line: [`WriteRenderer`], [`WebSearchRenderer`],
//! [`WebFetchRenderer`], [`LspRenderer`], [`TaskRenderer`],
//! [`SkillRenderer`], and [`ToolSearchRenderer`].
//! Expanded bodies are used only when the result shape carries useful
//! structured detail that would otherwise disappear from the compact header.
//!
//! The JSON shapes consumed here are produced by the matching tools in
//! `crates/norn/src/tools/`. Field access is uniformly defensive — a
//! missing or mistyped field degrades gracefully rather than panicking,
//! keeping every renderer total.

use std::fmt::Write as _;

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

fn count_label(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

fn result_action(args: &Value, result: &Value) -> String {
    string_field(args, result, "action")
}

fn task_summary(task: &Value) -> String {
    let description = task
        .get("description")
        .and_then(Value::as_str)
        .map_or_else(|| "(no description)".to_string(), truncate_preview);
    let status = task
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let id = task.get("id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() {
        format!("{description}  ({status})")
    } else {
        format!("{description}  [{id}]  ({status})")
    }
}

fn format_location(location: &Value) -> String {
    let path = location
        .get("path")
        .or_else(|| location.get("uri"))
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let line = location.get("line").and_then(Value::as_u64);
    let column = location.get("column").and_then(Value::as_u64);
    match (line, column) {
        (Some(line), Some(column)) => format!("{path}:{line}:{column}"),
        (Some(line), None) => format!("{path}:{line}"),
        _ => path.to_string(),
    }
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

    fn body(&self, _args: &Value, result: &Value, _caps: &TerminalCaps) -> Option<String> {
        let mut out = String::new();
        if let Some(results) = result.get("results").and_then(Value::as_array) {
            for item in results {
                let title = item
                    .get("title")
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let url = item
                    .get("url")
                    .or_else(|| item.get("link"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let snippet = item
                    .get("snippet")
                    .or_else(|| item.get("description"))
                    .and_then(Value::as_str)
                    .map(truncate_preview)
                    .unwrap_or_default();

                if title.is_empty() {
                    let _ = writeln!(out, "{url}");
                } else if url.is_empty() {
                    let _ = writeln!(out, "{title}");
                } else {
                    let _ = writeln!(out, "{title}  {url}");
                }
                if !snippet.is_empty() {
                    let _ = writeln!(out, "  {snippet}");
                }
            }
        }
        (!out.is_empty()).then_some(out)
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

/// Renders `web_fetch` tool calls from the extracted-page summary.
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
        let line_count = result.get("line_count").and_then(Value::as_u64);
        let answers = array_len(result, "answers");
        let truncated = result
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut parts = vec![format!("fetch: {url}")];
        if let Some(lines) = line_count {
            parts.push(format!("{lines} lines"));
        }
        if answers > 0 {
            parts.push(count_label(answers, "answer", "answers"));
        }
        if truncated {
            parts.push("truncated".to_string());
        }
        let content_type = result
            .get("content_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !content_type.is_empty() {
            parts.push(content_type.to_string());
        }
        parts.join("  ")
    }

    fn body(&self, _args: &Value, result: &Value, _caps: &TerminalCaps) -> Option<String> {
        let mut out = String::new();
        if let Some(saved) = result.get("saved_to").and_then(Value::as_str) {
            let _ = writeln!(out, "saved: {saved}");
        }
        if let Some(note) = result.get("truncation_note").and_then(Value::as_str) {
            let _ = writeln!(out, "note: {note}");
        }
        if let Some(answers) = result.get("answers").and_then(Value::as_array) {
            for answer in answers {
                let question = answer
                    .get("question")
                    .map_or_else(String::new, Value::to_string);
                let lines = answer.get("lines").and_then(Value::as_str).unwrap_or("");
                let text = answer.get("answer").and_then(Value::as_str).unwrap_or("");
                if lines.is_empty() {
                    let _ = writeln!(out, "answer {question}: {text}");
                } else {
                    let _ = writeln!(out, "answer {question} [{lines}]: {text}");
                }
            }
        }
        (!out.is_empty()).then_some(out)
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

/// Renders `lsp` tool calls with action-specific result counts.
pub struct LspRenderer;

impl ToolRenderer for LspRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let action = result_action(args, result);
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        let mut parts = vec![format!("lsp: {action}")];
        if !path.is_empty() {
            parts.push(path.to_string());
        }
        match action.as_str() {
            "definition" | "references" => {
                parts.push(count_label(
                    array_len(result, "locations"),
                    "location",
                    "locations",
                ));
            }
            "symbols" => {
                parts.push(count_label(
                    array_len(result, "symbols"),
                    "symbol",
                    "symbols",
                ));
            }
            "diagnostics" => {
                parts.push(count_label(
                    array_len(result, "diagnostics"),
                    "diagnostic",
                    "diagnostics",
                ));
            }
            "hover" if result.get("hover").is_some() => parts.push("hover".to_string()),
            _ => {}
        }
        parts.join("  ")
    }

    fn body(&self, _args: &Value, result: &Value, _caps: &TerminalCaps) -> Option<String> {
        let action = result.get("action").and_then(Value::as_str).unwrap_or("");
        let mut out = String::new();
        match action {
            "hover" => {
                if let Some(content) = result
                    .get("hover")
                    .and_then(|hover| hover.get("content"))
                    .and_then(Value::as_str)
                {
                    let _ = writeln!(out, "{content}");
                }
            }
            "definition" | "references" => {
                if let Some(locations) = result.get("locations").and_then(Value::as_array) {
                    for location in locations {
                        let _ = writeln!(out, "{}", format_location(location));
                    }
                }
            }
            "symbols" => {
                if let Some(symbols) = result.get("symbols").and_then(Value::as_array) {
                    for symbol in symbols {
                        let name = symbol.get("name").and_then(Value::as_str).unwrap_or("");
                        let kind = symbol.get("kind").and_then(Value::as_str).unwrap_or("");
                        let location = format_location(symbol);
                        let _ = writeln!(out, "{name}  {kind}  {location}");
                    }
                }
            }
            "diagnostics" => {
                if let Some(diagnostics) = result.get("diagnostics").and_then(Value::as_array) {
                    for diagnostic in diagnostics {
                        let severity = diagnostic
                            .get("severity")
                            .and_then(Value::as_str)
                            .unwrap_or("info");
                        let message = diagnostic
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let location = format_location(diagnostic);
                        let _ = writeln!(out, "{severity}: {location}: {message}");
                    }
                }
            }
            _ => {}
        }
        (!out.is_empty()).then_some(out)
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
        let action = result_action(args, result);
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
            _ if result.get("tasks").and_then(Value::as_array).is_some() => {
                format!(
                    "task: {action}  {}",
                    count_label(array_len(result, "tasks"), "task", "tasks")
                )
            }
            _ if result.get("groups").and_then(Value::as_array).is_some() => {
                format!(
                    "task: {action}  {}",
                    count_label(array_len(result, "groups"), "group", "groups")
                )
            }
            _ => {
                let group = result
                    .get("group_slug")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if group.is_empty() {
                    format!("task: {action}")
                } else {
                    format!("task: {action}  {group}")
                }
            }
        }
    }

    fn body(&self, _args: &Value, result: &Value, _caps: &TerminalCaps) -> Option<String> {
        let mut out = String::new();
        if let Some(tasks) = result.get("tasks").and_then(Value::as_array) {
            for task in tasks {
                let _ = writeln!(out, "{}", task_summary(task));
            }
        }
        if let Some(groups) = result.get("groups").and_then(Value::as_array) {
            for group in groups {
                if let Some(group) = group.as_str() {
                    let _ = writeln!(out, "{group}");
                }
            }
        }
        (!out.is_empty()).then_some(out)
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

    fn body(&self, _args: &Value, result: &Value, _caps: &TerminalCaps) -> Option<String> {
        let mut out = String::new();
        if let Some(path) = result.get("path").and_then(Value::as_str) {
            let _ = writeln!(out, "path: {path}");
        }
        if let Some(skill_dir) = result.get("skill_dir").and_then(Value::as_str) {
            let _ = writeln!(out, "dir: {skill_dir}");
        }
        if let Some(resources) = result.get("resources").and_then(Value::as_array) {
            let _ = writeln!(out, "resources: {}", resources.len());
            for resource in resources {
                if let Some(resource) = resource.as_str() {
                    let _ = writeln!(out, "  {resource}");
                }
            }
        }
        (!out.is_empty()).then_some(out)
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

    fn body(&self, _args: &Value, result: &Value, _caps: &TerminalCaps) -> Option<String> {
        let mut out = String::new();
        if let Some(results) = result.get("results").and_then(Value::as_array) {
            for item in results {
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let command = item
                    .get("command_value")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let description = item
                    .get("description")
                    .and_then(Value::as_str)
                    .map(truncate_preview)
                    .unwrap_or_default();
                if command.is_empty() {
                    let _ = writeln!(out, "{name}: {description}");
                } else {
                    let _ = writeln!(out, "{name} ({command}): {description}");
                }
                if let Some(fields) = item.get("fields").and_then(Value::as_array)
                    && !fields.is_empty()
                {
                    let field_list = fields
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    if !field_list.is_empty() {
                        let _ = writeln!(out, "  fields: {field_list}");
                    }
                }
            }
        }
        (!out.is_empty()).then_some(out)
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
    fn web_fetch_header_uses_current_result_shape() {
        let header = WebFetchRenderer.header_line(
            &json!({ "url": "https://example.com" }),
            &json!({
                "url": "https://example.com",
                "line_count": 42,
                "truncated": false,
                "content_type": "text/html",
                "answers": [{ "question": 1, "answer": "Example", "lines": "1-2" }],
                "saved_to": ".norn/fetched/example.md",
            }),
            0,
            &caps(),
        );
        assert!(header.contains("fetch: https://example.com"));
        assert!(header.contains("42 lines"));
        assert!(header.contains("1 answer"));
        assert!(header.contains("text/html"));
        assert!(!header.contains("bytes"));
    }

    #[test]
    fn web_fetch_body_shows_saved_path_and_answers() {
        let body = WebFetchRenderer
            .body(
                &json!({}),
                &json!({
                    "saved_to": ".norn/fetched/example.md",
                    "answers": [{ "question": 1, "answer": "Example Domain", "lines": "1-2" }],
                }),
                &caps(),
            )
            .unwrap();
        assert!(body.contains("saved: .norn/fetched/example.md"));
        assert!(body.contains("answer 1 [1-2]: Example Domain"));
    }

    // --- LSP ---------------------------------------------------------

    #[test]
    fn lsp_header_includes_action_and_path() {
        let header = LspRenderer.header_line(
            &json!({ "action": "definition", "path": "src/a.rs", "line": 10, "column": 4 }),
            &json!({ "action": "definition", "locations": [{ "path": "src/a.rs", "line": 3 }] }),
            0,
            &caps(),
        );
        assert!(header.contains("lsp: definition"));
        assert!(header.contains("src/a.rs"));
        assert!(header.contains("1 location"));
    }

    #[test]
    fn lsp_body_shows_hover_and_locations() {
        let hover = LspRenderer
            .body(
                &json!({}),
                &json!({ "action": "hover", "hover": { "content": "fn answer() -> u32" } }),
                &caps(),
            )
            .unwrap();
        assert!(hover.contains("fn answer() -> u32"));

        let locations = LspRenderer
            .body(
                &json!({}),
                &json!({
                    "action": "references",
                    "locations": [{ "path": "src/a.rs", "line": 10, "column": 4 }]
                }),
                &caps(),
            )
            .unwrap();
        assert!(locations.contains("src/a.rs:10:4"));
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
        assert_eq!(header, "task: list  0 tasks");
    }

    #[test]
    fn task_body_lists_tasks_and_groups() {
        let tasks = TaskRenderer
            .body(
                &json!({}),
                &json!({
                    "action": "children",
                    "tasks": [
                        { "id": "t1", "description": "wire the loop", "status": "pending" }
                    ]
                }),
                &caps(),
            )
            .unwrap();
        assert!(tasks.contains("wire the loop"));
        assert!(tasks.contains("[t1]"));

        let groups = TaskRenderer
            .body(
                &json!({}),
                &json!({ "action": "list_groups", "groups": ["norn-agents"] }),
                &caps(),
            )
            .unwrap();
        assert!(groups.contains("norn-agents"));
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

    #[test]
    fn skill_body_lists_path_and_resources() {
        let body = SkillRenderer
            .body(
                &json!({}),
                &json!({
                    "path": "/skills/messaging/SKILL.md",
                    "skill_dir": "/skills/messaging",
                    "resources": ["examples/a.md"]
                }),
                &caps(),
            )
            .unwrap();
        assert!(body.contains("path: /skills/messaging/SKILL.md"));
        assert!(body.contains("resources: 1"));
        assert!(body.contains("examples/a.md"));
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
    fn tool_search_body_lists_matches() {
        let body = ToolSearchRenderer
            .body(
                &json!({}),
                &json!({
                    "results": [
                        {
                            "name": "slack_send",
                            "command_value": "send",
                            "description": "Send a Slack message",
                            "fields": ["channel", "text"]
                        }
                    ]
                }),
                &caps(),
            )
            .unwrap();
        assert!(body.contains("slack_send (send)"));
        assert!(body.contains("fields: channel, text"));
    }
}
