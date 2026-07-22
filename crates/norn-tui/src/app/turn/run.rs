//! Turn orchestration: seeding, the in-flight `select!` loop, follow-up and
//! pending-child prompt threading, and end-of-turn finalisation.

use std::time::Instant;

use termina::Event;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use norn::agent_loop::active_input_channel;
use norn::agent_loop::inbound::{ChannelMessage, InboundChannel};
use norn::agent_loop::runner::{
    AgentMessageStepRequest, AgentStepRequest, AgentStepResult, run_agent_step,
    run_agent_step_from_messages,
};

use crate::TuiError;
use crate::render::MarkdownRenderer;
use crate::render::fixed_panel::StreamingIndicator;
use crate::terminal::setup::TerminalGuard;

use crate::app::child_results::{recv_child_result, render_child_result_batch};
use crate::app::dispatch::{finalise_turn, write_error_line};
use crate::app::event_loop::{ChildResultState, RENDER_TICK, RuntimeRefs, is_ctrl_c};
use crate::app::helpers::{checkpoint_session, flush_pending};
use crate::app::render::{
    redraw_panel, redraw_streaming_tick, render_input, with_scroll_region_cursor,
    write_cancelled_line, write_user_message,
};
use crate::app::state::AppState;
use crate::app::streaming::finish_thinking_block;

use super::mid::{
    handle_active_input_delivery, handle_mid_turn_agent_event, handle_mid_turn_event,
};

enum TurnSeed {
    UserPrompt(String),
    AgentMessages(Vec<ChannelMessage>),
}

#[derive(Default)]
struct TurnOutcome {
    interrupt_prompt: Option<String>,
}

pub(crate) async fn run_turn_and_pending(
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

pub(crate) async fn run_ready_root_inbound(
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

pub(crate) async fn run_pending_child_prompts(
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

    // Prompt commands are evaluated by the library request builder. The TUI
    // must not pre-run them: uncached commands may have side effects, and a
    // driver-side pass would be discarded and then executed again.

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
                        // `&Arc<dyn ToolExecutor>` (not `.as_ref()`) so the
                        // loop's concurrent batch steps get an owned handle
                        // and spawn each batch member on its own task —
                        // matching `Agent::run` so the TUI and library paths
                        // share identical concurrent-batch semantics.
                        executor: &runtime.executor,
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
                        // `&Arc<dyn ToolExecutor>` (not `.as_ref()`) so the
                        // loop's concurrent batch steps get an owned handle
                        // and spawn each batch member on its own task —
                        // matching `Agent::run` so the TUI and library paths
                        // share identical concurrent-batch semantics.
                        executor: &runtime.executor,
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
