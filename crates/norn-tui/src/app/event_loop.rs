//! TUI event loop and `ProviderEvent` dispatch.
//!
//! [`run_app`] drives pre-built [`TuiInputs`]. CLI construction stays in
//! `norn-cli` to preserve the one-way `norn-cli` to `norn-tui` dependency.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use termina::event::{KeyCode, KeyEventKind, Modifiers};
use termina::{Event, EventReader, Terminal as _};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use norn::agent::registry::AgentRegistry;
use norn::agent_loop::config::AgentLoopConfig;
use norn::agent_loop::inbound::InboundChannel;
use norn::agent_loop::loop_context::LoopContext;
use norn::agent_loop::runner::ToolExecutor;
use norn::integration::McpControlHandle;
use norn::provider::request::ToolDefinition;
use norn::provider::traits::Provider;
use norn::session::store::EventStore;

use crate::TuiError;
use crate::input::history::InputHistory;
use crate::input::keybindings::{InputAction, map_key_event};
use crate::render::fixed_panel::StatusBar;
use crate::terminal::caps::TerminalCaps;
use crate::terminal::setup::TerminalGuard;

use super::autocomplete::{PopupKeyOutcome, dismiss as dismiss_autocomplete, handle_popup_key};
use super::child_results::{ChildResultRx, PendingChildPrompts};
use super::dispatch::handle_agent_event;
use super::edit::apply_edit_action;
use super::mcp_slash::{
    McpCommandTask, mcp_exit_is_blocked, render_completed_mcp, render_pending_mcp_exit,
    wait_mcp_result,
};
use super::render::{load_visible, redraw_all, sync_input_area, write_user_message};
use super::session_replay::replay_visible_session_history;
use super::slash::{SlashOutcome, try_dispatch_slash};
use super::state::AppState;
use super::turn::{
    run_pending_child_prompts, run_ready_mcp_channels, run_ready_root_inbound, run_turn_and_pending,
};

/// Bundled runtime inputs needed by [`run_app`].
///
/// Keeps the function signature inside the
/// `clippy::too_many_arguments` budget and isolates the TUI from the
/// norn-cli crate's concrete builder types.
pub struct TuiInputs {
    /// Validated frontend choices and immutable save authority, loaded by the caller.
    pub frontend_preferences: crate::frontend_preferences::FrontendPreferencesLaunch,
    /// Concrete provider built by `norn-cli::print::build_provider`.
    pub provider: Arc<dyn Provider>,
    /// Tool executor (the gated `ToolRegistry` from the agent's `AgentParts`).
    pub executor: Arc<dyn ToolExecutor>,
    /// Session event store.
    pub store: Arc<EventStore>,
    /// Actual session owner binding supplied by the assembled runtime.
    pub session_binding: Arc<norn::session::SessionBinding>,
    /// Shared agent registry — read by the agent status panel.
    pub registry: Arc<RwLock<AgentRegistry>>,
    /// Loop context with system sections, rules, hooks, event schemas.
    pub loop_context: LoopContext,
    /// Agent-loop configuration.
    pub agent_config: AgentLoopConfig,
    /// Model identifier.
    pub model: String,
    /// Validated model policy including explicit context provenance.
    pub model_selection: norn::model_selection::ModelRuntime,
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
    /// Deadline for TUI-created session index locks during `/new`.
    pub index_lock_deadline: std::time::Duration,
    /// Root agent's event sender — used by `run_turn` to tag root
    /// events on the shared broadcast channel.
    pub root_event_sender: norn::provider::agent_event::AgentEventSender,
    /// Persistent receiver for root and child agent events.
    pub agent_event_rx: broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
    /// The root agent's inbound channel (W3.7), created by norn-cli's
    /// `install_agent_tool_infra`, which registered the sender half in
    /// the `MessageRouter` under `root_id`. Threaded into every root
    /// `run_agent_step` so a child's `signal_agent(to: "parent")` drains
    /// at the root's step boundaries through the framed
    /// `<agent_message>` injection path; messages arriving between turns
    /// buffer (bounded by the coordination envelope's
    /// `inbound_capacity`) and drain in the next turn. `None` only when
    /// the driver's assembly could not wire a route (no shared tool
    /// context) — child→root sends then fail with the typed `NotRouted`
    /// reason, exactly the pre-wiring behavior.
    pub root_inbound: Option<InboundChannel>,
    /// Session-scoped live MCP control for `/mcp`.
    pub mcp_control: Option<McpControlHandle>,
    /// The run tree's ROOT cancellation token — the builder's
    /// `AgentParts::cancel`, the same token published to every spawned
    /// descendant as `AgentCancellation`.
    ///
    /// The TUI uses it in exactly two places (retry-forever DESIGN D7):
    /// every turn runs on a `child_token` of it, and
    /// [`RootCancelOnExit`] cancels it when the app returns, so no
    /// descendant's retry loop outlives the TUI. Callers that assemble
    /// through `AgentBuilder` pass `parts.cancel`; an embedder without
    /// one passes a fresh token and gets exit-cancellation of nothing —
    /// honest, never a silent half-wiring.
    pub root_cancel: tokio_util::sync::CancellationToken,
}

