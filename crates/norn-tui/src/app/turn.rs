//! In-flight root turn execution for the TUI.

use std::io::Write as _;
use std::time::Instant;

use termina::Event;
use termina::Terminal as _;
use termina::event::KeyEventKind;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use norn::agent_loop::inbound::{ChannelMessage, InboundChannel};
use norn::agent_loop::runner::{
    AgentMessageStepRequest, AgentStepRequest, AgentStepResult, run_agent_step,
    run_agent_step_from_messages,
};
use norn::agent_loop::{
    ActiveInputDelivery, ActiveInputError, ActiveInputSender, active_input_channel,
};
use norn::provider::agent_event::{AgentEvent, AgentEventKind};
use norn::provider::events::ProviderEvent;

use crate::TuiError;
use crate::input::keybindings::{InputAction, map_key_event};
use crate::render::MarkdownRenderer;
use crate::render::fixed_panel::StreamingIndicator;
use crate::terminal::setup::TerminalGuard;

use super::active_input::InFlightSubmitMode;
use super::autocomplete::{PopupKeyOutcome, dismiss as dismiss_autocomplete, handle_popup_key};
use super::child_results::{recv_child_result, render_child_result_batch};
use super::dispatch::{finalise_turn, handle_agent_event, write_error_line};
use super::edit::apply_edit_action;
use super::event_loop::{
    ChildResultState, RENDER_TICK, RuntimeRefs, insert_paste_text, is_ctrl_c,
    sync_input_for_current_geometry,
};
use super::helpers::{checkpoint_session, flush_markdown, flush_pending};
use super::render::{
    park_input_cursor, redraw_panel, redraw_streaming_tick, render_input,
    with_scroll_region_cursor, write_cancelled_line, write_user_message,
};
use super::state::AppState;
use super::streaming::finish_thinking_block;

enum TurnSeed {
    UserPrompt(String),
    AgentMessages(Vec<ChannelMessage>),
}

#[derive(Default)]
struct TurnOutcome {
    interrupt_prompt: Option<String>,
}

pub(super) async fn run_turn_and_pending(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    user_prompt: &str,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
    child_results: &mut ChildResultState,
) -> Result<(), TuiError> {
    let outcome = run_turn(
        state,
        runtime,
        guard,
        TurnSeed::UserPrompt(user_prompt.to_string()),
        term_rx,
        agent_event_rx,
        child_results,
    )
    .await?;
    run_followup_prompts(
        state,
        runtime,
        guard,
        term_rx,
        agent_event_rx,
        child_results,
        outcome,
    )
    .await?;
    run_pending_child_prompts(
        state,
        runtime,
        guard,
        term_rx,
        agent_event_rx,
        child_results,
    )
    .await
}

pub(super) async fn run_ready_root_inbound(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
    child_results: &mut ChildResultState,
) -> Result<(), TuiError> {
    let Some(messages) = runtime
        .root_inbound
        .as_mut()
        .and_then(InboundChannel::drain_if_steer_ready)
    else {
        return Ok(());
    };

    let outcome = run_turn(
        state,
        runtime,
        guard,
        TurnSeed::AgentMessages(messages),
        term_rx,
        agent_event_rx,
        child_results,
    )
    .await?;
    run_followup_prompts(
        state,
        runtime,
        guard,
        term_rx,
        agent_event_rx,
        child_results,
        outcome,
    )
    .await
}

pub(super) async fn run_pending_child_prompts(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
    child_results: &mut ChildResultState,
) -> Result<(), TuiError> {
    while let Some(prompt) = child_results.pending_prompts.pop_front() {
        let outcome = run_turn(
            state,
            runtime,
            guard,
            TurnSeed::UserPrompt(prompt),
            term_rx,
            agent_event_rx,
            child_results,
        )
        .await?;
        run_followup_prompts(
            state,
            runtime,
            guard,
            term_rx,
            agent_event_rx,
            child_results,
            outcome,
        )
        .await?;
    }
    Ok(())
}

async fn run_followup_prompts(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
    child_results: &mut ChildResultState,
    first_outcome: TurnOutcome,
) -> Result<(), TuiError> {
    let mut next = first_outcome
        .interrupt_prompt
        .or_else(|| state.in_flight_input.pop_queued_followup());
    while let Some(prompt) = next {
        write_user_message(&prompt, state, guard)?;
        let outcome = run_turn(
            state,
            runtime,
            guard,
            TurnSeed::UserPrompt(prompt),
            term_rx,
            agent_event_rx,
            child_results,
        )
        .await?;
        next = outcome
            .interrupt_prompt
            .or_else(|| state.in_flight_input.pop_queued_followup());
    }
    Ok(())
}

