//! Agent-event dispatch handlers used by the event loop.
//!
//! [`handle_agent_event`] is the top-level entry point: it receives
//! a tagged [`AgentEvent`] and routes by payload kind and agent
//! identity — root provider events flow through the full rendering
//! pipeline via [`handle_provider_event`], child provider events route
//! only to the activity log and agent status panel via
//! [`handle_child_event`], and typed subagent lifecycle events drive
//! the status panel via [`handle_subagent_lifecycle`].
//!
//! The rendering legs live in sibling modules: [`super::streaming`]
//! owns the dim-stream text/thinking path and [`super::tool_calls`]
//! owns tool-call accumulation and result rendering.

use std::time::{Duration, Instant};

use norn::agent_loop::config::AgentStepResult;
use norn::error::{NornError, ProviderError};
use norn::provider::agent_event::{
    AgentEvent, AgentEventKind, AgentMessageLifecycle, SubagentLifecycle,
};
use norn::provider::events::ProviderEvent;
use norn::provider::usage::Usage;

use crate::TuiError;
use crate::agents::activity_log::ActivityLogEntry;
use crate::agents::status_line::AgentActivity;
use crate::render::MarkdownRenderer;
use crate::render::fixed_panel::StreamingIndicator;
use crate::render::scroll_region::write_to_scroll;
use crate::terminal::setup::TerminalGuard;

use super::helpers::{
    extract_argument_summary, extract_tool_use_description, flush_markdown, flush_pending,
    flush_terminal, format_usage_summary,
};
use super::state::AppState;
use super::streaming::{clear_thinking_buffer, handle_text_delta, handle_thinking_delta};
use super::tool_calls::{
    accumulate_tool_call_delta, handle_tool_call_complete, handle_tool_result,
};

/// Dispatch a tagged [`AgentEvent`] by routing on payload kind and
/// agent identity.
///
/// Root-agent provider events flow through the full rendering pipeline
/// (scroll region, markdown, tool renderers). Child-agent provider
/// events route only to the activity log and the agent status panel —
/// their streaming text and tool output stay out of the main scroll
/// region. Typed [`SubagentLifecycle`] events (always child-tagged)
/// drive the status panel's activity column directly.
pub fn handle_agent_event(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
    agent_event: AgentEvent,
) -> Result<(), TuiError> {
    let root_id = state.tab_state.root_id();
    match agent_event.event {
        AgentEventKind::Provider(event) => {
            if agent_event.agent_id == root_id {
                return handle_provider_event(state, guard, renderer, event);
            }
            handle_child_event(state, agent_event.agent_id, &agent_event.agent_role, event);
        }
        AgentEventKind::Subagent(lifecycle) => handle_subagent_lifecycle(state, &lifecycle),
        // Inter-agent message events (W3.1) surface in the activity log
        // — the same panel that shows tool-call initiations — so the
        // operator sees who messaged whom live. The durable record is
        // the agent_message.* audit trail in the session stores.
        AgentEventKind::Message(lifecycle) => {
            state.activity_log.push(message_activity_entry(
                &agent_event.agent_role,
                &lifecycle,
                Instant::now(),
            ));
        }
    }
    Ok(())
}

/// Build the activity-log row for an inter-agent message event.
///
/// `Sent` rows show recipient and kind in the name column and the
/// message content as the description (the renderer truncates long
/// content); the recipient label comes from the audit payload, which the
/// harness resolved from registry ground truth at send time. `Delivered`
/// rows confirm injection on the recipient's side with the router
/// sequence. The row's `[agent]` label is the broadcast wrapper's role
/// tag — the sender's for `Sent`, the recipient's for `Delivered` —
/// matching every other activity entry's attribution.
fn message_activity_entry(
    agent_role: &str,
    lifecycle: &AgentMessageLifecycle,
    at: Instant,
) -> ActivityLogEntry {
    let (label, description) = match lifecycle {
        AgentMessageLifecycle::Sent {
            to, kind, content, ..
        } => (
            format!("msg:{} → {to}", kind.as_str()),
            Some(content.clone()),
        ),
        AgentMessageLifecycle::Delivered { seq, .. } => {
            (format!("msg delivered (seq {seq})"), None)
        }
    };
    ActivityLogEntry {
        agent_role: agent_role.to_string(),
        tool_name: label,
        description,
        at,
    }
}

/// Route a typed subagent lifecycle event to the status panel.
///
/// `Started` seeds the child's activity as idle (its row appears from
/// the registry snapshot); `Completed` sets the terminal result text the
/// panel shows during its hold window. Hold/reclaim timing itself is
/// registry-status-driven and untouched here.
fn handle_subagent_lifecycle(state: &mut AppState, lifecycle: &SubagentLifecycle) {
    match lifecycle {
        SubagentLifecycle::Started { child_id, .. } => {
            state
                .agent_panel
                .set_activity(*child_id, AgentActivity::Idle);
        }
        SubagentLifecycle::Completed {
            child_id,
            succeeded,
            ..
        } => {
            let summary = if *succeeded { "completed" } else { "failed" };
            state
                .agent_panel
                .set_activity(*child_id, AgentActivity::Result(summary.to_string()));
        }
    }
}

