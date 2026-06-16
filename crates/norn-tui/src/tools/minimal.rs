//! Tier 3 (minimal) tool renderers.
//!
//! Minimal renderers serve agent-coordination tools that produce little
//! or no visible output: [`SpawnAgentRenderer`],
//! [`ForkRenderer`], [`SignalAgentRenderer`], and [`CloseAgentRenderer`].

use serde_json::Value;

use super::helpers::{string_field, truncate_preview};
use super::renderer::ToolRenderer;
use crate::terminal::caps::TerminalCaps;

fn short_id(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|s| s.chars().take(8).collect())
}

fn optional_field(args: &Value, result: &Value, key: &str) -> Option<String> {
    let value = string_field(args, result, key);
    if value.is_empty() { None } else { Some(value) }
}

fn push_part(parts: &mut Vec<String>, value: Option<String>) {
    if let Some(value) = value
        && !value.is_empty()
    {
        parts.push(value);
    }
}

/// Renders `spawn_agent` tool calls: a one-line note confirming the spawn.
pub struct SpawnAgentRenderer;

impl ToolRenderer for SpawnAgentRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let mut parts = Vec::new();
        push_part(&mut parts, optional_field(args, result, "role"));
        push_part(&mut parts, optional_field(args, result, "path"));
        push_part(&mut parts, optional_field(args, result, "model"));
        push_part(&mut parts, optional_field(args, result, "status"));
        push_part(&mut parts, short_id(result, "agent_id"));
        if parts.is_empty() {
            "spawned".to_string()
        } else {
            format!("spawned: {}", parts.join("  "))
        }
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, _partial_args: &str, _caps: &TerminalCaps) -> String {
        String::new()
    }
}

/// Renders `fork` tool calls: `fork → {model}  {request_preview}`.
pub struct ForkRenderer;

impl ToolRenderer for ForkRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let model = string_field(args, result, "model");
        let request = args.get("request").and_then(Value::as_str).unwrap_or("");
        let requirements = args
            .get("requirements")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let mut parts = Vec::new();
        push_part(&mut parts, Some(format!("fork → {model}")));
        push_part(
            &mut parts,
            (!request.is_empty()).then(|| truncate_preview(request)),
        );
        push_part(&mut parts, optional_field(args, result, "path"));
        push_part(&mut parts, optional_field(args, result, "status"));
        push_part(&mut parts, short_id(result, "agent_id"));
        if requirements > 0 {
            parts.push(format!("{requirements} reqs"));
        }
        parts.join("  ")
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, _partial_args: &str, _caps: &TerminalCaps) -> String {
        String::from("fork ⟳")
    }
}

/// Renders `signal_agent` tool calls: `→ {to} [{kind}]: {content_preview}`.
pub struct SignalAgentRenderer;

impl ToolRenderer for SignalAgentRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let to = args
            .get("to")
            .and_then(Value::as_str)
            .map_or_else(|| string_field(args, result, "to"), ToString::to_string);
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("");
        let content = args.get("content").and_then(Value::as_str).unwrap_or("");
        let preview = truncate_preview(content);
        let delivered = result
            .get("delivered")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let status = if delivered { "sent" } else { "failed" };
        let seq = result
            .get("seq")
            .and_then(Value::as_u64)
            .map(|seq| format!(" seq {seq}"))
            .unwrap_or_default();
        if kind.is_empty() {
            format!("→ {to} {status}{seq}: {preview}")
        } else {
            format!("→ {to} [{kind}] {status}{seq}: {preview}")
        }
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, _partial_args: &str, _caps: &TerminalCaps) -> String {
        String::from("→ ⟳")
    }
}

/// Silent renderer for `wait_agent` — the wait is invisible to the
/// user per D7. Empty header + `None` body hits the discard guard in
/// `write_tool_result`.
pub struct WaitAgentRenderer;

impl ToolRenderer for WaitAgentRenderer {
    fn header_line(
        &self,
        _args: &Value,
        _result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        String::new()
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, _partial_args: &str, _caps: &TerminalCaps) -> String {
        String::new()
    }
}

/// Renders `close_agent` tool calls with target and shutdown count.
pub struct CloseAgentRenderer;