/// Drive one agent turn from a submitted prompt.
///
/// `term_rx` is read from inside the inner `tokio::select!` so a Ctrl+C
/// keystroke can interrupt an in-flight agent step. Window resizes repaint the
/// fixed panel around the preserved scroll cursor, and child/fork final results
/// are rendered as soon as they arrive.
async fn run_turn(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    seed: TurnSeed,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
    child_results: &mut ChildResultState,
) -> Result<TurnOutcome, TuiError> {
    reset_turn_state(state);
    state.in_flight_input.set_running(true);
    let mut renderer: Option<MarkdownRenderer> = None;

    let model = runtime.model.clone();
    let agent_config = runtime.agent_config.clone();
    let tools = runtime.tools.clone();

    runtime.loop_context.clear_dynamic_sections();
    runtime
        .loop_context
        .evaluate_prompt_commands(agent_config.prompt_command_timeout)
        .await;

    let mut seed = seed;
    let mut tick = tokio::time::interval(RENDER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut step_result: Option<Result<AgentStepResult, norn::error::NornError>> = None;
    let mut cancel_requested = false;
    let cancel = CancellationToken::new();
    let (active_input_tx, active_input_rx, mut active_delivery_rx) = active_input_channel();
    let mut active_delivery_closed = false;
    let mut terminal_closed = false;

    runtime.loop_context.active_input_rx = Some(active_input_rx);

    {
        let step_future = async {
            match &mut seed {
                TurnSeed::UserPrompt(prompt) => {
                    run_agent_step(AgentStepRequest {
                        provider: runtime.provider.as_ref(),
                        executor: runtime.executor.as_ref(),
                        store: runtime.store.as_ref(),
                        user_prompt: prompt,
                        tools: &tools,
                        output_schema: None,
                        model: &model,
                        config: &agent_config,
                        event_tx: Some(&runtime.root_event_sender),
                        inbound: runtime.root_inbound.as_mut(),
                        loop_context: &mut runtime.loop_context,
                        cancel: Some(cancel.clone()),
                    })
                    .await
                }
                TurnSeed::AgentMessages(messages) => {
                    let initial_messages = std::mem::take(messages);
                    run_agent_step_from_messages(AgentMessageStepRequest {
                        provider: runtime.provider.as_ref(),
                        executor: runtime.executor.as_ref(),
                        store: runtime.store.as_ref(),
                        tools: &tools,
                        output_schema: None,
                        model: &model,
                        config: &agent_config,
                        event_tx: Some(&runtime.root_event_sender),
                        initial_messages,
                        inbound: runtime.root_inbound.as_mut(),
                        loop_context: &mut runtime.loop_context,
                        cancel: Some(cancel.clone()),
                    })
                    .await
                }
            }
        };
        tokio::pin!(step_future);
        while step_result.is_none() {
            tokio::select! {
                biased;
                res = &mut step_future => {
                    step_result = Some(res);
                }
                delivery = active_delivery_rx.recv(), if !active_delivery_closed => {
                    if let Some(delivery) = delivery {
                        handle_active_input_delivery(&delivery, state, guard, &mut renderer)?;
                        redraw_panel(state, guard)?;
                        render_input(state, guard)?;
                    } else {
                        active_delivery_closed = true;
                    }
                }
                msg = term_rx.recv(), if !terminal_closed => match msg {
                    Some(Ok(event)) => {
                        if is_ctrl_c(&event) {
                            cancel_requested = true;
                            cancel.cancel();
                        } else {
                            handle_mid_turn_event(
                                event,
                                state,
                                guard,
                                &active_input_tx,
                                &cancel,
                                &mut cancel_requested,
                            )?;
                        }
                    }
                    Some(Err(err)) => return Err(TuiError::Io(err)),
                    None => {
                        terminal_closed = true;
                        cancel_requested = true;
                        cancel.cancel();
                    }
                },
                Some(child_result) = recv_child_result(&mut child_results.rx) => {
                    render_child_result_batch(
                        state,
                        guard,
                        &mut child_results.rx,
                        &mut child_results.pending_prompts,
                        child_result,
                    )?;
                    redraw_panel(state, guard)?;
                    render_input(state, guard)?;
                },
                event = agent_event_rx.recv() => match event {
                    Ok(agent_ev) => {
                        handle_mid_turn_agent_event(state, guard, &mut renderer, agent_ev)?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "agent event receiver lagged — {n} events dropped");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = tick.tick() => {
                    redraw_streaming_tick(state, guard, renderer.as_ref(), Instant::now())?;
                }
            }
        }
    }

    while let Ok(agent_ev) = agent_event_rx.try_recv() {
        handle_mid_turn_agent_event(state, guard, &mut renderer, agent_ev)?;
    }
    while let Some(delivery) = active_delivery_rx.try_recv() {
        handle_active_input_delivery(&delivery, state, guard, &mut renderer)?;
    }
    let interrupt_prompt = state.in_flight_input.take_interrupt_prompt();
    if interrupt_prompt.is_none() && !cancel_requested {
        state.in_flight_input.requeue_pending_steers();
    }
    state.in_flight_input.set_running(false);
    runtime.loop_context.active_input_rx = None;

    if cancel_requested {
        norn::agent_loop::ensure_tool_results_complete(runtime.store.as_ref()).await;
    }
    // Checkpoint before the final render pass: every event of the turn is
    // already appended, and the off-executor await cannot run inside the
    // synchronous scroll-region closure below. A failure message is
    // carried into the closure and written in the error style there.
    let checkpoint_failure = checkpoint_session(&runtime.store).await;
    with_scroll_region_cursor(guard, |guard| {
        finish_thinking_block(state, guard, &mut renderer)?;
        flush_pending(state, guard, &mut renderer)?;
        finalise_turn(state, guard, step_result, &mut renderer)?;
        if cancel_requested {
            write_cancelled_line(guard)?;
            state.streaming_indicator = StreamingIndicator::Idle;
            state.complete_at = None;
            state.sync_indicator_into_panel();
        }
        if let Some(message) = &checkpoint_failure {
            write_error_line(state, guard, message)?;
        }
        Ok(())
    })?;
    redraw_panel(state, guard)?;
    Ok(TurnOutcome { interrupt_prompt })
}

fn reset_turn_state(state: &mut AppState) {
    state.pending_tools.clear();
    state.text_streamed_this_turn = false;
    state.last_was_tool_result = false;
    state.dim_wrapped_lines = 0;
    state.thinking_buffer.clear();
    state.styled_mid_line = false;
    state.turn_start = None;
    state.complete_at = None;
    state.streaming_indicator = StreamingIndicator::Idle;
    state.reset_live_usage();
}

fn handle_mid_turn_event(
    event: Event,
    state: &mut AppState,
    guard: &mut TerminalGuard,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    match event {
        Event::WindowResized(size) => {
            guard.restore_scroll_cursor_clamped()?;
            guard.save_scroll_cursor()?;
            guard.handle_resize(size.rows)?;
            sync_input_for_current_geometry(state, guard);
            redraw_panel(state, guard)?;
            guard.restore_scroll_cursor_clamped()?;
            guard.save_scroll_cursor()?;
            render_input(state, guard)?;
            guard.terminal_mut().flush()?;
        }
        Event::Paste(text) => {
            insert_paste_text(state, &text);
            sync_input_for_current_geometry(state, guard);
            redraw_panel(state, guard)?;
            render_input(state, guard)?;
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            handle_mid_turn_key(key, state, guard, active_input_tx, cancel, cancel_requested)?;
        }
        _ => {}
    }
    Ok(())
}

fn handle_mid_turn_agent_event(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
    agent_ev: AgentEvent,
) -> Result<(), TuiError> {
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let before_indicator_key = state.streaming_indicator.repaint_key(cols);
    let before_indicator_height = state.streaming_indicator.height();
    let structural_panel_change = agent_event_needs_panel_redraw(state, &agent_ev);

    guard.restore_scroll_cursor_clamped()?;
    handle_agent_event(state, guard, renderer, agent_ev)?;
    guard.save_scroll_cursor()?;

    let indicator_changed = before_indicator_height != state.streaming_indicator.height()
        || before_indicator_key != state.streaming_indicator.repaint_key(cols);
    if structural_panel_change || indicator_changed {
        redraw_panel(state, guard)?;
        render_input(state, guard)
    } else {
        park_input_cursor(state, guard)
    }
}

fn agent_event_needs_panel_redraw(state: &AppState, agent_ev: &AgentEvent) -> bool {
    match &agent_ev.event {
        AgentEventKind::Provider(event) if agent_ev.agent_id == state.tab_state.root_id() => {
            root_provider_event_needs_panel_redraw(event)
        }
        AgentEventKind::Provider(event) => child_provider_event_needs_panel_redraw(event),
        AgentEventKind::Subagent(_)
        | AgentEventKind::Message(_)
        | AgentEventKind::UsageEstimate(_) => true,
        // Consumed as a deliberate no-op in `handle_agent_event`.
        AgentEventKind::StreamRetry(_) => false,
    }
}

fn root_provider_event_needs_panel_redraw(event: &ProviderEvent) -> bool {
    !matches!(
        event,
        ProviderEvent::TextDelta { .. }
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::ToolCallDelta { .. }
            | ProviderEvent::TextComplete { .. }
            | ProviderEvent::ThinkingComplete { .. }
            | ProviderEvent::Compaction { .. }
    )
}

fn child_provider_event_needs_panel_redraw(event: &ProviderEvent) -> bool {
    !matches!(
        event,
        ProviderEvent::TextDelta { .. }
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::ToolCallDelta { name: None, .. }
            | ProviderEvent::TextComplete { .. }
            | ProviderEvent::ThinkingComplete { .. }
            | ProviderEvent::Compaction { .. }
    )
}

fn handle_mid_turn_key(
    key: termina::event::KeyEvent,
    state: &mut AppState,
    guard: &mut TerminalGuard,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    if state.autocomplete.is_some()
        && matches!(
            handle_popup_key(key, state, cols, guard.terminal_rows()),
            PopupKeyOutcome::Consumed
        )
    {
        return Ok(());
    }

    let caps = state.terminal_caps.clone();
    let popup_open = state.autocomplete.is_some();
    if let Some(action) = map_key_event(key, &caps, popup_open) {
        handle_mid_turn_action(
            action,
            state,
            guard,
            active_input_tx,
            cancel,
            cancel_requested,
        )?;
        sync_input_for_current_geometry(state, guard);
        redraw_panel(state, guard)?;
        render_input(state, guard)?;
    }
    Ok(())
}

fn handle_mid_turn_action(
    action: InputAction,
    state: &mut AppState,
    guard: &mut TerminalGuard,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    match action {
        InputAction::Submit => {
            submit_mid_turn_input(state, active_input_tx, cancel, cancel_requested)?;
        }
        InputAction::ToggleInFlightSubmitMode => state.in_flight_input.toggle_mode(),
        other => {
            let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
            apply_edit_action(other, state, cols, guard.terminal_rows());
        }
    }
    Ok(())
}

fn submit_mid_turn_input(
    state: &mut AppState,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    dismiss_autocomplete(state);
    let Some(text) = state.input_editor.submit()? else {
        if state.in_flight_input.has_pending_steers() {
            state.in_flight_input.request_interrupt_submit();
            *cancel_requested = true;
            cancel.cancel();
        }
        return Ok(());
    };

    match state.in_flight_input.mode() {
        InFlightSubmitMode::Steer => match active_input_tx.send_steer(text.clone()) {
            Ok(id) => state.in_flight_input.push_pending_steer(id, text),
            Err(ActiveInputError::Closed) => state.in_flight_input.queue_followup(text),
            Err(ActiveInputError::Empty) => {}
        },
        InFlightSubmitMode::Queue => state.in_flight_input.queue_followup(text),
    }
    Ok(())
}

fn handle_active_input_delivery(
    delivery: &ActiveInputDelivery,
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
) -> Result<(), TuiError> {
    state.in_flight_input.mark_steer_delivered(delivery.id);
    finish_thinking_block(state, guard, renderer)?;
    flush_markdown(state, guard, renderer)?;
    write_user_message(&delivery.content, state, guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn::provider::request::ToolCallKind;

    #[test]
    fn root_text_deltas_do_not_force_panel_redraw() {
        let event = ProviderEvent::TextDelta {
            text: "hello".to_string(),
        };

        assert!(!root_provider_event_needs_panel_redraw(&event));
    }

    #[test]
    fn root_tool_completion_forces_panel_redraw() {
        let event = ProviderEvent::ToolCallComplete {
            call_id: "call_1".to_string(),
            name: "bash".to_string(),
            arguments: "{}".to_string(),
            kind: ToolCallKind::Function,
        };

        assert!(root_provider_event_needs_panel_redraw(&event));
    }

    #[test]
    fn child_nameless_tool_delta_does_not_force_panel_redraw() {
        let event = ProviderEvent::ToolCallDelta {
            item_id: "item_1".to_string(),
            name: None,
            arguments_delta: "{}".to_string(),
            kind: ToolCallKind::Function,
        };

        assert!(!child_provider_event_needs_panel_redraw(&event));
    }

    #[test]
    fn child_named_tool_delta_forces_panel_redraw() {
        let event = ProviderEvent::ToolCallDelta {
            item_id: "item_1".to_string(),
            name: Some("read".to_string()),
            arguments_delta: "{}".to_string(),
            kind: ToolCallKind::Function,
        };

        assert!(child_provider_event_needs_panel_redraw(&event));
    }
}
