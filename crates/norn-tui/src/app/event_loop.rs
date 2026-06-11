//! TUI event loop and `ProviderEvent` dispatch.
//!
//! [`run_app`] is the higher-level entry point that takes [`TuiInputs`]
//! (already-constructed runtime objects from norn-cli) and drives the
//! main `tokio::select!` loop.
//!
//! Note on the brief's R9 dependency direction: the literal
//! `run_tui(cli: &Cli) -> ExitCode` from the brief cannot live in
//! `norn-tui` because [`norn_cli::cli::Cli`] and
//! [`norn_cli::cli::ExitCode`] are types in the `norn-cli` crate, and
//! `norn-cli` already depends on `norn-tui` (one direction). Putting
//! `Cli`/`ExitCode` into `norn-tui` would be a circular dependency. The
//! practical resolution: [`run_app`] is the highest-level primitive in
//! `norn-tui` taking pre-built runtime objects, and the actual
//! `&Cli → ExitCode` entry point lives in `norn-cli/src/tui/driver.rs`
//! which dispatches into [`run_app`].

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use termina::Terminal as _;
use termina::event::{KeyCode, KeyEventKind, Modifiers};
use termina::{Event, EventReader};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use norn::agent::registry::AgentRegistry;
use norn::r#loop::config::AgentLoopConfig;
use norn::r#loop::loop_context::LoopContext;
use norn::r#loop::runner::{AgentStepRequest, AgentStepResult, ToolExecutor, run_agent_step};
use norn::provider::request::ToolDefinition;
use norn::provider::traits::Provider;
use norn::session::store::EventStore;

use crate::TuiError;
use crate::input::history::InputHistory;
use crate::input::keybindings::{InputAction, map_key_event};
use crate::render::MarkdownRenderer;
use crate::render::fixed_panel::{StatusBar, StreamingIndicator};
use crate::terminal::caps::TerminalCaps;
use crate::terminal::setup::TerminalGuard;

use super::autocomplete::{PopupKeyOutcome, dismiss as dismiss_autocomplete, handle_popup_key};
use super::dispatch::{finalise_turn, handle_agent_event, write_error_line};
use super::edit::apply_edit_action;
use super::helpers::{checkpoint_session, flush_pending};
use super::render::{
    redraw_all, redraw_panel, render_input, sync_input_area, write_cancelled_line,
    write_user_message,
};
use super::slash::{SlashOutcome, try_dispatch_slash};
use super::state::AppState;

/// Bundled runtime inputs needed by [`run_app`].
///
/// Keeps the function signature inside the
/// `clippy::too_many_arguments` budget and isolates the TUI from the
/// norn-cli crate's concrete builder types.
pub struct TuiInputs {
    /// Concrete provider built by `norn-cli::print::build_provider`.
    pub provider: Arc<dyn Provider>,
    /// Tool executor (the gated `ToolRegistry` from `RuntimeBundle`).
    pub executor: Arc<dyn ToolExecutor>,
    /// Session event store.
    pub store: Arc<EventStore>,
    /// Shared agent registry — read by the agent status panel.
    pub registry: Arc<RwLock<AgentRegistry>>,
    /// Loop context with system sections, rules, hooks, event schemas.
    pub loop_context: LoopContext,
    /// Agent-loop configuration.
    pub agent_config: AgentLoopConfig,
    /// Model identifier.
    pub model: String,
    /// Tool definitions advertised to the provider.
    pub tools: Vec<ToolDefinition>,
    /// Input history (already loaded from disk by the caller).
    pub history: InputHistory,
    /// Status bar with the model name and session name prefilled.
    pub status_bar: StatusBar,
    /// Root agent id — the registry id of the top-level agent.
    pub root_id: Uuid,
    /// Optional initial user prompt to submit on startup.
    pub initial_prompt: Option<String>,
    /// Session data directory for persistence. When `None`, session
    /// events are kept in memory only (ephemeral / `--no-session` mode).
    pub data_dir: Option<std::path::PathBuf>,
    /// Session identifier used as the JSONL file stem. Paired with
    /// `data_dir` to locate the persistence file.
    pub session_id: Option<String>,
    /// Root agent's event sender — used by `run_turn` to tag root
    /// events on the shared broadcast channel.
    pub root_event_sender: norn::provider::agent_event::AgentEventSender,
    /// Persistent receiver for agent events from all agents (root +
    /// children). Lives across turns so child events arriving between
    /// turns are not lost.
    pub agent_event_rx: broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
}