impl ToolRenderer for CloseAgentRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let target = optional_field(args, result, "agent_id")
            .or_else(|| optional_field(args, result, "path"))
            .unwrap_or_else(|| "agent".to_string());
        if result
            .get("already_completed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let status = string_field(args, result, "status");
            return if status.is_empty() {
                format!("close: {target} already completed")
            } else {
                format!("close: {target} already {status}")
            };
        }
        let count = result
            .get("shut_down")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        match count {
            0 => format!("close: {target}"),
            1 => format!("closed: {target}"),
            n => format!("closed: {target}  ({n} agents)"),
        }
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, _partial_args: &str, _caps: &TerminalCaps) -> String {
        String::from("✕ ⟳")
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
    fn spawn_agent_header_uses_real_backend_shape() {
        let header = SpawnAgentRenderer.header_line(
            &json!({ "role": "researcher", "model": "gpt-5.5" }),
            &json!({
                "agent_id": "75395432-d9fd-4216-8c84-42d5cbdb26c6",
                "path": "/tool-smoke/spawn-child",
                "status": "active"
            }),
            0,
            &caps(),
        );
        assert!(header.contains("spawned: researcher"));
        assert!(header.contains("/tool-smoke/spawn-child"));
        assert!(header.contains("gpt-5.5"));
        assert!(header.contains("active"));
        assert!(header.contains("75395432"));
    }

    #[test]
    fn fork_header_includes_model_request_and_result_fields() {
        let header = ForkRenderer.header_line(
            &json!({
                "model": "gpt-5.4-mini",
                "request": "summarise the document",
                "requirements": [{ "name": "summary" }, { "name": "risks" }]
            }),
            &json!({
                "agent_id": "8ce649a0-577b-4c82-8a5b-26377680c5e4",
                "path": "/root/fork/ba5d9176-d5ce",
                "status": "active"
            }),
            0,
            &caps(),
        );
        assert!(header.contains("fork → gpt-5.4-mini"));
        assert!(header.contains("summarise"));
        assert!(header.contains("/root/fork/ba5d9176-d5ce"));
        assert!(header.contains("active"));
        assert!(header.contains("8ce649a0"));
        assert!(header.contains("2 reqs"));
    }

    #[test]
    fn signal_agent_header_includes_delivery_status() {
        let header = SignalAgentRenderer.header_line(
            &json!({ "to": "/workers/analyzer", "kind": "steer", "content": "check status" }),
            &json!({ "delivered": true, "to": "uuid", "kind": "steer", "seq": 3 }),
            0,
            &caps(),
        );
        assert!(header.contains("→ /workers/analyzer"));
        assert!(header.contains("[steer]"));
        assert!(header.contains("sent seq 3"));
        assert!(header.contains("check status"));
    }

    #[test]
    fn signal_agent_header_distinguishes_failure() {
        let header = SignalAgentRenderer.header_line(
            &json!({ "to": "/workers/analyzer", "kind": "update", "content": "context" }),
            &json!({ "delivered": false, "to": "uuid" }),
            0,
            &caps(),
        );
        assert!(header.contains("failed"));
        assert!(header.contains("context"));
    }

    #[test]
    fn close_agent_header_uses_shutdown_count() {
        let header = CloseAgentRenderer.header_line(
            &json!({ "agent_id": "/root/worker" }),
            &json!({
                "agent_id": "uuid",
                "reason": "stop",
                "shut_down": [
                    { "agent_id": "a", "status": "cancelled" },
                    { "agent_id": "b", "status": "cancelled" },
                    { "agent_id": "c", "status": "cancelled" }
                ]
            }),
            0,
            &caps(),
        );
        assert!(header.contains("closed: /root/worker"));
        assert!(header.contains("3 agents"));
    }

    #[test]
    fn close_agent_already_completed_is_explicit() {
        let header = CloseAgentRenderer.header_line(
            &json!({ "agent_id": "/leaf" }),
            &json!({
                "agent_id": "uuid",
                "path": "/leaf",
                "already_completed": true,
                "status": "completed"
            }),
            0,
            &caps(),
        );
        assert_eq!(header, "close: /leaf already completed");
    }

    #[test]
    fn wait_agent_is_completely_silent() {
        let result = json!({ "status": "completed", "output": "done" });
        let header = WaitAgentRenderer.header_line(&json!({}), &result, 500, &caps());
        assert!(
            header.is_empty(),
            "wait_agent must be invisible: {header:?}"
        );
        assert!(
            WaitAgentRenderer
                .body(&json!({}), &result, &caps())
                .is_none()
        );
    }

    #[test]
    fn all_minimal_renderers_return_none_body() {
        let empty = json!({});
        assert!(SpawnAgentRenderer.body(&empty, &empty, &caps()).is_none());
        assert!(ForkRenderer.body(&empty, &empty, &caps()).is_none());
        assert!(SignalAgentRenderer.body(&empty, &empty, &caps()).is_none());
        assert!(WaitAgentRenderer.body(&empty, &empty, &caps()).is_none());
        assert!(CloseAgentRenderer.body(&empty, &empty, &caps()).is_none());
    }
}
