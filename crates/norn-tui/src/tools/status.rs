//! Renderers for status and introspection tools.

use std::fmt::Write as _;

use serde_json::Value;

use super::helpers::{SPINNER, partial_field, truncate_preview};
use super::renderer::ToolRenderer;
use crate::terminal::caps::TerminalCaps;

fn action(args: &Value, result: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .or_else(|| result.get(key).and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

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

fn short_id(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|s| s.chars().take(8).collect())
        .unwrap_or_default()
}

fn write_agent_row(out: &mut String, agent: &Value) {
    let path = agent.get("path").and_then(Value::as_str).unwrap_or("");
    let role = agent.get("role").and_then(Value::as_str).unwrap_or("");
    let model = agent.get("model").and_then(Value::as_str).unwrap_or("");
    let status = agent
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let id = short_id(agent, "id");
    let label = if path.is_empty() { role } else { path };
    let _ = writeln!(out, "{label}  {status}  {model}  {id}");
}

/// Renderer for the `agents` inspection tool.
pub struct AgentsRenderer;

impl ToolRenderer for AgentsRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let act = action(args, result, "action");
        match act.as_str() {
            "list" => format!(
                "agents: list  {}",
                count_label(array_len(result, "agents"), "agent", "agents")
            ),
            "messages" => format!(
                "agents: messages  {}",
                count_label(array_len(result, "edges"), "edge", "edges")
            ),
            "get" if result.get("agent").is_some() => "agents: get  found".to_string(),
            _ => format!("agents: {act}"),
        }
    }

    fn body(&self, _args: &Value, result: &Value, _caps: &TerminalCaps) -> Option<String> {
        let mut out = String::new();
        if let Some(agent) = result.get("agent") {
            write_agent_row(&mut out, agent);
        }
        if let Some(agents) = result.get("agents").and_then(Value::as_array) {
            for agent in agents {
                write_agent_row(&mut out, agent);
            }
        }
        if let Some(edges) = result.get("edges").and_then(Value::as_array) {
            for edge in edges {
                let from = edge.get("from").and_then(Value::as_str).unwrap_or("");
                let to = edge.get("to").and_then(Value::as_str).unwrap_or("");
                let sent = edge.get("sent").and_then(Value::as_u64).unwrap_or(0);
                let delivered = edge.get("delivered").and_then(Value::as_u64).unwrap_or(0);
                let _ = writeln!(out, "{from} -> {to}  sent:{sent} delivered:{delivered}");
            }
        }
        (!out.is_empty()).then_some(out)
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "action") {
            Some(act) => format!("agents: {act}  {SPINNER}"),
            None => format!("agents: {SPINNER}"),
        }
    }
}

/// Renderer for `action_log` queries.
pub struct ActionLogRenderer;

impl ToolRenderer for ActionLogRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let query = action(args, result, "query");
        let count = result
            .get("count")
            .and_then(Value::as_u64)
            .map_or_else(String::new, |n| format!("  {n}"));
        format!("action_log: {query}{count}")
    }

    fn body(&self, _args: &Value, result: &Value, _caps: &TerminalCaps) -> Option<String> {
        let mut out = String::new();
        write_action_entries(&mut out, result);
        write_action_events(&mut out, result);
        write_action_followups(&mut out, result);
        write_action_mutations(&mut out, result);
        write_action_detail(&mut out, result);
        (!out.is_empty()).then_some(out)
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "query") {
            Some(query) => format!("action_log: {query}  {SPINNER}"),
            None => format!("action_log: {SPINNER}"),
        }
    }
}

fn write_action_entries(out: &mut String, result: &Value) {
    if let Some(entries) = result.get("entries").and_then(Value::as_array) {
        for entry in entries {
            let tool = entry
                .get("tool")
                .or_else(|| entry.get("tool_name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let outcome = entry.get("outcome").and_then(Value::as_str).unwrap_or("");
            let summary = entry
                .get("summary")
                .or_else(|| entry.get("summary_line"))
                .and_then(Value::as_str)
                .map(truncate_preview)
                .unwrap_or_default();
            let _ = writeln!(out, "{tool}  {outcome}  {summary}");
        }
    }
}

fn write_action_events(out: &mut String, result: &Value) {
    if let Some(events) = result.get("events").and_then(Value::as_array) {
        for event in events {
            let ty = event.get("type").and_then(Value::as_str).unwrap_or("");
            let agent = event.get("agent").and_then(Value::as_str).unwrap_or("");
            if agent.is_empty() {
                let _ = writeln!(out, "{ty}");
            } else {
                let _ = writeln!(out, "{ty}  {agent}");
            }
        }
    }
}

fn write_action_followups(out: &mut String, result: &Value) {
    if let Some(actions) = result.get("actions").and_then(Value::as_array) {
        for action in actions {
            let tool = action.get("tool").and_then(Value::as_str).unwrap_or("");
            let name = action.get("action").and_then(Value::as_str).unwrap_or("");
            let description = action
                .get("description")
                .and_then(Value::as_str)
                .map(truncate_preview)
                .unwrap_or_default();
            let _ = writeln!(out, "{tool}: {name}  {description}");
        }
    }
}

fn write_action_mutations(out: &mut String, result: &Value) {
    if let Some(entries) = result.get("entries").and_then(Value::as_array) {
        for entry in entries {
            if let Some(file) = entry.get("file_path").and_then(Value::as_str) {
                let operation = entry
                    .get("operation")
                    .and_then(Value::as_str)
                    .unwrap_or("modified");
                let _ = writeln!(out, "{operation}: {file}");
            }
        }
    }
}

fn write_action_detail(out: &mut String, result: &Value) {
    if let Some(entry) = result.get("entry") {
        let tool = entry
            .get("tool_name")
            .or_else(|| entry.get("tool"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let summary = entry
            .get("summary_line")
            .or_else(|| entry.get("summary"))
            .and_then(Value::as_str)
            .map(truncate_preview)
            .unwrap_or_default();
        if !tool.is_empty() || !summary.is_empty() {
            let _ = writeln!(out, "{tool}: {summary}");
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
    fn agents_renderer_lists_agents() {
        let result = json!({
            "action": "list",
            "agents": [
                {
                    "id": "75395432-d9fd-4216-8c84-42d5cbdb26c6",
                    "path": "/tool-smoke/spawn-child",
                    "role": "smoke-child",
                    "model": "gpt-5.5",
                    "status": "active"
                }
            ]
        });
        let header = AgentsRenderer.header_line(&json!({"action": "list"}), &result, 0, &caps());
        let body = AgentsRenderer.body(&json!({}), &result, &caps()).unwrap();
        assert_eq!(header, "agents: list  1 agent");
        assert!(body.contains("/tool-smoke/spawn-child"));
        assert!(body.contains("75395432"));
    }

    #[test]
    fn action_log_renderer_lists_entries() {
        let result = json!({
            "query": "list",
            "count": 1,
            "entries": [
                {
                    "tool": "edit",
                    "outcome": "success",
                    "summary": "edit committed"
                }
            ]
        });
        let header = ActionLogRenderer.header_line(&json!({"query": "list"}), &result, 0, &caps());
        let body = ActionLogRenderer
            .body(&json!({}), &result, &caps())
            .unwrap();
        assert_eq!(header, "action_log: list  1");
        assert!(body.contains("edit"));
        assert!(body.contains("edit committed"));
    }
}