/// Render-tick cadence — 120 fps for tear-free panel redraws and
/// immediate input painting during streaming.
const RENDER_TICK: Duration = Duration::from_millis(8);

/// Drive the TUI to completion.
///
/// Sets up the terminal, constructs [`AppState`], and enters the main
/// `tokio::select!` loop. Returns on Ctrl+C with an empty input buffer
/// or on a fatal terminal I/O error.
///
/// # Errors
///
/// Returns [`TuiError::Io`] on terminal I/O errors and
/// [`TuiError::UnsupportedTerminal`] if the terminal cannot meet hard
/// requirements during capability detection.
pub async fn run_app(inputs: TuiInputs) -> Result<(), TuiError> {
    TerminalCaps::check_hard_requirements()?;
    let mut guard = TerminalGuard::new()?;
    // Clear screen, home the cursor, and save the initial scroll-region
    // position via the guard's clamping save helper. The DECSC slot is
    // the single source of truth for "where should the next
    // write_to_scroll go" — only updated when the cursor is known to
    // be inside the scroll region. The guard also tracks the row in
    // software so DECRC restores after a panel grow get clamped back
    // into the scroll region instead of writing into the panel area.
    write!(guard.terminal_mut(), "\x1b[2J\x1b[H")?;
    guard.terminal_mut().flush()?;
    guard.reset_scroll_cursor(1);
    guard.save_scroll_cursor()?;
    guard.terminal_mut().flush()?;

    let caps = guard.caps().clone();
    let mut state = AppState::new(
        caps,
        inputs.history,
        Arc::clone(&inputs.registry),
        inputs.root_id,
        inputs.status_bar,
    );

    redraw_panel(&mut state, &mut guard)?;
    render_input(&state, &mut guard)?;

    // Spawn the terminal-event reader thread up front so the initial
    // prompt path (below) can observe Ctrl+C just like the outer-loop
    // path. The reader is owned by run_app, not outer_loop — moving it
    // here is what makes mid-turn cancellation possible.
    let event_reader = guard.terminal_mut().event_reader();
    let mut term_rx = spawn_event_reader(event_reader);

    let mut agent_event_rx = inputs.agent_event_rx;

    let mut runtime = RuntimeRefs {
        provider: inputs.provider,
        executor: inputs.executor,
        store: inputs.store,
        loop_context: inputs.loop_context,
        agent_config: inputs.agent_config,
        model: inputs.model,
        tools: inputs.tools,
        data_dir: inputs.data_dir,
        session_id: inputs.session_id,
        root_event_sender: inputs.root_event_sender,
    };

    if let Some(prompt) = inputs.initial_prompt
        && !prompt.trim().is_empty()
    {
        let trimmed = prompt.trim().to_string();
        write_user_message(&trimmed, &mut state, &mut guard)?;
        run_turn(
            &mut state,
            &mut runtime,
            &mut guard,
            &trimmed,
            &mut term_rx,
            &mut agent_event_rx,
        )
        .await?;
        redraw_panel(&mut state, &mut guard)?;
        render_input(&state, &mut guard)?;
    }

    // Take the child-result receiver from LoopContext so the outer loop
    // can deliver fork/spawn completions between turns. The runner's
    // drain_child_results guards with `if let Some(rx)` and handles
    // None gracefully — mid-turn results will be queued until the turn
    // ends and the outer loop picks them up as visible user messages.
    let child_rx = runtime.loop_context.child_result_rx.take();

    outer_loop(
        &mut state,
        &mut runtime,
        &mut guard,
        term_rx,
        child_rx,
        &mut agent_event_rx,
    )
    .await
}