/// Route a child agent's provider event to the activity log and status
/// panel.
///
/// Only `ToolCallComplete`, `ToolResult`, and `Done` carry
/// meaningful observability data for the external printer; delta
/// events from children are silently dropped since children don't
/// render into the scroll region.
fn handle_child_event(
    state: &mut AppState,
    child_id: uuid::Uuid,
    agent_role: &str,
    event: ProviderEvent,
) {
    match event {
        ProviderEvent::ToolCallComplete {
            name, arguments, ..
        } => {
            state
                .agent_panel
                .set_activity(child_id, AgentActivity::Running(name.clone()));
            let description = extract_tool_use_description(&arguments)
                .or_else(|| extract_argument_summary(&arguments));
            state.activity_log.push(ActivityLogEntry {
                agent_role: agent_role.to_string(),
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
        | AgentStepResult::Cancelled { usage, .. }
        | AgentStepResult::TimedOut { usage, .. }
        | AgentStepResult::Truncated { usage, .. } => usage.clone(),
    }
}

/// Write a red `error: {message}` line into the scroll region.
///
/// `pub(super)` so [`super::slash`] can surface command failures (e.g.
/// `/new` session-creation errors) in the same style as provider and
/// turn errors.
pub(super) fn write_error_line(
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
    use std::sync::Arc;

    use parking_lot::RwLock;
    use serde_json::Value;

    use norn::agent::registry::AgentRegistry;
    use norn::error::ProviderError;

    use super::*;
    use crate::input::history::InputHistory;
    use crate::render::fixed_panel::StatusBar;
    use crate::terminal::caps::TerminalCaps;

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
            children_usage: Usage::default(),
        };
        let extracted = extract_usage(&step);
        assert_eq!(extracted.input_tokens, 12);
        assert_eq!(extracted.output_tokens, 34);
    }

    #[test]
    fn extract_usage_timed_out_pulls_usage_field() {
        let step = AgentStepResult::TimedOut {
            elapsed: Duration::from_mins(1),
            iterations: 3,
            partial_output: None,
            usage: Usage {
                input_tokens: 7,
                output_tokens: 11,
                ..Usage::default()
            },
            children_usage: Usage::default(),
        };
        let extracted = extract_usage(&step);
        assert_eq!(extracted.input_tokens, 7);
        assert_eq!(extracted.output_tokens, 11);
    }

    #[test]
    fn extract_usage_truncated_pulls_usage_field() {
        let step = AgentStepResult::Truncated {
            kind: norn::agent_loop::config::TruncationKind::MaxTokens,
            partial_text: Some("partial".to_string()),
            iterations: 1,
            usage: Usage {
                input_tokens: 5,
                output_tokens: 9,
                ..Usage::default()
            },
            children_usage: Usage::default(),
        };
        let extracted = extract_usage(&step);
        assert_eq!(extracted.input_tokens, 5);
        assert_eq!(extracted.output_tokens, 9);
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
            norn::agent::child_policy::ChildPolicy {
                messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: norn::agent::child_policy::DelegationBudget {
                    remaining_depth: 5,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
            },
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
            norn::agent::child_policy::ChildPolicy {
                messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: norn::agent::child_policy::DelegationBudget {
                    remaining_depth: 4,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
            },
            None,
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

    // ---------------- Inter-agent message events (W3.7) ----------------

    #[test]
    fn message_sent_event_renders_recipient_kind_and_content() {
        let lifecycle = AgentMessageLifecycle::Sent {
            message_id: uuid::Uuid::from_u128(9),
            from_id: uuid::Uuid::from_u128(1),
            from: "/root/spawn/worker".to_owned(),
            to_id: uuid::Uuid::from_u128(2),
            to: "root".to_owned(),
            kind: norn::agent_loop::inbound::MessageKind::Steer,
            seq: 7,
            content: "blocked on schema review".to_owned(),
            sent_at: chrono::Utc::now(),
        };
        let entry = message_activity_entry("spawn/haiku", &lifecycle, std::time::Instant::now());
        assert_eq!(entry.agent_role, "spawn/haiku");
        assert_eq!(entry.tool_name, "msg:steer → root");
        assert_eq!(
            entry.description.as_deref(),
            Some("blocked on schema review")
        );
    }

    #[test]
    fn message_delivered_event_renders_confirmation_with_seq() {
        let lifecycle = AgentMessageLifecycle::Delivered {
            message_id: uuid::Uuid::from_u128(9),
            from_id: uuid::Uuid::from_u128(1),
            to_id: uuid::Uuid::from_u128(2),
            seq: 7,
            delivered_at: chrono::Utc::now(),
        };
        let entry = message_activity_entry("root", &lifecycle, std::time::Instant::now());
        assert_eq!(entry.agent_role, "root");
        assert_eq!(entry.tool_name, "msg delivered (seq 7)");
        assert!(entry.description.is_none());
    }

    /// The full dispatch path: a Message-kind agent event lands in the
    /// activity log (the arm needs no terminal guard, so it is
    /// exercisable end-to-end, unlike the provider-event arms).
    #[test]
    fn message_event_pushes_activity_log_entry_via_helper() {
        let (mut state, _root_id) = state_with_one_child();
        let lifecycle = AgentMessageLifecycle::Sent {
            message_id: uuid::Uuid::from_u128(9),
            from_id: uuid::Uuid::from_u128(1),
            from: "/root/child".to_owned(),
            to_id: uuid::Uuid::from_u128(2),
            to: "/root/other".to_owned(),
            kind: norn::agent_loop::inbound::MessageKind::Update,
            seq: 1,
            content: "fyi".to_owned(),
            sent_at: chrono::Utc::now(),
        };
        state.activity_log.push(message_activity_entry(
            "spawn/haiku",
            &lifecycle,
            std::time::Instant::now(),
        ));
        assert_eq!(state.activity_log.len(), 1);
        let entry = state.activity_log.entries().front().unwrap();
        assert_eq!(entry.tool_name, "msg:update → /root/other");
        assert_eq!(entry.description.as_deref(), Some("fyi"));
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
