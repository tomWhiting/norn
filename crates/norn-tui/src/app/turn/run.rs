//! Turn orchestration: seeding, the in-flight `select!` loop, follow-up and
//! pending-child prompt threading, and end-of-turn finalisation.

use std::time::Instant;

use termina::Event;
use tokio::sync::{broadcast, mpsc};

use norn::agent_loop::active_input_channel;
use norn::agent_loop::inbound::{ChannelMessage, InboundChannel};
use norn::agent_loop::runner::{
    AgentMessageStepRequest, AgentStepRequest, AgentStepResult, run_agent_step,
    run_agent_step_from_messages,
};

use crate::TuiError;
use crate::render::streaming_indicator::StreamingIndicator;
use crate::terminal::setup::TerminalGuard;

use crate::app::child_results::{recv_child_result, render_child_result_batch};
use crate::app::dispatch::{channel_wake_pause_reason, finalise_turn, write_error_line};
use crate::app::event_loop::{
    ChildResultState, RENDER_TICK, RuntimeRefs, is_ctrl_c, turn_cancel_token,
};
use crate::app::helpers::checkpoint_session;
use crate::app::render::{load_visible, redraw_all, redraw_streaming_tick, write_user_message};
use crate::app::state::AppState;

use super::mid::{
    handle_active_input_delivery, handle_mid_turn_agent_event, handle_mid_turn_event,
};

enum TurnSeed {
    Operator(crate::app::transcript::publication::SubmittedInput),
    ChildResult(String),
    AgentMessages(Vec<ChannelMessage>),
    McpChannelWake,
}

#[derive(Default)]
struct TurnOutcome {
    interrupt_prompt: Option<String>,
    channel_wake_pause: Option<String>,
}

