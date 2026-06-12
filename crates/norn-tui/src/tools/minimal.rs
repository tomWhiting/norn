//! Tier 3 (minimal) tool renderers.
//!
//! Minimal renderers serve agent-coordination tools that produce little
//! or no visible output: [`SpawnAgentRenderer`],
//! [`ForkRenderer`], [`SignalAgentRenderer`], and [`CloseAgentRenderer`].

use serde_json::Value;

use super::helpers::{string_field, truncate_preview};
use super::renderer::ToolRenderer;
use crate::terminal::caps::TerminalCaps;

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
        let name = string_field(args, result, "name");
        format!("spawned: {name}")
    }

    fn body(&self, _args: &Value, _result: &Value, _caps: &TerminalCaps) -> Option<String> {
        None
    }

    fn streaming_header(&self, _name: &str, _partial_args: &str, _caps: &TerminalCaps) -> String {
        String::new()
    }
}

/// Renders `fork` tool calls: `fork → {model}  {task_preview}`.
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
        let task = args.get("task").and_then(Value::as_str).unwrap_or("");
        let preview = truncate_preview(task);
        if preview.is_empty() {
            format!("fork → {model}")
        } else {
            format!("fork → {model}  {preview}")
        }
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
        let to = string_field(args, result, "to");
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("");
        let content = args.get("content").and_then(Value::as_str).unwrap_or("");
        let preview = truncate_preview(content);
        if kind.is_empty() {
            format!("→ {to}: {preview}")
        } else {
            format!("→ {to} [{kind}]: {preview}")
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

/// Renders `close_agent` tool calls: `✕ {agent_path}  ({N} cascaded)`.
pub struct CloseAgentRenderer;

impl ToolRenderer for CloseAgentRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        _duration_ms: u64,
        _caps: &TerminalCaps,
    ) -> String {
        let path = string_field(args, result, "agent_path");
        let cascade = result
            .get("cascade_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if cascade > 0 {
            format!("✕ {path}  ({cascade} cascaded)")
        } else {
            format!("✕ {path}")
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
    fn spawn_agent_header_shows_name() {
        let header = SpawnAgentRenderer.header_line(
            &json!({ "name": "researcher" }),
            &json!({ "name": "researcher", "agent_id": "abc" }),
            0,
            &caps(),
        );
        assert_eq!(header, "spawned: researcher");
    }

    #[test]
    fn fork_header_includes_model_and_task() {
        let header = ForkRenderer.header_line(
            &json!({ "model": "haiku", "task": "summarise the document" }),
            &json!({ "model": "haiku" }),
            0,
            &caps(),
        );
        assert!(header.contains("fork → haiku"));
        assert!(header.contains("summarise"));
    }

    #[test]
    fn signal_agent_header_includes_recipient_kind_and_content() {
        let header = SignalAgentRenderer.header_line(
            &json!({ "to": "/workers/analyzer", "kind": "steer", "content": "check status" }),
            &json!({ "to": "/workers/analyzer" }),
            0,
            &caps(),
        );
        assert!(header.contains("→ /workers/analyzer"));
        assert!(header.contains("[steer]"));
        assert!(header.contains("check status"));
    }

    #[test]
    fn close_agent_header_includes_cascade_count() {
        let header = CloseAgentRenderer.header_line(
            &json!({ "agent_path": "root/worker" }),
            &json!({ "agent_path": "root/worker", "cascade_count": 3 }),
            0,
            &caps(),
        );
        assert!(header.contains("✕ root/worker"));
        assert!(header.contains("3 cascaded"));
    }

    #[test]
    fn close_agent_without_cascade_omits_count() {
        let header = CloseAgentRenderer.header_line(
            &json!({ "agent_path": "leaf" }),
            &json!({ "agent_path": "leaf", "cascade_count": 0 }),
            0,
            &caps(),
        );
        assert_eq!(header, "✕ leaf");
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
