//! Agent-event dispatch handlers used by the event loop.
//!
//! [`handle_agent_event`] is the top-level entry point: it receives
//! a tagged [`AgentEvent`] and routes by agent identity — root events
//! flow through the full rendering pipeline via
//! [`handle_provider_event`], while child events route only to the
//! activity log and agent status panel via [`handle_child_event`].

use std::io::Write as _;
use std::time::{Duration, Instant};

use serde_json::Value;
use termina::Terminal as _;

use norn::error::{NornError, ProviderError};
use norn::r#loop::config::AgentStepResult;
use norn::provider::agent_event::AgentEvent;
use norn::provider::events::ProviderEvent;
use norn::provider::usage::Usage;

use crate::TuiError;
use crate::agents::activity_log::ActivityLogEntry;
use crate::agents::status_line::AgentActivity;
use crate::events::DisplayToggles;
use crate::render::MarkdownRenderer;
use crate::render::fixed_panel::{StreamingIndicator, ToolUseInFlight};
use crate::render::markdown::FeedOutput;
use crate::render::scroll_region::write_to_scroll;
use crate::terminal::setup::TerminalGuard;
use crate::tools::VerbosityState;
use crate::tools::renderer::renderer_for;

use super::helpers::{
    clear_dim_state, dim_line_count, erase_dim_lines, extract_argument_summary,
    extract_tool_use_description, flush_markdown, flush_pending, flush_terminal,
    format_usage_summary, sync_with_guard,
};
use super::state::{AppState, PendingToolCall};

/// Dispatch a tagged [`AgentEvent`] by routing on agent identity.
///
/// Root-agent events flow through the full rendering pipeline
/// (scroll region, markdown, tool renderers). Child-agent events
/// route only to the activity log and the agent status panel —
/// their streaming text and tool output stay out of the main
/// scroll region.
pub fn handle_agent_event(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
    agent_event: AgentEvent,
) -> Result<(), TuiError> {
    let root_id = state.tab_state.root_id();
    if agent_event.agent_id == root_id {
        return handle_provider_event(state, guard, renderer, agent_event.event);
    }
    handle_child_event(state, agent_event);
    Ok(())
}

/// Route a child agent's event to the activity log and status panel.
///
/// Only `ToolCallComplete`, `ToolResult`, and `Done` carry
/// meaningful observability data for the external printer; delta
/// events from children are silently dropped since children don't
/// render into the scroll region.
fn handle_child_event(state: &mut AppState, agent_event: AgentEvent) {
    let child_id = agent_event.agent_id;
    match agent_event.event {
        ProviderEvent::ToolCallComplete {
            name, arguments, ..
        } => {
            state
                .agent_panel
                .set_activity(child_id, AgentActivity::Running(name.clone()));
            let description = extract_tool_use_description(&arguments)
                .or_else(|| extract_argument_summary(&arguments));
            state.activity_log.push(ActivityLogEntry {
                agent_role: agent_event.agent_role.to_string(),
                tool_name: name,
                description,
                at: std::time::Instant::now(),
            });
        }
        ProviderEvent::ToolResult { tool_name, .. } => {
            state
                .agent_panel
                .set_activity(child_id, AgentActivity::Result(tool_name));
        }
        ProviderEvent::Done { usage, .. } => {
            state
                .agent_panel
                .set_activity(child_id, AgentActivity::Idle);
            state
                .agent_panel
                .set_tokens(child_id, usage.input_tokens, usage.output_tokens);
        }
        _ => {}
    }
}