/// Render-tick cadence — 120 fps for tear-free panel redraws and
/// immediate input painting during streaming.
pub(super) const RENDER_TICK: Duration = Duration::from_millis(8);

/// Cancels the run tree's ROOT token when the TUI app returns — by ANY
/// path (retry-forever DESIGN D7).
///
/// The root token is the builder's `AgentParts::cancel`, published to every
/// spawned descendant as `AgentCancellation`, so cancelling it ends every
/// child's and grandchild's in-flight run. With the loop's retry policy
/// unbounded by default, a descendant abandoned at TUI exit is not merely
/// an idle task: it is a retry loop that keeps calling the provider with
/// nobody watching. The single exit seam is therefore the guard's `Drop`
/// rather than a list of `return` sites — quit, terminal EOF, a fatal
/// terminal I/O error, and an unwinding panic all pass through it, and no
/// future exit path can forget to.
///
/// Per-turn cancellation is deliberately NOT this token: each turn runs on
/// a `child_token` of it (`turn::run::turn_cancel_token`), so Ctrl+C
/// mid-turn stays turn-local while exit cascades.
pub(super) struct RootCancelOnExit(tokio_util::sync::CancellationToken);

impl RootCancelOnExit {
    /// Take ownership of the root token for the app's lifetime.
    pub(super) fn new(root: tokio_util::sync::CancellationToken) -> Self {
        Self(root)
    }

    /// The guarded root token — cloned into [`RuntimeRefs`] so each turn
    /// can mint its child token from it.
    pub(super) fn token(&self) -> &tokio_util::sync::CancellationToken {
        &self.0
    }
}

impl Drop for RootCancelOnExit {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Mint the cancellation token for one turn — the other half of D7.
///
/// The turn's token is a
/// [`child_token`](tokio_util::sync::CancellationToken::child_token) of the
/// run tree's ROOT token, which makes the two directions explicit and
/// opposite:
///
/// - **Ctrl+C during a turn stays turn-local**: cancelling the returned
///   token ends this step only. The root — and therefore every spawned
///   child, which the operator closes through `close_agent` — is untouched.
/// - **App exit cascades**: [`RootCancelOnExit`] cancels the root, which
///   cancels this turn and every descendant's run with it, so no child's
///   retry loop outlives the TUI.
///
/// It lives here rather than in the turn module deliberately: the turn
/// module then needs no `CancellationToken` in scope at all, so a turn
/// cannot quietly go back to minting a free-standing token (which is
/// exactly the defect this replaced).
pub(super) fn turn_cancel_token(
    root: &tokio_util::sync::CancellationToken,
) -> tokio_util::sync::CancellationToken {
    root.child_token()
}

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
    // FIRST statement, so it is also the LAST drop: every exit from this
    // function — clean quit, terminal EOF, a `?` on terminal setup, an
    // unwinding panic — cancels the root token and with it every spawned
    // descendant's run (D7). Declared before the terminal guard so the
    // cascade fires after the terminal is restored.
    let root_cancel = RootCancelOnExit::new(inputs.root_cancel);
    TerminalCaps::check_hard_requirements()?;
    let mut guard = TerminalGuard::new()?;
    let source = inputs
        .store
        .bind_view_source(&inputs.session_binding, inputs.root_id, None)?;
    let caps = guard.caps().clone();
    let pending_messages = inputs.loop_context.pending_agent_messages.clone();
    let mut state = AppState::new(
        caps,
        inputs.history,
        Arc::clone(&inputs.registry),
        source,
        inputs.status_bar,
    );
    super::frontend_preferences::install(&mut state, inputs.frontend_preferences);
    state.agent_panel.set_pending_messages(pending_messages);

