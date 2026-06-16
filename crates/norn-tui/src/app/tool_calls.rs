//! Tool-call accumulation and result rendering.
//!
//! Extracted from `dispatch.rs` to keep that module under the 500-line
//! production code limit. Owns the tool-call lifecycle on the root
//! agent's rendering path: delta accumulation into the pending map,
//! envelope finalisation, and `ToolResult` rendering through the
//! per-tool renderers.

use serde_json::Value;
use termina::style::RgbColor;

use crate::TuiError;
use crate::render::fixed_panel::ToolUseInFlight;
use crate::render::scroll_region::write_to_scroll;
use crate::render::{MarkdownRenderer, colour_for};
use crate::terminal::setup::TerminalGuard;
use crate::tools::VerbosityState;
use crate::tools::renderer::renderer_for;

use super::helpers::{
    clear_dim_state, extract_tool_use_description, flush_terminal, sync_with_guard,
};
use super::state::{AppState, PendingToolCall};
use super::streaming::finish_thinking_block;

const TOOL_ERROR_RED: RgbColor = RgbColor::new(200, 80, 80);

/// Accumulate a `ToolCallDelta` into the pending map keyed by id.
///
/// Two side-effects on the streaming indicator: the delta's bytes add
/// to [`AppState::est_output_bytes`], and on the first delta carrying a
/// tool `name` the in-flight tool record is populated with the name
/// alone (description is `None` until the matching `ToolCallComplete`
/// arrives). This bridges the gap between "tool is starting" and "tool
/// envelope received" so the indicator shows the tool name as soon as
/// it's known rather than staying on `generating...`.
pub(super) fn accumulate_tool_call_delta(
    state: &mut AppState,
    id: String,
    name: Option<String>,
    arguments_delta: &str,
) {
    state.est_output_bytes = state.est_output_bytes.saturating_add(arguments_delta.len());
    let pending = state.pending_tools.entry(id).or_default();
    if pending.name.is_none()
        && let Some(n) = name
    {
        if state.current_tool_use.is_none() {
            state.current_tool_use = Some(ToolUseInFlight {
                tool_name: n.clone(),
                description: None,
            });
        }
        pending.name = Some(n);
    }
    pending.arguments.push_str(arguments_delta);
}

/// Finalise the pending tool call: persist its name and accumulated
/// argument JSON so [`handle_tool_result`] can render with the full
/// argument shape when the matching `ToolResult` arrives.
///
/// No scroll-region line is written for the in-flight state — the
/// fixed-panel streaming indicator already shows the user that
/// generation is in progress, and writing a transient header into the
/// scroll region would require rewriting it later when the result
/// arrives, which CO7 forbids ("Scroll region content is immutable
/// once written") and CO8 forbids (scroll-region writes are
/// append-only). One scroll-region line per completed tool call.
///
/// Side-effect on the streaming indicator: extracts the
/// `tool_use_description` envelope field from the assembled arguments
/// and folds it into [`AppState::current_tool_use`] so the indicator
/// can paint `● {tool}: '{description}' …` while the result is in
/// flight. When the model omits the envelope field, the indicator
/// keeps showing the tool name alone.
pub(super) fn handle_tool_call_complete(
    state: &mut AppState,
    id: String,
    name: &str,
    arguments: &str,
) {
    let description = extract_tool_use_description(arguments);
    state.current_tool_use = Some(ToolUseInFlight {
        tool_name: name.to_string(),
        description,
    });
    let pending = state.pending_tools.entry(id).or_default();
    pending.name = Some(name.to_string());
    pending.arguments = arguments.to_string();
}

/// Render the `ToolResult` through the per-tool renderer.
///
/// Side-effect on the streaming indicator: if the in-flight tool
/// matches `tool_name`, [`AppState::current_tool_use`] is cleared so
/// the indicator drops back to the no-tool form. Mismatched names are
/// left intact — this can happen when several tool calls overlap and
/// is handled by the next matching `ToolResult` / `TextDelta` clearing.
pub(super) fn handle_tool_result(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
    tool_call_id: &str,
    tool_name: &str,
    output: &Value,
    duration_ms: u64,
) -> Result<(), TuiError> {
    if state
        .current_tool_use
        .as_ref()
        .is_some_and(|t| t.tool_name == tool_name)
    {
        state.current_tool_use = None;
    }
    let pending = state.pending_tools.remove(tool_call_id).unwrap_or_default();
    let args = parse_args(&pending);
    write_tool_result(
        state,
        guard,
        renderer,
        tool_name,
        &args,
        output,
        duration_ms,
    )
}