/// Dispatch a single [`ProviderEvent`] to its handler.
pub fn handle_provider_event(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
    event: ProviderEvent,
) -> Result<(), TuiError> {
    state.note_event_received(Instant::now());
    match event {
        ProviderEvent::TextDelta { text } => handle_text_delta(state, guard, renderer, &text),
        ProviderEvent::ThinkingDelta { text } => handle_thinking_delta(state, guard, &text),
        ProviderEvent::ToolCallDelta {
            item_id,
            name,
            arguments_delta,
            kind: _,
        } => {
            accumulate_tool_call_delta(state, item_id, name, &arguments_delta);
            Ok(())
        }
        ProviderEvent::ToolCallComplete {
            call_id,
            name,
            arguments,
            kind: _,
        } => {
            let root_id = state.tab_state.root_id();
            state
                .agent_panel
                .set_activity(root_id, AgentActivity::Running(name.clone()));
            let description = extract_tool_use_description(&arguments)
                .or_else(|| extract_argument_summary(&arguments));
            state.activity_log.push(ActivityLogEntry {
                agent_role: "root".to_string(),
                tool_name: name.clone(),
                description,
                at: Instant::now(),
            });
            handle_tool_call_complete(state, call_id, &name, &arguments);
            Ok(())
        }
        ProviderEvent::ToolResult {
            tool_call_id,
            tool_name,
            output,
            duration_ms,
        } => {
            let root_id = state.tab_state.root_id();
            state
                .agent_panel
                .set_activity(root_id, AgentActivity::Result(tool_name.clone()));
            handle_tool_result(
                state,
                guard,
                renderer,
                &tool_call_id,
                &tool_name,
                &output,
                duration_ms,
            )
        }
        ProviderEvent::Done { usage, .. } => {
            let root_id = state.tab_state.root_id();
            state.agent_panel.set_activity(root_id, AgentActivity::Idle);
            state
                .agent_panel
                .set_tokens(root_id, usage.input_tokens, usage.output_tokens);
            handle_done(state, guard, &usage, renderer)
        }
        ProviderEvent::Error { error } => handle_error(state, guard, &error, renderer),
        ProviderEvent::TextComplete { .. }
        | ProviderEvent::ThinkingComplete { .. }
        | ProviderEvent::Compaction { .. } => Ok(()),
    }
}

/// Erase any thinking dim text from the scroll region.
///
/// Thinking deltas write dim SGR text without calling
/// `note_scroll_newlines`, so the software cursor falls behind the
/// hardware cursor. Erasing before non-thinking writes prevents
/// blank-line accumulation during tool-heavy work.
fn clear_thinking_buffer(state: &mut AppState, guard: &mut TerminalGuard) -> Result<(), TuiError> {
    if state.thinking_buffer.is_empty() {
        return Ok(());
    }
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let lines = dim_line_count(&state.thinking_buffer, cols);
    erase_dim_lines(lines, guard)?;
    state.thinking_buffer.clear();
    Ok(())
}

/// Accept a `TextDelta`: feed through the streaming
/// [`MarkdownRenderer`] and write dim preview + styled output.
///
/// Dim-stream: every token is written in dim SGR immediately for
/// streaming feel. When a complete line arrives (`\n`), the dim text
/// is cleared (CR + erase-in-line) and replaced with the fully
/// styled version — the "line-pop" effect. Inside code fences, dim
/// preview is suppressed; the entire block is written styled.
fn handle_text_delta(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
    text: &str,
) -> Result<(), TuiError> {
    if text.is_empty() {
        return Ok(());
    }
    clear_thinking_buffer(state, guard)?;
    if state.last_was_tool_result {
        write_to_scroll("\n", guard.terminal_mut())?;
        guard.note_scroll_newlines("\n")?;
        state.last_was_tool_result = false;
    }
    state.est_output_bytes = state.est_output_bytes.saturating_add(text.len());
    state.current_tool_use = None;
    let r = if let Some(r) = renderer {
        r
    } else {
        let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
        renderer.insert(MarkdownRenderer::new(state.terminal_caps.clone(), cols))
    };
    let output = r.feed(text);
    let wrote;
    if output.replace_dim {
        // Composite frame: erase the live dim → write the styled
        // replacement → optionally write the next dim preview. Wrap
        // in DCS 2026 brackets (cursor hide/show on baseline
        // terminals) so the terminal paints the line-pop atomically
        // and the user never sees the cleared-but-not-yet-rewritten
        // intermediate state. R11.
        let caps = state.terminal_caps.clone();
        sync_with_guard(&caps, guard, |guard| {
            erase_dim_lines(state.dim_wrapped_lines, guard)?;
            state.dim_wrapped_lines = 0;
            write_styled_then_dim(state, guard, &output)?;
            Ok(())
        })?;
        wrote = true;
    } else {
        // No prior dim to replace — styled and dim are written
        // individually. Per brief, individual dim writes are NOT
        // wrapped in sync brackets (the overhead of the bracket pair
        // would exceed the render time for a single short write).
        wrote = write_styled_then_dim(state, guard, &output)?;
    }
    if wrote {
        flush_terminal(guard)?;
    }
    state.text_streamed_this_turn = true;
    Ok(())
}