    replay_visible_session_history(&mut state, &inputs.store).await?;

    redraw_all(&mut state, &mut guard)?;
    load_visible(&mut state, &inputs.store)?;
    redraw_all(&mut state, &mut guard)?;

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
        session_binding: inputs.session_binding,
        loop_context: inputs.loop_context,
        agent_config: inputs.agent_config,
        model: inputs.model,
        model_selection: inputs.model_selection,
        tools: inputs.tools,
        data_dir: inputs.data_dir,
        session_id: inputs.session_id,
        index_lock_deadline: inputs.index_lock_deadline,
        root_event_sender: inputs.root_event_sender,
        root_inbound: inputs.root_inbound,
        mcp_control: inputs.mcp_control,
        mcp_command: None,
        root_cancel: root_cancel.token().clone(),
    };

    // The TUI owns the child-result receiver so it can surface final
    // child/fork results as soon as they arrive, including while the
    // root turn is still streaming. The framed model injection is queued
    // and processed at the next safe root-turn boundary.
    let mut child_results = ChildResultState::new(runtime.loop_context.child_result_rx.take());

    let outcome = async {
        if let Some(prompt) = inputs.initial_prompt
            && !prompt.trim().is_empty()
        {
            let trimmed = prompt.trim().to_string();
            let input = write_user_message(trimmed, &mut state)?;
            run_turn_and_pending(
                &mut state,
                &mut runtime,
                &mut guard,
                input,
                &mut term_rx,
                &mut agent_event_rx,
                &mut child_results,
            )
            .await?;
            redraw_all(&mut state, &mut guard)?;
        }

        outer_loop(
            &mut state,
            &mut runtime,
            &mut guard,
            term_rx,
            child_results,
            &mut agent_event_rx,
        )
        .await
    }
    .await;
    let saves = super::frontend_preferences::drain(&mut state).await;
    let exports = super::view_actions::reading::drain_exports(&mut state).await;
    super::frontend_preferences::exit_outcome(outcome, saves, exports)
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
                    if term_tx.send(Err(err)).is_err() {
                        tracing::debug!("terminal error receiver has closed");
                    }
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
    pub(super) session_binding: Arc<norn::session::SessionBinding>,
    pub(super) loop_context: LoopContext,
    pub(super) agent_config: AgentLoopConfig,
    pub(super) model: String,
    pub(super) model_selection: norn::model_selection::ModelRuntime,
    pub(super) tools: Vec<ToolDefinition>,
    /// Session data directory for persistence. `None` in ephemeral mode.
    pub(super) data_dir: Option<std::path::PathBuf>,
    /// Current session identifier. Updated on `/new` rotation.
    pub(super) session_id: Option<String>,
    /// Index-lock deadline for TUI-constructed `SessionManager`s
    /// (`/new` rotation). See [`TuiInputs::index_lock_deadline`].
    pub(super) index_lock_deadline: std::time::Duration,
    /// Root agent's event sender — passed to `run_agent_step` so the
    /// root's `ProviderEvent` values are tagged and broadcast.
    pub(super) root_event_sender: norn::provider::agent_event::AgentEventSender,
    /// Root agent's inbound channel (W3.7) — owned here for the app's
    /// lifetime (the route registered under the root's id in the
    /// `MessageRouter` lives exactly as long as this receiver) and
    /// passed to every root `run_agent_step` so child→root messages
    /// drain at step boundaries. Survives `/new` rotation: rotation
    /// reuses the router and the root identity, so the route stays
    /// valid across store swaps.
    pub(super) root_inbound: Option<InboundChannel>,
    pub(super) mcp_control: Option<McpControlHandle>,
    pub(super) mcp_command: Option<McpCommandTask>,
    /// The run tree's root cancellation token (see
    /// [`TuiInputs::root_cancel`]). Every turn's own token is minted from
    /// this one via
    /// [`turn_cancel_token`](crate::app::turn::turn_cancel_token).
    pub(super) root_cancel: tokio_util::sync::CancellationToken,
}