pub(crate) async fn run_turn_and_pending(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    input: crate::app::transcript::publication::SubmittedInput,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
    child_results: &mut ChildResultState,
) -> Result<(), TuiError> {
    let outcome = run_turn(
        state,
        runtime,
        guard,
        TurnSeed::Operator(input),
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
    .await?;
    Ok(())
}

/// Start from the channel owner's persisted input, without synthesizing a prompt.
/// The wake future only reports readiness; the agent loop retains delivery ownership.
pub(crate) async fn run_ready_mcp_channels(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
    child_results: &mut ChildResultState,
) -> Result<bool, TuiError> {
    let outcome = run_turn(
        state,
        runtime,
        guard,
        TurnSeed::McpChannelWake,
        term_rx,
        agent_event_rx,
        child_results,
    )
    .await?;
    let pause_reason = outcome.channel_wake_pause.clone();
    let operator_followup = run_followup_prompts(
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
    .await?;
    if !operator_followup && let Some(reason) = pause_reason {
        write_error_line(
            state,
            &format!(
                "Automatic channel wake paused: {reason}. Send an ordinary message to resume; retained input stays in the inbox."
            ),
        )?;
        return Ok(false);
    }
    Ok(true)
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
            TurnSeed::ChildResult(prompt),
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
) -> Result<bool, TuiError> {
    let mut ran_operator_turn = false;
    let mut next = first_outcome
        .interrupt_prompt
        .or_else(|| state.in_flight_input.pop_queued_followup());
    while let Some(prompt) = next {
        ran_operator_turn = true;
        let input = write_user_message(prompt, state)?;
        let outcome = run_turn(
            state,
            runtime,
            guard,
            TurnSeed::Operator(input),
            term_rx,
            agent_event_rx,
            child_results,
        )
        .await?;
        next = outcome
            .interrupt_prompt
            .or_else(|| state.in_flight_input.pop_queued_followup());
    }
    Ok(ran_operator_turn)
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
    let local_input = match &seed {
        TurnSeed::Operator(input) => Some(input.local.clone()),
        _ => None,
    };
    let event_sender = state.transcript.observe_execution(
        &runtime.root_event_sender,
        &runtime.store,
        &runtime.model_selection,
        local_input.clone(),
    )?;
    let observation = state.transcript.observation();
    crate::app::composer_submission::bind(state, local_input.as_ref(), observation.as_ref())?;
    state.turn_start = Some(Instant::now());
    state.in_flight_input.set_running(true);

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
    // Turn-local, but rooted: cancelling it ends this step only, while an
    // app-exit cancel of the root ends this step and every descendant's
    // run with it (D7).
    let cancel = turn_cancel_token(&runtime.root_cancel);
    let (active_input_tx, active_input_rx, mut active_delivery_rx) = active_input_channel();
    let mut active_delivery_closed = false;
    let mut terminal_closed = false;
    let mut events_closed = false;

    runtime.loop_context.active_input_rx = Some(active_input_rx);

    {
        let step_future = async {
            match &mut seed {
                TurnSeed::Operator(crate::app::transcript::publication::SubmittedInput {
                    text: prompt,
                    ..
                })
                | TurnSeed::ChildResult(prompt) => {
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
                        event_tx: Some(&event_sender),
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
                        event_tx: Some(&event_sender),
                        initial_messages,
                        inbound: runtime.root_inbound.as_mut(),
                        loop_context: &mut runtime.loop_context,
                        cancel: Some(cancel.clone()),
                    })
                    .await
                }
                TurnSeed::McpChannelWake => {
                    run_agent_step_from_messages(AgentMessageStepRequest {
                        provider: runtime.provider.as_ref(),
                        executor: &runtime.executor,
                        store: runtime.store.as_ref(),
                        tools: &tools,
                        output_schema: None,
                        model: &model,
                        config: &agent_config,
                        event_tx: Some(&event_sender),
                        initial_messages: Vec::new(),
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
                        handle_active_input_delivery(&delivery, state, &runtime.store)?;
                        redraw_all(state, guard)?;
                    } else {
                        active_delivery_closed = true;
                    }
                }
                msg = term_rx.recv(), if !terminal_closed => match msg {
                    Some(Ok(event)) => {
                        crate::app::composer_submission::resolve(state)?;
                        state.screen.terminal_event(term_rx.len());
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
                            load_visible(state, &runtime.store)?;
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
                        &mut child_results.rx,
                        &mut child_results.pending_prompts,
                        child_result,
                    )?;
                    redraw_all(state, guard)?;
                },
                event = agent_event_rx.recv(), if !events_closed => match event {
                    Ok(agent_ev) => {
                        handle_mid_turn_agent_event(state, agent_ev)?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        state.transcript.projection.mark_lagged(n)?;
                        tracing::warn!(missed = n, "agent event receiver lagged — {n} events dropped");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        events_closed = true;
                        crate::app::notices::notice(state, "Live event source closed during execution", None)?;
                    },
                },
                () = async { match &observation { Some(owner) => owner.changed().await, None => std::future::pending().await } } => {
                    state.transcript.drain_publications()?;
                    crate::app::composer_submission::resolve(state)?;
                    state.screen.allow_body_load = true;
                    redraw_all(state, guard)?;
                }
                Some(result) = state.transcript.input_tasks.join_next() => {
                    state.transcript.finish_input(result)?;
                    state.screen.allow_body_load = true;
                }
                result = crate::app::frontend_preferences::wait(&mut state.preferences) => { crate::app::frontend_preferences::finish(state, result)?; }
                Some(result) = state.export_tasks.join_next() => {
                    crate::app::view_actions::reading::finish_export(state, result)?;
                }
                Some(result) = state.screen.changes.jobs.join_next() => {
                    crate::app::render::changes::finish(state, result)?;
                }
                Some(result) = state.transcript.history_tasks.join_next() => {
                    crate::app::view_actions::reading::finish_history(state, result)?;
                }
                Some(result) = state.transcript.body_tasks.join_next() => {
                    state.transcript.finish_body(result)?;
                    state.screen.allow_body_load = true;
                    state.screen.dirty = true;
                }
                _ = tick.tick() => {
                    redraw_streaming_tick(state, guard, Instant::now())?;
                    load_visible(state, &runtime.store)?;
                    redraw_all(state, guard)?;
                }
            }
        }
    }

    loop {
        match agent_event_rx.try_recv() {
            Ok(agent_ev) => handle_mid_turn_agent_event(state, agent_ev)?,
            Err(broadcast::error::TryRecvError::Lagged(missed)) => {
                state.transcript.projection.mark_lagged(missed)?;
            }
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Closed) => {
                crate::app::notices::notice(state, "Live event source closed", None)?;
                break;
            }
        }
    }
    while let Some(delivery) = active_delivery_rx.try_recv() {
        handle_active_input_delivery(&delivery, state, &runtime.store)?;
    }
    state.transcript.drain_publications()?;
    crate::app::composer_submission::resolve(state)?;
    while let Some(result) = state.transcript.input_tasks.join_next().await {
        state.transcript.finish_input(result)?;
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
    loop {
        let page = crate::app::transcript::read_history(
            std::sync::Arc::clone(&runtime.store),
            state.transcript.newer_history()?,
        )
        .await?;
        if !state.transcript.accept_history(&page)? {
            return Err(norn::session_view::ViewError::AttemptMismatch.into());
        }
        if !state.transcript.has_newer {
            break;
        }
    }
    let interrupted = cancel_requested
        || !matches!(
            &step_result,
            Some(Ok(
                AgentStepResult::Completed { .. } | AgentStepResult::Refused { .. }
            ))
        );
    state.screen.allow_body_load = true;
    let channel_wake_pause = matches!(seed, TurnSeed::McpChannelWake)
        .then(|| channel_wake_pause_reason(step_result.as_ref(), cancel_requested))
        .flatten();
    finalise_turn(state, step_result)?;
    state.transcript.projection.end_execution(interrupted)?;
    if cancel_requested {
        state.streaming_indicator = StreamingIndicator::Idle;
        state.complete_at = None;
        state.sync_indicator_into_panel();
    }
    if let Some(message) = &checkpoint_failure {
        write_error_line(state, message)?;
    }
    redraw_all(state, guard)?;
    load_visible(state, &runtime.store)?;
    redraw_all(state, guard)?;
    Ok(TurnOutcome {
        interrupt_prompt,
        channel_wake_pause,
    })
}

fn reset_turn_state(state: &mut AppState) {
    state.turn_start = None;
    state.complete_at = None;
    state.streaming_indicator = StreamingIndicator::Idle;
    state.reset_live_usage();
}