/// Spawn the dedicated OS thread that reads terminal events.
///
/// [`EventReader::read`] blocks the calling thread, so it cannot run
/// inside the tokio runtime. The thread forwards each event onto an
/// unbounded mpsc channel; the returned receiver is the single source
/// of terminal events for both the outer loop and the in-flight turn
/// (Ctrl+C interrupt path).
fn spawn_event_reader(
    event_reader: EventReader,
) -> mpsc::UnboundedReceiver<std::io::Result<Event>> {
    let (term_tx, term_rx) = mpsc::unbounded_channel::<std::io::Result<Event>>();
    std::thread::spawn(move || {
        loop {
            match event_reader.read(|_| true) {
                Ok(event) => {
                    if term_tx.send(Ok(event)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = term_tx.send(Err(err));
                    break;
                }
            }
        }
    });
    term_rx
}

/// Runtime references threaded through the turn helper.
///
/// `pub(super)` so [`super::slash`] can read and mutate fields when
/// dispatching slash commands that touch runtime state (`/clear` swaps
/// the store, `/compact` mutates `loop_context.context_edits`,
/// `/model` mutates the model name).
pub(super) struct RuntimeRefs {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) executor: Arc<dyn ToolExecutor>,
    pub(super) store: Arc<EventStore>,
    pub(super) loop_context: LoopContext,
    pub(super) agent_config: AgentLoopConfig,
    pub(super) model: String,
    pub(super) tools: Vec<ToolDefinition>,
    /// Session data directory for persistence. `None` in ephemeral mode.
    pub(super) data_dir: Option<std::path::PathBuf>,
    /// Current session identifier. Updated on `/new` rotation.
    pub(super) session_id: Option<String>,
    /// Root agent's event sender — passed to `run_agent_step` so the
    /// root's `ProviderEvent` values are tagged and broadcast.
    pub(super) root_event_sender: norn::provider::agent_event::AgentEventSender,
}

/// Outer loop — input dispatch + render ticks between turns.
///
/// The channel is created and the reader thread is spawned in
/// [`run_app`]; this loop takes ownership of the receiver and threads
/// `&mut term_rx` down into [`run_turn`] so Ctrl+C interrupts mid-turn.
async fn outer_loop(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    mut term_rx: mpsc::UnboundedReceiver<std::io::Result<Event>>,
    mut child_rx: Option<
        tokio::sync::mpsc::Receiver<norn::agent::result_channel::ChildAgentResult>,
    >,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
) -> Result<(), TuiError> {
    let mut tick = tokio::time::interval(RENDER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            msg = term_rx.recv() => {
                let Some(result) = msg else { return Ok(()); };
                let event = result.map_err(TuiError::Io)?;
                match dispatch_input(event, state, runtime, guard, &mut term_rx, agent_event_rx).await? {
                    InputOutcome::Continue => {}
                    InputOutcome::Exit => return Ok(()),
                }
            }
            Some(child_result) = async {
                match child_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending::<Option<norn::agent::result_channel::ChildAgentResult>>().await,
                }
            } => {
                let mut batch = vec![child_result];
                if let Some(rx) = child_rx.as_mut() {
                    while let Ok(r) = rx.try_recv() {
                        batch.push(r);
                    }
                }
                let (display, prompt) = format_child_result_batch(&batch);
                write_user_message(&display, state, guard)?;
                run_turn(state, runtime, guard, &prompt, &mut term_rx, agent_event_rx).await?;
                redraw_panel(state, guard)?;
                render_input(state, guard)?;
            }
            event = agent_event_rx.recv() => {
                if let Ok(agent_ev) = event {
                    handle_agent_event(state, guard, &mut None, agent_ev)?;
                    redraw_panel(state, guard)?;
                    render_input(state, guard)?;
                }
            }
            _ = tick.tick() => {
                state.tick(Instant::now());
                if !matches!(state.streaming_indicator, StreamingIndicator::Idle) {
                    state.sync_indicator_into_panel();
                    redraw_panel(state, guard)?;
                    render_input(state, guard)?;
                }
            }
        }
    }
}

/// Result of dispatching an outer-loop terminal event.
enum InputOutcome {
    /// Keep looping.
    Continue,
    /// Exit cleanly.
    Exit,
}