/// TUI-owned child-result delivery state.
pub(super) struct ChildResultState {
    pub(super) rx: ChildResultRx,
    pub(super) pending_prompts: PendingChildPrompts,
}

impl ChildResultState {
    fn new(rx: ChildResultRx) -> Self {
        Self {
            rx,
            pending_prompts: PendingChildPrompts::new(),
        }
    }
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
    mut child_results: ChildResultState,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
) -> Result<(), TuiError> {
    let mut tick = tokio::time::interval(RENDER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut channel_wake_paused = false;
    let mut events_closed = false;
    let mut inbound_closed = false;
    loop {
        redraw_all(state, guard)?;
        load_visible(state, &runtime.store)?;
        redraw_all(state, guard)?;
        tokio::select! {
            biased;
            msg = term_rx.recv() => {
                let Some(result) = msg else { return Ok(()); };
                state.screen.terminal_event(term_rx.len());
                match dispatch_input(result?, state, runtime, guard, &mut term_rx, agent_event_rx, &mut child_results).await? {
                    InputOutcome::Continue => {},
                    InputOutcome::OperatorTurn => channel_wake_paused = false,
                    InputOutcome::Exit => return Ok(()),
                }
            }
            event = agent_event_rx.recv(), if !events_closed => {
                match event {
                    Ok(event) => { handle_agent_event(state, event)?; state.screen.allow_body_load = true; }
                    Err(broadcast::error::RecvError::Lagged(missed)) => { state.transcript.projection.mark_lagged(missed)?; }
                    Err(broadcast::error::RecvError::Closed) => {
                        events_closed = true;
                        super::notices::notice(state, "Live event source closed", None)?;
                    }
                }
            }
            result = super::frontend_preferences::wait(&mut state.preferences) => {
                super::frontend_preferences::finish(state, result)?;
            }
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
                state.transcript.finish_body(result)?; state.screen.allow_body_load = true; state.screen.dirty = true;
            }
            result = wait_mcp_result(&mut runtime.mcp_command) => {
                render_completed_mcp(state, &mut runtime.mcp_command, result)?;
                state.screen.allow_body_load = true;
            }
            readiness = async {
                match runtime.loop_context.mcp_channel_session.as_ref() {
                    Some(session) => session.wake_ready().await,
                    None => std::future::pending().await,
                }
            }, if !channel_wake_paused => {
                readiness?;
                channel_wake_paused = !run_ready_mcp_channels(state, runtime, guard, &mut term_rx, agent_event_rx, &mut child_results).await?;
            }
            ready = async {
                match runtime.root_inbound.as_mut() {
                    Some(inbound) => inbound.steer_ready().await,
                    None => std::future::pending().await,
                }
            }, if !inbound_closed => {
                if ready { run_ready_root_inbound(state, runtime, guard, &mut term_rx, agent_event_rx, &mut child_results).await?; }
                else { inbound_closed = true; }
            }
            Some(first) = super::child_results::recv_child_result(&mut child_results.rx) => {
                super::child_results::render_child_result_batch(state, &mut child_results.rx, &mut child_results.pending_prompts, first)?;
                state.screen.allow_body_load = true;
                run_pending_child_prompts(state, runtime, guard, &mut term_rx, agent_event_rx, &mut child_results).await?;
            }
            _ = tick.tick() => { state.tick(Instant::now()); }
        }
    }
}