/// Emit the styled segment (if any), insert a separator newline when a
/// new dim preview needs its own line below mid-line styled output,
/// then write the dim preview wrapped in dim SGR. Shared between the
/// sync-bracketed `replace_dim` path and the unwrapped append path so
/// the line-pop logic stays in lockstep. Returns whether any bytes
/// were written so the caller can gate `flush_terminal`.
fn write_styled_then_dim(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    output: &FeedOutput,
) -> Result<bool, TuiError> {
    let mut wrote = false;
    if !output.styled.is_empty() {
        write_to_scroll(&output.styled, guard.terminal_mut())?;
        guard.note_scroll_newlines(&output.styled)?;
        state.styled_mid_line = !output.styled.ends_with('\n');
        wrote = true;
    }
    if !output.dim.is_empty() && state.styled_mid_line {
        write_to_scroll("\n", guard.terminal_mut())?;
        guard.note_scroll_newlines("\n")?;
        state.styled_mid_line = false;
    }
    if !output.dim.is_empty() {
        let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
        let lines = dim_line_count(&output.dim, cols);
        let avail = guard.scroll_rows_below_cursor();
        if lines <= avail {
            let writer = guard.terminal_mut();
            write!(writer, "\x1b[2m")?;
            write_to_scroll(&output.dim, writer)?;
            write!(writer, "\x1b[22m")?;
            state.dim_wrapped_lines = lines;
            wrote = true;
        }
    }
    Ok(wrote)
}

/// Accept a `ThinkingDelta`: render dim-styled output when thinking is
/// visible, otherwise discard (ephemeral, never persisted — C12).
///
/// The tokens are spent regardless of whether the bytes are painted, so
/// [`AppState::est_output_bytes`] accumulates either way. `ThinkingDelta`
/// also signals the model has moved past any pending tool call (it's
/// processing, not executing), so [`AppState::current_tool_use`] is
/// cleared.
fn handle_thinking_delta(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    text: &str,
) -> Result<(), TuiError> {
    state.est_output_bytes = state.est_output_bytes.saturating_add(text.len());
    state.current_tool_use = None;
    let dim = render_thinking_delta(text, state.display_toggles);
    if dim.is_empty() {
        return Ok(());
    }
    state.thinking_buffer.push_str(&dim);
    write_to_scroll(&dim, guard.terminal_mut())?;
    flush_terminal(guard)
}