/// Map a terminal event to the appropriate handler.
///
/// `term_rx` is forwarded into [`handle_action`] so that the
/// [`InputAction::Submit`] path can give [`run_turn`] live access to
/// incoming terminal events for Ctrl+C interrupt handling.
///
/// When the autocomplete popup is open, a small set of keys is
/// pre-intercepted before [`map_key_event`] runs:
///
/// - `Up`/`Down` navigate the popup instead of history (`map_key_event`
///   already returns `None` for these when `popup_open` is set).
/// - `Tab` or bare `Enter` accept the highlighted candidate and splice
///   it into the editor.
/// - `Escape` dismisses the popup without clearing the input.
///
/// All other keys fall through to the normal action pipeline, and the
/// popup state is refreshed after the action — which keeps the popup
/// narrowed against the typed prefix, replaced when the trigger
/// changes, or dismissed when no trigger is active.
async fn dispatch_input(
    event: Event,
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
) -> Result<InputOutcome, TuiError> {
    match event {
        Event::Key(key) => {
            let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
            if state.autocomplete.is_some()
                && key.kind == KeyEventKind::Press
                && matches!(
                    handle_popup_key(key, state, cols, guard.terminal_rows()),
                    PopupKeyOutcome::Consumed
                )
            {
                redraw_all(state, guard)?;
                return Ok(InputOutcome::Continue);
            }
            let caps = state.terminal_caps.clone();
            let popup_open = state.autocomplete.is_some();
            let Some(action) = map_key_event(key, &caps, popup_open) else {
                return Ok(InputOutcome::Continue);
            };
            handle_action(action, state, runtime, guard, term_rx, agent_event_rx).await
        }
        Event::Paste(text) => {
            insert_paste_text(state, &text);
            sync_input_for_current_geometry(state, guard);
            redraw_panel(state, guard)?;
            render_input(state, guard)?;
            Ok(InputOutcome::Continue)
        }
        Event::WindowResized(size) => {
            guard.handle_resize(size.rows)?;
            sync_input_for_current_geometry(state, guard);
            redraw_panel(state, guard)?;
            render_input(state, guard)?;
            Ok(InputOutcome::Continue)
        }
        _ => Ok(InputOutcome::Continue),
    }
}

/// Apply an [`InputAction`] to the state and trigger any side effects.
///
/// `term_rx` is only consumed by the [`InputAction::Submit`] arm, where
/// it is forwarded into [`run_turn`] so a mid-turn Ctrl+C key event can
/// abort the in-flight agent step.
fn sync_input_for_current_geometry(state: &mut AppState, guard: &mut TerminalGuard) {
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let input_rows = sync_input_area(&mut state.input_editor, cols, guard.terminal_rows());
    state.fixed_panel.set_input_area(input_rows);
}

fn insert_paste_text(state: &mut AppState, text: &str) {
    dismiss_autocomplete(state);
    for ch in text.chars() {
        if ch == '\n' {
            state.input_editor.insert_newline();
        } else {
            state.input_editor.insert_char(ch);
        }
    }
}

async fn handle_action(
    action: InputAction,
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
) -> Result<InputOutcome, TuiError> {
    match action {
        InputAction::Exit => {
            if state.input_editor.is_empty() {
                return Ok(InputOutcome::Exit);
            }
            state.input_editor.clear();
            dismiss_autocomplete(state);
        }
        InputAction::Submit => {
            dismiss_autocomplete(state);
            let text = state.input_editor.submit()?.unwrap_or_default();
            if text.trim().is_empty() {
                return Ok(InputOutcome::Continue);
            }
            // Phase 1 slash dispatch. None = not a recognised slash —
            // fall through to the normal run_turn pipeline. Some =
            // handled here; skip run_turn. Exit short-circuits the
            // outer loop directly.
            match try_dispatch_slash(&text, state, runtime, guard)? {
                Some(SlashOutcome::Exit) => return Ok(InputOutcome::Exit),
                Some(SlashOutcome::Continue) => {}
                None => {
                    write_user_message(&text, state, guard)?;
                    run_turn(state, runtime, guard, &text, term_rx, agent_event_rx).await?;
                }
            }
        }
        other => {
            let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
            apply_edit_action(other, state, cols, guard.terminal_rows());
        }
    }
    sync_input_for_current_geometry(state, guard);
    redraw_all(state, guard)?;
    Ok(InputOutcome::Continue)
}