/// Result of dispatching an outer-loop terminal event.
enum InputOutcome {
    /// Keep looping.
    Continue,
    /// An ordinary operator turn ran and may retry retained channel input.
    OperatorTurn,
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
    child_results: &mut ChildResultState,
) -> Result<InputOutcome, TuiError> {
    match event {
        Event::Key(key) => {
            let cols = guard.terminal_columns();
            if state.autocomplete.is_some()
                && key.kind == KeyEventKind::Press
                && matches!(
                    handle_popup_key(key, state, cols, guard.terminal_rows())?,
                    PopupKeyOutcome::Consumed
                )
            {
                redraw_all(state, guard)?;
                return Ok(InputOutcome::Continue);
            }
            if super::view_actions::key(key, state) {
                redraw_all(state, guard)?;
                load_visible(state, &runtime.store)?;
                return Ok(InputOutcome::Continue);
            }
            let popup_open = state.autocomplete.is_some();
            let Some(action) = map_key_event(key, state.composer_send_key, popup_open) else {
                return Ok(InputOutcome::Continue);
            };
            handle_action(
                action,
                state,
                runtime,
                guard,
                term_rx,
                agent_event_rx,
                child_results,
            )
            .await
        }
        Event::Mouse(event) => {
            if super::view_actions::mouse(event, state) {
                redraw_all(state, guard)?;
                load_visible(state, &runtime.store)?;
            }
            Ok(InputOutcome::Continue)
        }
        Event::Paste(text) => {
            super::view_actions::pin_visible(state)?;
            insert_paste_text(state, &text)?;
            sync_input_for_current_geometry(state, guard)?;
            redraw_all(state, guard)?;
            Ok(InputOutcome::Continue)
        }
        Event::WindowResized(size) => {
            guard.handle_resize(size.cols, size.rows);
            state.screen.allow_body_load = false;
            sync_input_for_current_geometry(state, guard)?;
            redraw_all(state, guard)?;
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
pub(super) fn sync_input_for_current_geometry(
    state: &mut AppState,
    guard: &TerminalGuard,
) -> Result<(), TuiError> {
    let rows = sync_input_area(state, guard.terminal_columns(), guard.terminal_rows())?;
    state.fixed_panel.set_input_area(rows);
    Ok(())
}

/// Bracketed paste is one reversible edit and never a send or completion gesture.
pub(super) fn insert_paste_text(state: &mut AppState, text: &str) -> Result<(), TuiError> {
    state.input_editor.paste_cells(text)?;
    dismiss_autocomplete(state);
    state.screen.dirty = true;
    Ok(())
}

async fn handle_action(
    action: InputAction,
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    term_rx: &mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    agent_event_rx: &mut broadcast::Receiver<norn::provider::agent_event::AgentEvent>,
    child_results: &mut ChildResultState,
) -> Result<InputOutcome, TuiError> {
    let mut outcome = InputOutcome::Continue;
    match action {
        InputAction::Exit => {
            if state.input_editor.is_empty() {
                if mcp_exit_is_blocked(runtime.mcp_command.as_ref()) {
                    render_pending_mcp_exit(state)?;
                } else {
                    return Ok(InputOutcome::Exit);
                }
            }
            state.input_editor.clear()?;
            dismiss_autocomplete(state);
        }
        InputAction::Submit => {
            dismiss_autocomplete(state);
            let Some(snapshot) = super::composer_submission::prepare(state)? else {
                return Ok(InputOutcome::Continue);
            };
            let text = snapshot.text().to_owned();
            let slash = try_dispatch_slash(&text, state, runtime).await?;
            match slash {
                Some(SlashOutcome::Rejected) => {}
                Some(SlashOutcome::AcceptedWithError(error)) => {
                    return Err(super::composer_submission::accepted_with_error(
                        state, &snapshot, error,
                    ));
                }
                Some(SlashOutcome::Exit) => {
                    super::composer_submission::accepted_local(state, &snapshot)?;
                    return Ok(InputOutcome::Exit);
                }
                Some(SlashOutcome::Continue) => {
                    super::composer_submission::accepted_local(state, &snapshot)?;
                }
                None => {
                    let input = super::composer_submission::begin(state, snapshot)?;
                    run_turn_and_pending(
                        state,
                        runtime,
                        guard,
                        input,
                        term_rx,
                        agent_event_rx,
                        child_results,
                    )
                    .await?;
                    outcome = InputOutcome::OperatorTurn;
                }
            }
        }
        InputAction::ToggleInFlightSubmitMode => {
            state.in_flight_input.toggle_mode();
        }
        other => {
            let cols = guard.terminal_columns();
            let result = apply_edit_action(other, state, cols, guard.terminal_rows())?;
            super::composer_effects::finish(state, guard, result)?;
        }
    }
    super::frontend_preferences::edited(state)?;
    sync_input_for_current_geometry(state, guard)?;
    redraw_all(state, guard)?;
    Ok(outcome)
}

/// Detect Ctrl+C on a terminal [`Event`].
///
/// Mirrors the keybindings module's Ctrl+C handling
/// ([`map_key_event`](crate::input::keybindings::map_key_event)) —
/// only [`KeyEventKind::Press`] counts, so a key release never
/// triggers cancellation. Inlined here rather than going through
/// [`map_key_event`] so the cancel path doesn't depend on the broader
/// [`InputAction`] enum or popup-state argument.
pub(super) fn is_ctrl_c(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('c')
                && key.modifiers.contains(Modifiers::CONTROL),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::InputEditor;
    use crate::input::autocomplete::{AutocompletePopup, SlashCandidate, SourceTag};
    use crate::input::history::InputHistory;
    use crate::render::fixed_panel::StatusBar;
    use crate::terminal::caps::TerminalCaps;
    use crate::tools::VerbosityState;
    use norn::agent::registry::AgentRegistry;

    fn fresh_state() -> Result<AppState, Box<dyn std::error::Error>> {
        let registry = AgentRegistry::shared();
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
        )?;
        let root_id = guard.id();
        guard.confirm()?;
        Ok(AppState::new(
            TerminalCaps::baseline(),
            InputHistory::in_memory(),
            registry,
            crate::app::state::test_view_source(root_id),
            StatusBar::default(),
        ))
    }

    fn type_text(editor: &mut InputEditor, text: &str) -> Result<(), TuiError> {
        let options = iridium_editor::editor::CellInputOptions {
            wrap: iridium_editor::cell_layout::CellWrapParameters::new(80, 4),
            visible_rows: 10,
        };
        for character in text.chars() {
            assert_eq!(
                editor.handle_cell_key(
                    &iridium_editor::KeyEvent::simple(iridium_editor::KeyCode::Char(character)),
                    options
                )?,
                iridium_editor::EditorKeyResult::None
            );
        }
        Ok(())
    }

    fn logical_lines(editor: &InputEditor) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let document = &editor.kernel().state().document;
        (0..document.line_count())
            .map(|line| {
                document
                    .line(line)
                    .ok_or_else(|| format!("fixture logical line {line} is missing").into())
            })
            .collect()
    }

    fn seed_popup(state: &mut AppState) -> Result<(), TuiError> {
        let candidates = vec![SlashCandidate {
            name: "help".to_owned(),
            source_tag: SourceTag::Builtin,
            description: "Show help".to_owned(),
        }];
        state.autocomplete = Some(AutocompletePopup::new_slash(
            candidates,
            "",
            state.input_editor.completion_context(0)?,
        ));
        state.fixed_panel.set_autocomplete_popup(1);
        Ok(())
    }

    #[test]
    fn live_definition_secrets_never_reach_file_backed_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let history_path = directory.path().join("history.txt");
        let mut state = fresh_state()?;
        state.input_editor = InputEditor::new(InputHistory::load_from(&history_path));
        type_text(&mut state.input_editor, "ordinary prompt")?;
        let snapshot = super::super::composer_submission::prepare(&mut state)?
            .ok_or("ordinary submission absent")?;
        assert_eq!(snapshot.text(), "ordinary prompt");
        super::super::composer_submission::accepted_local(&mut state, &snapshot)?;
        assert!(state.input_editor.is_empty());

        let secret_inputs = [
            "/mcp add local stdio command --env TOKEN=env-secret",
            "/mcp add remote http https://example.test/private --header Authorization=header-secret",
        ];
        for input in secret_inputs {
            type_text(&mut state.input_editor, input)?;
            let snapshot = super::super::composer_submission::prepare(&mut state)?
                .ok_or("definition submission absent")?;
            assert_eq!(snapshot.text(), input);
            super::super::composer_submission::accepted_local(&mut state, &snapshot)?;
            assert!(state.input_editor.is_empty());
        }

        let persisted = std::fs::read_to_string(history_path)?;
        assert!(persisted.contains("ordinary prompt"));
        assert!(!persisted.contains("env-secret"));
        assert!(!persisted.contains("header-secret"));
        assert!(!persisted.contains("example.test/private"));
        Ok(())
    }

    #[test]
    fn paste_inserts_multiline_text_and_dismisses_popup_without_turn()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        seed_popup(&mut state)?;
        let initial_turn_start = state.turn_start;

        insert_paste_text(&mut state, "line 1\nline 2\nline 3")?;

        assert_eq!(state.input_editor.text(), "line 1\nline 2\nline 3");
        assert_eq!(
            logical_lines(&state.input_editor)?,
            &[
                "line 1".to_owned(),
                "line 2".to_owned(),
                "line 3".to_owned()
            ]
        );
        assert!(state.autocomplete.is_none());
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
        assert_eq!(state.turn_start, initial_turn_start);
        Ok(())
    }

    #[test]
    fn paste_splices_at_cursor_and_parks_after_inserted_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        type_text(&mut state.input_editor, "hello world")?;
        for _ in 0..6 {
            assert_eq!(
                apply_edit_action(InputAction::CursorLeft, &mut state, 80, 24)?,
                iridium_editor::EditorKeyResult::None
            );
        }

        insert_paste_text(&mut state, "PASTED\nLINE2")?;

        assert_eq!(
            logical_lines(&state.input_editor)?,
            &["helloPASTED".to_owned(), "LINE2 world".to_owned()]
        );
        assert_eq!(state.input_editor.text(), "helloPASTED\nLINE2 world");
        assert_eq!(state.input_editor.cursor_position(), (1, 5));
        Ok(())
    }

    #[test]
    fn paste_then_delete_shrinks_fixed_panel_to_visual_height()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let cols = 80;
        let terminal_rows = 24;

        insert_paste_text(&mut state, "line1\nline2\nline3")?;
        let grown_rows = sync_input_area(&mut state, cols, terminal_rows)?;
        state.fixed_panel.set_input_area(grown_rows);
        assert_eq!(state.fixed_panel.total_height(), 6);

        for _ in 0..=5 {
            assert_eq!(
                apply_edit_action(InputAction::Backspace, &mut state, cols, terminal_rows)?,
                iridium_editor::EditorKeyResult::None
            );
        }
        let shrunk_rows = sync_input_area(&mut state, cols, terminal_rows)?;
        state.fixed_panel.set_input_area(shrunk_rows);

        assert_eq!(state.input_editor.text(), "line1\nline2");
        assert_eq!(
            usize::from(shrunk_rows),
            state.composer_geometry.total_rows()
        );
        assert_eq!(state.fixed_panel.total_height(), 3 + shrunk_rows);
        Ok(())
    }

    #[test]
    fn handle_action_toggle_verbosity_flips_state() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        assert_eq!(state.verbosity, VerbosityState::Expanded);
        state.verbosity = state.verbosity.toggle();
        assert_eq!(state.verbosity, VerbosityState::Collapsed);
        state.verbosity = state.verbosity.toggle();
        assert_eq!(state.verbosity, VerbosityState::Expanded);
        Ok(())
    }

    #[test]
    fn handle_action_toggle_thinking_flips_display_toggles()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        assert!(state.display_toggles.thinking_visible);
        assert!(!state.display_toggles.secondary_fields_visible);
        state.display_toggles.toggle();
        assert!(!state.display_toggles.thinking_visible);
        assert!(!state.display_toggles.secondary_fields_visible);
        state.display_toggles.toggle();
        assert!(state.display_toggles.thinking_visible);
        assert!(state.display_toggles.secondary_fields_visible);
        Ok(())
    }

    /// D7 exit seam: the app returning — by any path — cancels the ROOT
    /// token, which is what every spawned descendant's run token descends
    /// from. Pre-fix nothing in the TUI ever touched the root token, so a
    /// quit left children retrying against the provider forever.
    #[test]
    fn root_cancel_guard_cancels_the_root_token_on_app_exit() {
        let root = tokio_util::sync::CancellationToken::new();
        {
            let guard = RootCancelOnExit::new(root.clone());
            assert!(
                !guard.token().is_cancelled(),
                "the token must stay live while the app runs",
            );
        }
        assert!(
            root.is_cancelled(),
            "leaving the app must cancel the root token",
        );
    }

    /// D7: the per-turn token descends from the root token, so an app-exit
    /// cancel of the root reaches a turn that is still in flight. Pre-fix
    /// `run_turn` minted a free-standing `CancellationToken::new()` and the
    /// builder's root token was never used by the TUI at all.
    #[test]
    fn turn_token_is_cancelled_by_the_root_token() {
        let root = tokio_util::sync::CancellationToken::new();
        let turn = turn_cancel_token(&root);
        assert!(!turn.is_cancelled());

        root.cancel();

        assert!(
            turn.is_cancelled(),
            "an app-exit root cancel must reach the in-flight turn",
        );
    }

    /// The other direction must NOT hold: Ctrl+C cancels the turn only.
    /// Making the turn token a child (not a clone) of the root is exactly
    /// what keeps mid-turn cancellation turn-local while still cascading
    /// on exit.
    #[test]
    fn cancelling_a_turn_leaves_the_root_and_the_next_turn_alive() {
        let root = tokio_util::sync::CancellationToken::new();
        let first = turn_cancel_token(&root);

        first.cancel();

        assert!(
            !root.is_cancelled(),
            "a mid-turn Ctrl+C must not tear down the whole app tree",
        );
        let second = turn_cancel_token(&root);
        assert!(
            !second.is_cancelled(),
            "the next turn starts from a live token",
        );
    }

    /// The two D7 halves composed: a turn in flight when the app exits is
    /// cancelled, and so is a child the turn spawned — because both
    /// descend from the root token the exit guard cancels.
    #[test]
    fn app_exit_cancels_an_in_flight_turn_and_its_descendants() {
        let root = tokio_util::sync::CancellationToken::new();
        let guard = RootCancelOnExit::new(root);
        let turn = turn_cancel_token(guard.token());
        // A spawned child chains its own token under its spawner's,
        // exactly as `spawn_agent`/`fork` do from the published
        // `AgentCancellation`.
        let descendant = turn.child_token();

        drop(guard);

        assert!(turn.is_cancelled(), "the in-flight turn must be cancelled");
        assert!(
            descendant.is_cancelled(),
            "no descendant retry loop may outlive the app",
        );
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