/// Accumulate a `ToolCallDelta` into the pending map keyed by id.
///
/// Two side-effects on the streaming indicator: the delta's bytes add
/// to [`AppState::est_output_bytes`], and on the first delta carrying a
/// tool `name` the in-flight tool record is populated with the name
/// alone (description is `None` until the matching `ToolCallComplete`
/// arrives). This bridges the gap between "tool is starting" and "tool
/// envelope received" so the indicator shows the tool name as soon as
/// it's known rather than staying on `generating...`.
fn accumulate_tool_call_delta(
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
fn handle_tool_call_complete(state: &mut AppState, id: String, name: &str, arguments: &str) {
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
fn handle_tool_result(
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

/// Handle [`ProviderEvent::Done`].
///
/// Flushes trailing markdown but does NOT flush pending tool calls. The
/// Done event fires when the provider stream ends — tool results arrive
/// later on the broadcast channel. Flushing pending tools here would
/// render them with null output ("0 results"), then the real `ToolResult`
/// would render again, causing duplication. Pending tools are flushed by
/// their matching `ToolResult` events, or by [`flush_pending`] on error /
/// turn finalization if no result arrives.
pub fn handle_done(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    usage: &Usage,
    renderer: &mut Option<MarkdownRenderer>,
) -> Result<(), TuiError> {
    clear_thinking_buffer(state, guard)?;
    flush_markdown(state, guard, renderer)?;
    if state.text_streamed_this_turn {
        write_to_scroll("\n", guard.terminal_mut())?;
        guard.note_scroll_newlines("\n")?;
        flush_terminal(guard)?;
    }
    let elapsed = state
        .turn_start
        .map_or(Duration::ZERO, |start| start.elapsed());
    let summary = format_usage_summary(usage, elapsed);
    state.mark_complete(summary, Instant::now());
    state.sync_indicator_into_panel();
    Ok(())
}

/// Handle [`ProviderEvent::Error`].
pub fn handle_error(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    error: &ProviderError,
    renderer: &mut Option<MarkdownRenderer>,
) -> Result<(), TuiError> {
    flush_pending(state, guard, renderer)?;
    write_error_line(state, guard, &error.to_string())
}

/// Convert the agent-step result into either a usage indicator or an
/// error line in the scroll region.
pub fn finalise_turn(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    step_result: Option<Result<AgentStepResult, NornError>>,
    renderer: &mut Option<MarkdownRenderer>,
) -> Result<(), TuiError> {
    let Some(result) = step_result else {
        return Ok(());
    };
    match result {
        Ok(step) => set_complete_from_step(state, &step),
        Err(err) => {
            flush_pending(state, guard, renderer)?;
            write_error_line(state, guard, &err.to_string())?;
        }
    }
    state.sync_indicator_into_panel();
    Ok(())
}

fn set_complete_from_step(state: &mut AppState, step: &AgentStepResult) {
    if !matches!(state.streaming_indicator, StreamingIndicator::Idle) {
        return;
    }
    let usage = extract_usage(step);
    let elapsed = state
        .turn_start
        .map_or(Duration::ZERO, |start| start.elapsed());
    let summary = format_usage_summary(&usage, elapsed);
    state.mark_complete(summary, Instant::now());
}

/// Extract the `Usage` field from any [`AgentStepResult`] variant.
pub fn extract_usage(result: &AgentStepResult) -> Usage {
    match result {
        AgentStepResult::Completed { usage, .. }
        | AgentStepResult::SchemaUnreachable { usage, .. }
        | AgentStepResult::MaxIterationsReached { usage, .. }
        | AgentStepResult::Cancelled { usage, .. } => usage.clone(),
        AgentStepResult::TimedOut { .. } => Usage::default(),
    }
}

/// Render a `ThinkingDelta` chunk wrapped in dim SGR markers, or return
/// the empty string when thinking is hidden.
pub fn render_thinking_delta(text: &str, toggles: DisplayToggles) -> String {
    if !toggles.thinking_visible || text.is_empty() {
        return String::new();
    }
    format!("\x1b[2m{text}\x1b[22m")
}

/// Parse `pending.arguments` as JSON, falling back to `Value::Null` for
/// partial fragments.
pub fn parse_args(pending: &PendingToolCall) -> Value {
    if pending.arguments.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(&pending.arguments).unwrap_or(Value::Null)
}

/// Render a [`ProviderEvent::ToolResult`] through the per-tool renderer.
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
    clear_thinking_buffer(state, guard)?;
    if state.text_streamed_this_turn || state.last_was_tool_result {
        write_to_scroll("\n", guard.terminal_mut())?;
        guard.note_scroll_newlines("\n")?;
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
    let expanded = matches!(state.verbosity, VerbosityState::Expanded);
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

/// Write a red `error: {message}` line into the scroll region.
fn write_error_line(
    state: &AppState,
    guard: &mut TerminalGuard,
    message: &str,
) -> Result<(), TuiError> {
    let red = crate::render::style::colour_for(
        termina::style::RgbColor::new(200, 80, 80),
        &state.terminal_caps,
    );
    let reset = termina::escape::csi::Csi::Sgr(termina::escape::csi::Sgr::Reset).to_string();
    let line = format!("{red}error: {message}{reset}\n");
    write_to_scroll(&line, guard.terminal_mut())?;
    guard.note_scroll_newlines(&line)?;
    flush_terminal(guard)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use parking_lot::RwLock;

    use norn::agent::registry::AgentRegistry;
    use norn::error::ProviderError;

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
    fn text_delta_pipeline_writes_hello_to_writer() {
        let caps = TerminalCaps::baseline();
        let mut renderer = MarkdownRenderer::new(caps, 80);
        let mut buf: Vec<u8> = Vec::new();
        let output = renderer.feed("hello");
        if !output.dim.is_empty() {
            crate::render::scroll_region::write_to_scroll(&output.dim, &mut buf).unwrap();
        }
        if !output.styled.is_empty() {
            crate::render::scroll_region::write_to_scroll(&output.styled, &mut buf).unwrap();
        }
        let tail = renderer.finalize();
        if !tail.styled.is_empty() {
            crate::render::scroll_region::write_to_scroll(&tail.styled, &mut buf).unwrap();
        }
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("hello"), "got: {out:?}");
    }

    #[test]
    fn render_thinking_delta_hidden_yields_empty() {
        let toggles = DisplayToggles::default();
        assert!(render_thinking_delta("pondering", toggles).is_empty());
    }

    #[test]
    fn render_thinking_delta_visible_wraps_in_dim_sgr() {
        let mut toggles = DisplayToggles::default();
        toggles.toggle();
        let out = render_thinking_delta("pondering", toggles);
        assert!(out.contains("\x1b[2m"));
        assert!(out.contains("\x1b[22m"));
        assert!(out.contains("pondering"));
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

    #[test]
    fn format_usage_summary_matches_print_mode_shape() {
        let usage = Usage {
            input_tokens: 1_234,
            output_tokens: 5_678,
            ..Usage::default()
        };
        let summary = format_usage_summary(&usage, Duration::from_millis(1_200));
        assert!(summary.contains("1,234 in"));
        assert!(summary.contains("5,678 out"));
        assert!(summary.contains("1.2s"));
        assert!(summary.contains("in /"));
    }

    #[test]
    fn extract_usage_completed_pulls_usage_field() {
        let usage = Usage {
            input_tokens: 12,
            output_tokens: 34,
            ..Usage::default()
        };
        let step = AgentStepResult::Completed {
            usage,
            output: Value::Null,
        };
        let extracted = extract_usage(&step);
        assert_eq!(extracted.input_tokens, 12);
        assert_eq!(extracted.output_tokens, 34);
    }

    #[test]
    fn extract_usage_timed_out_returns_default() {
        let step = AgentStepResult::TimedOut {
            elapsed: Duration::from_mins(1),
            iterations: 3,
            partial_output: None,
        };
        let extracted = extract_usage(&step);
        assert_eq!(extracted.input_tokens, 0);
        assert_eq!(extracted.output_tokens, 0);
    }

    #[test]
    fn provider_error_renders_with_red_palette_escape() {
        let caps = TerminalCaps::baseline();
        let red =
            crate::render::style::colour_for(termina::style::RgbColor::new(200, 80, 80), &caps);
        let err = ProviderError::StreamInterrupted {
            reason: "boom".to_string(),
        };
        let line = format!("{red}error: {err}");
        assert!(line.contains("38;5;"));
        assert!(line.contains("error:"));
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

    // ---------------- AgentStatusPanel wire-up (Task 1) ----------------
    //
    // These tests cover the contract that handle_provider_event upholds:
    // after a tool-call event flows through dispatch, the
    // [`AgentStatusPanel`] cache reflects the new state. We exercise the
    // mutation directly (mirroring the lines in `handle_provider_event`)
    // because `handle_provider_event` requires a [`TerminalGuard`] and
    // there is no way to construct one without a real terminal. The
    // rendering pass — which is the only externally observable proof
    // that activity/tokens stuck — is identical regardless of caller.

    fn state_with_one_child() -> (AppState, uuid::Uuid) {
        let registry: Arc<RwLock<AgentRegistry>> = AgentRegistry::shared();
        let root_guard = AgentRegistry::reserve(
            &registry,
            "/root".to_string(),
            "lead".to_string(),
            "claude".to_string(),
            None,
        )
        .unwrap();
        let root_id = root_guard.id();
        root_guard.confirm().unwrap();

        let child_guard = AgentRegistry::reserve(
            &registry,
            "/root/child".to_string(),
            "dev".to_string(),
            "haiku".to_string(),
            Some(root_id),
        )
        .unwrap();
        child_guard.confirm().unwrap();

        let state = AppState::new(
            TerminalCaps::baseline(),
            InputHistory::in_memory(),
            registry,
            root_id,
            StatusBar::default(),
        );
        (state, root_id)
    }

    #[test]
    fn tool_call_complete_running_activity_surfaces_in_render() {
        let (mut state, root_id) = state_with_one_child();
        // Mirror the dispatch hook for ProviderEvent::ToolCallComplete.
        state
            .agent_panel
            .set_activity(root_id, AgentActivity::Running("bash".to_string()));

        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        state
            .agent_panel
            .render(
                0,
                &mut buf,
                &caps,
                std::time::Instant::now(),
                chrono::Utc::now(),
            )
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("bash"),
            "Running tool name must surface on root row: {out:?}"
        );
    }

    #[test]
    fn tool_result_sets_result_activity_on_root() {
        let (mut state, root_id) = state_with_one_child();
        // Mirror the dispatch hook for ProviderEvent::ToolResult.
        state
            .agent_panel
            .set_activity(root_id, AgentActivity::Result("read".to_string()));

        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        state
            .agent_panel
            .render(
                0,
                &mut buf,
                &caps,
                std::time::Instant::now(),
                chrono::Utc::now(),
            )
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("read"),
            "Result tool name must surface on root row: {out:?}"
        );
    }

    #[test]
    fn done_event_sets_idle_and_token_counts() {
        let (mut state, root_id) = state_with_one_child();
        // Mirror the dispatch hook for ProviderEvent::Done.
        state.agent_panel.set_activity(root_id, AgentActivity::Idle);
        state.agent_panel.set_tokens(root_id, 5_000, 2_000);

        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        state
            .agent_panel
            .render(
                0,
                &mut buf,
                &caps,
                std::time::Instant::now(),
                chrono::Utc::now(),
            )
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains('◌'),
            "Idle activity on Active root must render the dotted-circle swap: {out:?}"
        );
        assert!(
            out.contains("7k"),
            "Combined 7k tokens (5k in + 2k out) must surface: {out:?}"
        );
    }

    // ---------------- Activity log wire-up (Task 2 interim) ----------------

    #[test]
    fn tool_call_complete_pushes_activity_log_entry_with_description() {
        // Mirror the dispatch hook in handle_provider_event for
        // ProviderEvent::ToolCallComplete: the activity log receives a
        // new entry with the tool name and the extracted envelope
        // description, agent_role hardcoded to "root" for the interim
        // wire.
        let (mut state, _root_id) = state_with_one_child();
        let args = serde_json::json!({
            "tool_use_description": "listing docs folder",
            "command": "ls docs/"
        })
        .to_string();

        state.activity_log.push(ActivityLogEntry {
            agent_role: "root".to_string(),
            tool_name: "bash".to_string(),
            description: extract_tool_use_description(&args),
            at: std::time::Instant::now(),
        });

        assert_eq!(state.activity_log.len(), 1);
        let entry = state.activity_log.entries().front().unwrap();
        assert_eq!(entry.agent_role, "root");
        assert_eq!(entry.tool_name, "bash");
        assert_eq!(entry.description.as_deref(), Some("listing docs folder"));
    }

    #[test]
    fn tool_call_complete_with_empty_description_pushes_none() {
        // Some("") from the envelope is normalised to None by
        // extract_tool_use_description — the activity log keeps the
        // same policy as the streaming indicator.
        let (mut state, _root_id) = state_with_one_child();
        let args = serde_json::json!({
            "tool_use_description": "   ",
            "command": "ls"
        })
        .to_string();

        state.activity_log.push(ActivityLogEntry {
            agent_role: "root".to_string(),
            tool_name: "bash".to_string(),
            description: extract_tool_use_description(&args),
            at: std::time::Instant::now(),
        });

        let entry = state.activity_log.entries().front().unwrap();
        assert!(entry.description.is_none());
    }
}