/// Parse `pending.arguments` as JSON, falling back to `Value::Null` for
/// partial fragments.
pub fn parse_args(pending: &PendingToolCall) -> Value {
    if pending.arguments.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(&pending.arguments).unwrap_or(Value::Null)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StructuredToolError {
    header: String,
    body: Option<String>,
}

fn structured_tool_error(output: &Value, expanded: bool) -> Option<StructuredToolError> {
    let error = output.get("error")?;
    let (kind, message, detail) = match error {
        Value::Object(map) => {
            let kind = map.get("kind").and_then(Value::as_str).unwrap_or("");
            let message = map
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("tool failed");
            let detail = map.get("detail");
            (kind, message, detail)
        }
        Value::String(message) => ("", message.as_str(), None),
        other => ("", other.as_str().unwrap_or("tool failed"), None),
    };
    let message = truncate_error_message(message);
    let header = if kind.is_empty() {
        format!("error: {message}")
    } else {
        format!("error [{kind}]: {message}")
    };
    let body = if expanded {
        detail
            .and_then(|detail| serde_json::to_string_pretty(detail).ok())
            .filter(|text| !text.is_empty())
    } else {
        None
    };
    Some(StructuredToolError { header, body })
}

fn truncate_error_message(message: &str) -> String {
    const LIMIT: usize = 220;
    if message.len() <= LIMIT {
        return message.to_owned();
    }
    let end = message
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx <= LIMIT)
        .last()
        .unwrap_or(0);
    format!("{}...", &message[..end])
}