/// Drive one agent turn from a submitted prompt.
///
/// `term_rx` is read from inside the inner `tokio::select!` so a
/// Ctrl+C keystroke can interrupt an in-flight agent step. A
/// `WindowResized` event arriving mid-turn is also honoured — it
/// reissues DECSTBM and repaints the panel so the geometry tracks
/// the new dimensions. Any other terminal event is dropped silently
/// (the input area is not interactive while the agent runs).
///
/// On cancel the step future is dropped (which propagates tokio
/// cancellation into the provider's HTTP request), the broadcast
/// senders are closed, any buffered provider events are drained, the
/// renderer is finalised, a dim `[cancelled]` indicator is written
/// into the scroll region, and the streaming indicator is reset.
async fn run_turn(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    user_prompt: &str,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
) -> Result<(), TuiError> {
    state.pending_tools.clear();
    state.text_streamed_this_turn = false;
    state.last_was_tool_result = false;
    state.dim_wrapped_lines = 0;
    state.thinking_buffer.clear();
    state.styled_mid_line = false;
    state.turn_start = None;
    state.complete_at = None;
    state.streaming_indicator = StreamingIndicator::Idle;
    let mut renderer: Option<MarkdownRenderer> = None;

    let model = runtime.model.clone();
    let agent_config = runtime.agent_config.clone();
    let tools = runtime.tools.clone();

    runtime.loop_context.clear_dynamic_sections();
    runtime.loop_context.evaluate_prompt_commands().await;

    let prompt = user_prompt.to_string();

    let mut tick = tokio::time::interval(RENDER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut step_result: Option<Result<AgentStepResult, norn::error::NornError>> = None;
    let mut cancelled = false;

    {
        let step_future = run_agent_step(AgentStepRequest {
            provider: runtime.provider.as_ref(),
            executor: runtime.executor.as_ref(),
            store: runtime.store.as_ref(),
            user_prompt: &prompt,
            tools: &tools,
            output_schema: None,
            model: &model,
            config: &agent_config,
            event_tx: Some(&runtime.root_event_sender),
            inbound: None,
            loop_context: &mut runtime.loop_context,
            cancel: None,
        });
        tokio::pin!(step_future);
        while step_result.is_none() && !cancelled {
            tokio::select! {
                // Bias order matters here:
                //   1. step_future — observe natural completion ASAP.
                //   2. term_rx     — Ctrl+C and WindowResized MUST NOT
                //                    be starved by a fast TextDelta
                //                    storm on the broadcast channel
                //                    below. Putting `term_rx` above
                //                    `rx` guarantees the keystroke and
                //                    structural events are picked up
                //                    on the very next select round.
                //   3. rx          — broadcast provider events.
                //   4. tick        — render cadence.
                biased;
                res = &mut step_future => {
                    step_result = Some(res);
                }
                msg = term_rx.recv() => match msg {
                    Some(Ok(event)) => {
                        if is_ctrl_c(&event) {
                            cancelled = true;
                        } else if let Event::WindowResized(size) = event {
                            // Resize is structural — DECSTBM and the
                            // panel must follow the new dimensions or
                            // every subsequent paint lands at stale
                            // rows. Same clamping save/restore dance
                            // as the tick arm so the scroll-region
                            // cursor survives the panel redraw — and,
                            // if the panel grew, gets clamped back
                            // into the (now smaller) scroll region
                            // instead of being parked over the panel.
                            guard.save_scroll_cursor()?;
                            guard.handle_resize(size.rows)?;
                            sync_input_for_current_geometry(state, guard);
                            redraw_panel(state, guard)?;
                            render_input(state, guard)?;
                            guard.restore_scroll_cursor_clamped()?;
                            guard.terminal_mut().flush()?;
                        } else if let Event::Paste(text) = event {
                            insert_paste_text(state, &text);
                            sync_input_for_current_geometry(state, guard);
                            redraw_panel(state, guard)?;
                            render_input(state, guard)?;
                        } else if let Event::Key(key) = event
                            && key.kind == KeyEventKind::Press
                        {
                            // Allow editing while the agent runs so
                            // the user can compose their next prompt
                            // during output streaming. The next tick
                            // repaints the input area with the changes.
                            let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
                            if state.autocomplete.is_some()
                                && matches!(
                                    handle_popup_key(key, state, cols, guard.terminal_rows()),
                                    PopupKeyOutcome::Consumed
                                )
                            {
                                // Popup key handled.
                            } else {
                                let caps = state.terminal_caps.clone();
                                let popup_open = state.autocomplete.is_some();
                                if let Some(action) = map_key_event(key, &caps, popup_open) {
                                    let cols = guard
                                        .terminal_mut()
                                        .get_dimensions()
                                        .map_or(80, |d| d.cols);
                                    apply_edit_action(action, state, cols, guard.terminal_rows());
                                }
                            }
                        }
                    }
                    Some(Err(err)) => return Err(TuiError::Io(err)),
                    None => return Ok(()),
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
                    super::render::redraw_streaming_tick(state, guard, renderer.as_ref(), Instant::now())?;
                }
            }
        }
    }

    // step_future has dropped — drain any buffered events that arrived
    // between the last select! round and the future completing. On
    // cancel, dropping the future propagates tokio's cancellation into
    // the provider HTTP request.
    while let Ok(agent_ev) = agent_event_rx.try_recv() {
        handle_agent_event(state, guard, &mut renderer, agent_ev)?;
    }
    if cancelled {
        norn::r#loop::ensure_tool_results_complete(runtime.store.as_ref()).await;
    }
    flush_pending(state, guard, &mut renderer)?;
    finalise_turn(state, guard, step_result, &mut renderer)?;
    if cancelled {
        write_cancelled_line(guard)?;
        state.streaming_indicator = StreamingIndicator::Idle;
        state.complete_at = None;
        state.sync_indicator_into_panel();
    }
    // Turn boundary: flush the store's index-registered sink so the
    // session index entry (event count, usage, updated_at) tracks this
    // turn instead of going stale until clean shutdown — an abort would
    // lose the pending delta entirely. Failure is surfaced in the red
    // error-line style (and logged inside the helper) but never aborts
    // the turn: the on-screen conversation and the write-through JSONL
    // event file are both intact.
    if let Some(message) = checkpoint_session(runtime.store.as_ref()) {
        write_error_line(state, guard, &message)?;
    }
    // Save scroll position (cursor is in scroll region after all writes).
    // The save persists into the next outer-loop iteration so the next
    // user submission's write_user_message restores to this same row.
    guard.save_scroll_cursor()?;
    redraw_panel(state, guard)?;
    Ok(())
}

/// Detect Ctrl+C on a terminal [`Event`].
///
/// Mirrors the keybindings module's Ctrl+C handling
/// ([`map_key_event`](crate::input::keybindings::map_key_event)) —
/// only [`KeyEventKind::Press`] counts, so a key release never
/// triggers cancellation. Inlined here rather than going through
/// [`map_key_event`] so the cancel path doesn't depend on the broader
/// [`InputAction`] enum or popup-state argument.
fn is_ctrl_c(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('c')
                && key.modifiers.contains(Modifiers::CONTROL),
    )
}

