//! In-flight root turn execution for the TUI.

use std::io::Write as _;
use std::time::Instant;

use termina::Event;
use termina::Terminal as _;
use termina::event::KeyEventKind;
use tokio::sync::{broadcast, mpsc};

use norn::agent_loop::inbound::{ChannelMessage, InboundChannel};
use norn::agent_loop::runner::{
    AgentMessageStepRequest, AgentStepRequest, AgentStepResult, run_agent_step,
    run_agent_step_from_messages,
};

use crate::TuiError;
use crate::input::keybindings::map_key_event;
use crate::render::MarkdownRenderer;
use crate::render::fixed_panel::StreamingIndicator;
use crate::terminal::setup::TerminalGuard;

use super::autocomplete::{PopupKeyOutcome, handle_popup_key};
use super::child_results::{recv_child_result, render_child_result_batch};
use super::dispatch::{finalise_turn, handle_agent_event, write_error_line};
use super::edit::apply_edit_action;
use super::event_loop::{
    ChildResultState, RENDER_TICK, RuntimeRefs, insert_paste_text, is_ctrl_c,
    sync_input_for_current_geometry,
};
use super::helpers::{checkpoint_session, flush_pending};
use super::render::{redraw_panel, redraw_streaming_tick, render_input, write_cancelled_line};
use super::state::AppState;
use super::streaming::finish_thinking_block;

enum TurnSeed {
    UserPrompt(String),
    AgentMessages(Vec<ChannelMessage>),
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
    run_turn(
        state,
        runtime,
        guard,
        TurnSeed::UserPrompt(user_prompt.to_string()),
        term_rx,
        agent_event_rx,
        child_results,
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

    run_turn(
        state,
        runtime,
        guard,
        TurnSeed::AgentMessages(messages),
        term_rx,
        agent_event_rx,
        child_results,
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
        run_turn(
            state,
            runtime,
            guard,
            TurnSeed::UserPrompt(prompt),
            term_rx,
            agent_event_rx,
            child_results,
        )
        .await?;
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
) -> Result<(), TuiError> {
    reset_turn_state(state);
    let mut renderer: Option<MarkdownRenderer> = None;

    let model = runtime.model.clone();
    let agent_config = runtime.agent_config.clone();
    let tools = runtime.tools.clone();

    runtime.loop_context.clear_dynamic_sections();
    runtime.loop_context.evaluate_prompt_commands().await;

    let mut seed = seed;
    let mut tick = tokio::time::interval(RENDER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut step_result: Option<Result<AgentStepResult, norn::error::NornError>> = None;
    let mut cancelled = false;

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
                        cancel: None,
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
                        cancel: None,
                    })
                    .await
                }
            }
        };
        tokio::pin!(step_future);
        while step_result.is_none() && !cancelled {
            tokio::select! {
                biased;
                res = &mut step_future => {
                    step_result = Some(res);
                }
                msg = term_rx.recv() => match msg {
                    Some(Ok(event)) => {
                        if is_ctrl_c(&event) {
                            cancelled = true;
                        } else {
                            handle_mid_turn_event(event, state, guard)?;
                        }
                    }
                    Some(Err(err)) => return Err(TuiError::Io(err)),
                    None => return Ok(()),
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
                        handle_agent_event(state, guard, &mut renderer, agent_ev)?;
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
        handle_agent_event(state, guard, &mut renderer, agent_ev)?;
    }
    if cancelled {
        norn::agent_loop::ensure_tool_results_complete(runtime.store.as_ref()).await;
    }
    finish_thinking_block(state, guard, &mut renderer)?;
    flush_pending(state, guard, &mut renderer)?;
    finalise_turn(state, guard, step_result, &mut renderer)?;
    if cancelled {
        write_cancelled_line(guard)?;
        state.streaming_indicator = StreamingIndicator::Idle;
        state.complete_at = None;
        state.sync_indicator_into_panel();
    }
    if let Some(message) = checkpoint_session(runtime.store.as_ref()) {
        write_error_line(state, guard, &message)?;
    }
    guard.save_scroll_cursor()?;
    redraw_panel(state, guard)?;
    Ok(())
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
}

fn handle_mid_turn_event(
    event: Event,
    state: &mut AppState,
    guard: &mut TerminalGuard,
) -> Result<(), TuiError> {
    match event {
        Event::WindowResized(size) => {
            guard.save_scroll_cursor()?;
            guard.handle_resize(size.rows)?;
            sync_input_for_current_geometry(state, guard);
            redraw_panel(state, guard)?;
            render_input(state, guard)?;
            guard.restore_scroll_cursor_clamped()?;
            guard.terminal_mut().flush()?;
        }
        Event::Paste(text) => {
            insert_paste_text(state, &text);
            sync_input_for_current_geometry(state, guard);
            redraw_panel(state, guard)?;
            render_input(state, guard)?;
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            handle_mid_turn_key(key, state, guard);
        }
        _ => {}
    }
    Ok(())
}

fn handle_mid_turn_key(
    key: termina::event::KeyEvent,
    state: &mut AppState,
    guard: &mut TerminalGuard,
) {
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    if state.autocomplete.is_some()
        && matches!(
            handle_popup_key(key, state, cols, guard.terminal_rows()),
            PopupKeyOutcome::Consumed
        )
    {
        return;
    }

    let caps = state.terminal_caps.clone();
    let popup_open = state.autocomplete.is_some();
    if let Some(action) = map_key_event(key, &caps, popup_open) {
        let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
        apply_edit_action(action, state, cols, guard.terminal_rows());
    }
}