/// Render a [`norn::provider::events::ProviderEvent::ToolResult`]
/// through the per-tool renderer.
///
/// A dim `│` left border prefixes every line of tool output so tool
/// calls are visually distinct from assistant text. The header line
/// renders at normal intensity with its per-tool styling (error
/// colours, duration, etc.); the border alone is dim.
///
/// Falls back to `{tool_name}: {json}` for unknown tools (mirrors
/// [`crate::events::schema_render::render_tool_call`]). When a known
/// renderer returns an empty header and no body, nothing is written —
/// a bare `│` on a blank line is not output.
pub fn write_tool_result(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
    tool_name: &str,
    args: &Value,
    output: &Value,
    duration_ms: u64,
) -> Result<(), TuiError> {
    clear_dim_state(state, guard, renderer.as_mut())?;
    finish_thinking_block(state, guard, renderer)?;
    if state.text_streamed_this_turn || state.last_was_tool_result {
        write_to_scroll("\n", guard.terminal_mut())?;
        guard.note_scroll_newlines("\n")?;
    }

    let expanded = matches!(state.verbosity, VerbosityState::Expanded);
    if let Some(error) = structured_tool_error(output, expanded) {
        let colour = colour_for(TOOL_ERROR_RED, &state.terminal_caps);
        let mut out = format!(
            "\x1b[2m│ {tool_name}\x1b[22m  {colour}{}\x1b[39m",
            error.header,
        );
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if let Some(body) = error.body
            && !body.is_empty()
        {
            for line in body.lines() {
                out.push_str("\x1b[2m│\x1b[22m ");
                out.push_str(line);
                out.push('\n');
            }
        }
        let caps = state.terminal_caps.clone();
        sync_with_guard(&caps, guard, |guard| {
            write_to_scroll(&out, guard.terminal_mut())?;
            guard.note_scroll_newlines(&out)?;
            Ok(())
        })?;
        state.last_was_tool_result = true;
        state.text_streamed_this_turn = false;
        return flush_terminal(guard);
    }

    let Some(renderer) = renderer_for(tool_name) else {
        let json = serde_json::to_string(output).unwrap_or_default();
        let line = format!("\x1b[2m│\x1b[22m {tool_name}: {json}\n");
        write_to_scroll(&line, guard.terminal_mut())?;
        guard.note_scroll_newlines(&line)?;
        state.last_was_tool_result = true;
        state.text_streamed_this_turn = false;
        return flush_terminal(guard);
    };
    let header = renderer.header_line(args, output, duration_ms, &state.terminal_caps);
    let body = if expanded || header.is_empty() {
        let blocks = renderer.body_blocks(args, output, &state.terminal_caps);
        if let Some(ref blocks) = blocks {
            let rendered_blocks = crate::render::content::render_blocks(
                blocks,
                &state.highlighter,
                &state.terminal_caps,
            );
            if rendered_blocks.is_empty() {
                None
            } else {
                Some(rendered_blocks)
            }
        } else {
            renderer.body(args, output, &state.terminal_caps)
        }
    } else {
        None
    };
    if header.is_empty() && body.is_none() {
        return Ok(());
    }
    let mut out = format!("\x1b[2m│ {tool_name}\x1b[22m  {header}");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if expanded
        && let Some(body) = body
        && !body.is_empty()
    {
        for line in body.lines() {
            out.push_str("\x1b[2m│\x1b[22m ");
            out.push_str(line);
            out.push('\n');
        }
    }
    // Multi-line tool output (header + every body line) paints
    // atomically under DCS 2026 brackets so the user never sees a
    // half-rendered header. Baseline terminals fall back to cursor
    // hide/show. R11.
    let caps = state.terminal_caps.clone();
    sync_with_guard(&caps, guard, |guard| {
        write_to_scroll(&out, guard.terminal_mut())?;
        guard.note_scroll_newlines(&out)?;
        Ok(())
    })?;
    state.last_was_tool_result = true;
    state.text_streamed_this_turn = false;
    flush_terminal(guard)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use parking_lot::RwLock;

    use norn::agent::registry::AgentRegistry;

    use super::*;
    use crate::input::history::InputHistory;
    use crate::render::fixed_panel::StatusBar;
    use crate::terminal::caps::TerminalCaps;

    fn fresh_state() -> AppState {
        let registry: Arc<RwLock<AgentRegistry>> = AgentRegistry::shared();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root".to_string(),
            "lead".to_string(),
            "claude".to_string(),
            None,
            norn::agent::child_policy::ChildPolicy {
                messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: norn::agent::child_policy::DelegationBudget {
                    remaining_depth: 5,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
                loop_config: None,
            },
            None,
        )
        .unwrap();
        let root_id = guard.id();
        guard.confirm().unwrap();
        AppState::new(
            TerminalCaps::baseline(),
            InputHistory::in_memory(),
            registry,
            root_id,
            StatusBar::default(),
        )
    }

    #[test]
    fn parse_args_returns_null_for_partial_fragment() {
        let partial = PendingToolCall {
            name: Some("bash".to_string()),
            arguments: "{\"comm".to_string(),
        };
        assert_eq!(parse_args(&partial), Value::Null);
    }

    #[test]
    fn parse_args_returns_parsed_value_for_complete_json() {
        let complete = PendingToolCall {
            name: Some("bash".to_string()),
            arguments: "{\"command\":\"ls -la\"}".to_string(),
        };
        let parsed = parse_args(&complete);
        assert_eq!(parsed["command"], "ls -la");
    }

    #[test]
    fn parse_args_handles_empty_arguments() {
        let empty = PendingToolCall::default();
        assert_eq!(parse_args(&empty), Value::Null);
    }

    #[test]
    fn structured_tool_error_surfaces_kind_and_message() {
        let output = serde_json::json!({
            "error": {
                "kind": "unsupported",
                "message": "image search is not available on this text-only web surface",
                "detail": { "tool": "image_query" }
            }
        });
        let rendered = structured_tool_error(&output, false).unwrap();

        assert_eq!(
            rendered.header,
            "error [unsupported]: image search is not available on this text-only web surface",
        );
        assert!(rendered.body.is_none());
    }

    #[test]
    fn structured_tool_error_includes_detail_when_expanded() {
        let output = serde_json::json!({
            "error": {
                "kind": "capability_unavailable",
                "message": "image search unavailable",
                "detail": { "surface": "text-only" }
            }
        });
        let rendered = structured_tool_error(&output, true).unwrap();

        assert!(rendered.header.contains("capability_unavailable"));
        assert!(rendered.body.unwrap().contains("text-only"));
    }

    #[test]
    fn accumulate_tool_call_delta_concatenates_arguments() {
        let mut state = fresh_state();
        accumulate_tool_call_delta(
            &mut state,
            "tc_1".to_string(),
            Some("bash".to_string()),
            "{\"command\":",
        );
        accumulate_tool_call_delta(&mut state, "tc_1".to_string(), None, "\"ls\"}");
        let pending = &state.pending_tools["tc_1"];
        assert_eq!(pending.name.as_deref(), Some("bash"));
        assert_eq!(pending.arguments, "{\"command\":\"ls\"}");
        let parsed = parse_args(pending);
        assert_eq!(parsed["command"], "ls");
    }

    // Smoke check: pending-tools map remains accessible after a mutation
    // through the helper.
    #[test]
    fn pending_tools_map_is_a_hashmap_indexed_by_id() {
        let mut state = fresh_state();
        accumulate_tool_call_delta(
            &mut state,
            "abc".to_string(),
            Some("read".to_string()),
            "{}",
        );
        let _: &HashMap<String, PendingToolCall> = &state.pending_tools;
        assert!(state.pending_tools.contains_key("abc"));
    }
}
