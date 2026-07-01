//! `search` tool renderer.
//!
//! Renders `search` tool calls: a `? {query}` header with a result
//! count, and a body grouping `file:line:content` hits by path with the
//! matched term highlighted. The JSON shapes consumed here are produced
//! by `crates/norn/src/tools/search/`; field access is defensive so a
//! missing or mistyped field degrades gracefully rather than panicking.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use regex::Regex;
use serde_json::Value;
use termina::escape::csi::{Csi, Sgr};

use crate::terminal::caps::TerminalCaps;
use crate::tools::helpers::{bold, dim, format_duration_ms, partial_field, reset, SPINNER};
use crate::tools::renderer::ToolRenderer;

/// Renders `search` tool calls: a `? {query}` header with a result
/// count, and a body grouping `file:line:content` hits by path with the
/// matched term highlighted.
pub struct SearchRenderer;

/// Wraps each regex match in `content` with inverse-video SGR.
///
/// Inverse video is part of baseline ANSI (universal per the TUI's hard
/// requirements), so no capability gate is needed.
fn highlight_matches(content: &str, regex: Option<&Regex>) -> String {
    let Some(re) = regex else {
        return content.to_string();
    };
    let mut out = String::with_capacity(content.len());
    let mut last = 0;
    for m in re.find_iter(content) {
        if m.start() == m.end() {
            continue;
        }
        out.push_str(&content[last..m.start()]);
        let _ = write!(
            out,
            "{}{}{}",
            Csi::Sgr(Sgr::Reverse(true)),
            m.as_str(),
            Csi::Sgr(Sgr::Reverse(false)),
        );
        last = m.end();
    }
    out.push_str(&content[last..]);
    out
}

/// Renders content-mode matches grouped by path, sorted by path.
fn search_content_body(matches: &[Value], args: &Value) -> String {
    let regex = args
        .get("pattern")
        .and_then(Value::as_str)
        .and_then(|p| Regex::new(p).ok());
    let mut groups: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    for m in matches {
        let path = m
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let line = m.get("line").and_then(Value::as_u64).unwrap_or(0);
        let content = m
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        groups.entry(path).or_default().push((line, content));
    }
    let mut out = String::new();
    for (path, hits) in groups {
        let _ = writeln!(out, "{}{path}{}", bold(), reset());
        for (line, content) in hits {
            let _ = writeln!(
                out,
                "  {line}: {}",
                highlight_matches(&content, regex.as_ref()),
            );
        }
    }
    out
}