/// Build the display text (for the scroll region) and the model prompt
/// (for the API) from a batch of child agent results.
///
/// The display text is a short summary the user sees. The model prompt
/// includes the full `[System: ...]` envelope so the model can
/// distinguish auto-delivered results from user input.
fn format_child_result_batch(
    batch: &[norn::agent::result_channel::ChildAgentResult],
) -> (String, String) {
    if batch.len() == 1 {
        let r = &batch[0];
        let display = format!("[{} completed]", r.agent_role);
        (display, r.formatted_message.clone())
    } else {
        let display = format!("[{} agents completed]", batch.len());
        let mut prompt = format!("Results from {} completed agents:\n\n", batch.len());
        for r in batch {
            prompt.push_str(&r.formatted_message);
            prompt.push('\n');
        }
        (display, prompt)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::input::autocomplete::{AutocompletePopup, SlashCandidate, SourceTag};
    use crate::input::history::InputHistory;
    use crate::render::fixed_panel::StatusBar;
    use crate::terminal::caps::TerminalCaps;
    use crate::tools::VerbosityState;
    use norn::agent::registry::AgentRegistry;

    fn fresh_state() -> AppState {
        let registry = AgentRegistry::shared();
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

    fn seed_popup(state: &mut AppState) {
        let candidates = vec![SlashCandidate {
            name: "help".to_owned(),
            source_tag: SourceTag::Builtin,
            description: "Show help".to_owned(),
        }];
        state.autocomplete = Some(AutocompletePopup::new_slash(candidates, "", 0));
        state.fixed_panel.set_autocomplete_popup(1);
    }

    #[test]
    fn paste_inserts_multiline_text_and_dismisses_popup_without_turn() {
        let mut state = fresh_state();
        seed_popup(&mut state);
        let initial_turn_start = state.turn_start;

        insert_paste_text(&mut state, "line 1\nline 2\nline 3");

        assert_eq!(state.input_editor.text(), "line 1\nline 2\nline 3");
        assert_eq!(
            state.input_editor.lines(),
            &[
                "line 1".to_owned(),
                "line 2".to_owned(),
                "line 3".to_owned()
            ]
        );
        assert!(state.autocomplete.is_none());
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
        assert_eq!(state.turn_start, initial_turn_start);
    }

    #[test]
    fn paste_splices_at_cursor_and_parks_after_inserted_text() {
        let mut state = fresh_state();
        for ch in "hello world".chars() {
            state.input_editor.insert_char(ch);
        }
        for _ in 0..6 {
            state.input_editor.cursor_left();
        }

        insert_paste_text(&mut state, "PASTED\nLINE2");

        assert_eq!(
            state.input_editor.lines(),
            &["helloPASTED".to_owned(), "LINE2 world".to_owned()]
        );
        assert_eq!(state.input_editor.text(), "helloPASTED\nLINE2 world");
        assert_eq!(state.input_editor.cursor_position(), (1, 5));
    }

    #[test]
    fn paste_then_delete_shrinks_fixed_panel_to_visual_height() {
        let mut state = fresh_state();
        let cols = 80;
        let terminal_rows = 24;

        insert_paste_text(&mut state, "line1\nline2\nline3");
        let grown_rows = sync_input_area(&mut state.input_editor, cols, terminal_rows);
        state.fixed_panel.set_input_area(grown_rows);
        assert_eq!(state.fixed_panel.total_height(), 5);

        for _ in 0..=5 {
            state.input_editor.backspace();
        }
        let shrunk_rows = sync_input_area(&mut state.input_editor, cols, terminal_rows);
        state.fixed_panel.set_input_area(shrunk_rows);

        assert_eq!(state.input_editor.text(), "line1\nline2");
        assert_eq!(shrunk_rows, state.input_editor.visual_height(cols));
        assert_eq!(state.fixed_panel.total_height(), 2 + shrunk_rows);
    }

    #[test]
    fn handle_action_toggle_verbosity_flips_state() {
        let mut state = fresh_state();
        assert_eq!(state.verbosity, VerbosityState::Expanded);
        state.verbosity = state.verbosity.toggle();
        assert_eq!(state.verbosity, VerbosityState::Collapsed);
        state.verbosity = state.verbosity.toggle();
        assert_eq!(state.verbosity, VerbosityState::Expanded);
    }

    #[test]
    fn handle_action_toggle_thinking_flips_display_toggles() {
        let mut state = fresh_state();
        assert!(!state.display_toggles.thinking_visible);
        state.display_toggles.toggle();
        assert!(state.display_toggles.thinking_visible);
        state.display_toggles.toggle();
        assert!(!state.display_toggles.thinking_visible);
    }

    #[test]
    fn types_compile() {
        // Surface-level type checks: TuiInputs and RuntimeRefs are
        // structurally honest. We cannot run the loop without a real
        // terminal, so we settle for this.
        let _ = std::mem::size_of::<TuiInputs>();
        let _ = std::mem::size_of::<RuntimeRefs>();
    }

    use termina::event::{KeyEvent, KeyEventState};

    fn key_press(code: KeyCode, modifiers: Modifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn is_ctrl_c_detects_press_with_control_modifier() {
        let event = key_press(KeyCode::Char('c'), Modifiers::CONTROL);
        assert!(is_ctrl_c(&event));
    }

    #[test]
    fn is_ctrl_c_ignores_uppercase_c_without_control() {
        let event = key_press(KeyCode::Char('C'), Modifiers::SHIFT);
        assert!(
            !is_ctrl_c(&event),
            "shifted C is a literal capital, not a cancellation"
        );
    }

    #[test]
    fn is_ctrl_c_ignores_control_with_other_letters() {
        let event = key_press(KeyCode::Char('a'), Modifiers::CONTROL);
        assert!(!is_ctrl_c(&event));
    }

    #[test]
    fn is_ctrl_c_ignores_release_event_kind() {
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            kind: KeyEventKind::Release,
            modifiers: Modifiers::CONTROL,
            state: KeyEventState::NONE,
        });
        assert!(
            !is_ctrl_c(&event),
            "key release must not trigger cancellation"
        );
    }

    #[test]
    fn is_ctrl_c_ignores_non_key_events() {
        let event = Event::WindowResized(termina::WindowSize {
            rows: 24,
            cols: 80,
            pixel_width: None,
            pixel_height: None,
        });
        assert!(!is_ctrl_c(&event));
    }

    #[test]
    fn is_ctrl_c_requires_control_modifier_not_just_c() {
        let event = key_press(KeyCode::Char('c'), Modifiers::NONE);
        assert!(!is_ctrl_c(&event));
    }
}