impl ToolRenderer for SearchRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let (prefix, query) = match args.get("pattern").and_then(Value::as_str) {
            Some(pattern) => ("?", pattern.to_string()),
            None => (
                "??",
                args.get("glob")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
        };
        let count = result
            .get("matches")
            .and_then(Value::as_array)
            .map(Vec::len)
            .or_else(|| result.get("paths").and_then(Value::as_array).map(Vec::len))
            .unwrap_or(0);
        let mut header = format!(
            "{prefix} {query}  {count} results  ({})",
            format_duration_ms(duration_ms)
        );
        if result
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let _ = write!(header, " {}(truncated){}", dim(), reset());
        }
        header
    }

    fn body(&self, args: &Value, result: &Value, _caps: &TerminalCaps) -> Option<String> {
        // Files mode: a flat `paths` array, no line numbers.
        if let Some(paths) = result.get("paths").and_then(Value::as_array) {
            if paths.is_empty() {
                return None;
            }
            let mut out = String::new();
            for p in paths.iter().filter_map(Value::as_str) {
                let _ = writeln!(out, "  • {}{p}{}", bold(), reset());
            }
            return Some(out);
        }
        let matches = result.get("matches").and_then(Value::as_array)?;
        let first = matches.first()?;
        if first.get("content").is_some() {
            Some(search_content_body(matches, args))
        } else if first.get("score").is_some() {
            // Fuzzy mode.
            let mut out = String::new();
            for m in matches {
                let path = m.get("path").and_then(Value::as_str).unwrap_or("");
                let score = m.get("score").and_then(Value::as_u64).unwrap_or(0);
                let _ = writeln!(
                    out,
                    "  {}{path}{}  {}score={score}{}",
                    bold(),
                    reset(),
                    dim(),
                    reset(),
                );
            }
            Some(out)
        } else {
            // AST mode.
            let mut out = String::new();
            for m in matches {
                let path = m.get("path").and_then(Value::as_str).unwrap_or("");
                let line = m.get("line").and_then(Value::as_u64).unwrap_or(0);
                let column = m.get("column").and_then(Value::as_u64).unwrap_or(0);
                let text = m.get("text").and_then(Value::as_str).unwrap_or("");
                let _ = writeln!(
                    out,
                    "  {}{path}{}{}:{line}:{column}{} → {text}",
                    bold(),
                    reset(),
                    dim(),
                    reset(),
                );
            }
            Some(out)
        }
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        let query =
            partial_field(partial_args, "pattern").or_else(|| partial_field(partial_args, "glob"));
        match query {
            Some(query) => format!("? {query}  {SPINNER}"),
            None => format!("? {SPINNER}"),
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
    fn search_header_has_query_and_count() {
        let header = SearchRenderer.header_line(
            &json!({ "pattern": "needle" }),
            &json!({ "matches": [
                { "path": "a.txt", "line": 2, "content": "x" },
                { "path": "b.txt", "line": 1, "content": "y" },
            ], "truncated": false }),
            30,
            &caps(),
        );
        assert!(header.contains("? needle"));
        assert!(header.contains("2 results"));
    }

    #[test]
    fn search_body_groups_by_path() {
        let result = json!({
            "matches": [
                { "path": "a.txt", "line": 2, "content": "alpha needle here" },
                { "path": "a.txt", "line": 9, "content": "needle again" },
                { "path": "b.txt", "line": 1, "content": "needle on b" },
            ],
            "truncated": false,
        });
        let body = SearchRenderer
            .body(&json!({ "pattern": "needle" }), &result, &caps())
            .unwrap();
        // Each path appears exactly once as a group header.
        assert_eq!(body.matches("a.txt").count(), 1);
        assert_eq!(body.matches("b.txt").count(), 1);
        // Each match's line:content appears.
        assert!(body.contains("2: "));
        assert!(body.contains("9: "));
        assert!(body.contains("1: "));
        // The matched term is highlighted with inverse video.
        assert!(
            body.contains("\u{1b}[7m"),
            "expected reverse-video SGR: {body:?}"
        );
    }

    #[test]
    fn search_files_mode_lists_paths() {
        let body = SearchRenderer
            .body(
                &json!({ "glob": "**/*.rs" }),
                &json!({ "paths": ["src/a.rs", "src/b.rs"], "truncated": false }),
                &caps(),
            )
            .unwrap();
        assert!(body.contains("src/a.rs"));
        assert!(body.contains("src/b.rs"));
        assert!(
            body.contains("\u{1b}[1m") && body.contains("\u{1b}[m"),
            "file paths should preserve styled tool output: {body:?}",
        );
    }

    #[test]
    fn search_truncated_indicator_appears() {
        let header = SearchRenderer.header_line(
            &json!({ "pattern": "foo" }),
            &json!({ "matches": [{ "path": "a.txt", "line": 1, "content": "foo" }], "truncated": true }),
            10,
            &caps(),
        );
        assert!(header.contains("truncated"), "got: {header:?}");
    }

    #[test]
    fn search_fuzzy_mode_body() {
        let result = json!({
            "matches": [
                { "path": "src/a.rs", "score": 100 },
                { "path": "src/b.rs", "score": 80 },
            ],
        });
        let body = SearchRenderer
            .body(&json!({ "pattern": "a" }), &result, &caps())
            .unwrap();
        assert!(body.contains("src/a.rs"));
        assert!(body.contains("score=100"));
        assert!(
            body.contains("\u{1b}[1m") && body.contains("\u{1b}[2m"),
            "fuzzy path and score should be styled: {body:?}",
        );
    }

    #[test]
    fn search_ast_mode_body() {
        let result = json!({
            "matches": [
                { "path": "src/a.rs", "line": 10, "column": 5, "text": "fn foo()" },
            ],
        });
        let body = SearchRenderer
            .body(&json!({ "pattern": "foo" }), &result, &caps())
            .unwrap();
        assert!(body.contains("src/a.rs"));
        assert!(body.contains(":10:5"));
        assert!(body.contains("fn foo()"));
        assert!(
            body.contains("\u{1b}[1m") && body.contains("\u{1b}[2m"),
            "AST path and location should be styled: {body:?}",
        );
    }
}
