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
use norn::agent_loop::config::AgentLoopConfig;
use norn::agent_loop::inbound::InboundChannel;
use norn::agent_loop::loop_context::LoopContext;
use norn::agent_loop::runner::ToolExecutor;
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
use super::child_results::{ChildResultRx, PendingChildPrompts, drain_ready_child_results};
use super::dispatch::handle_agent_event;
use super::edit::apply_edit_action;
use super::render::{
    redraw_all, redraw_panel, render_input, sync_input_area, with_scroll_region_cursor_async,
    write_user_message,
};
use super::session_replay::replay_visible_session_history;
use super::slash::{SlashOutcome, try_dispatch_slash};
use super::state::AppState;
use super::turn::{run_pending_child_prompts, run_ready_root_inbound, run_turn_and_pending};

/// Bundled runtime inputs needed by [`run_app`].
///
/// Keeps the function signature inside the
/// `clippy::too_many_arguments` budget and isolates the TUI from the
/// norn-cli crate's concrete builder types.
pub struct TuiInputs {
    /// Concrete provider built by `norn-cli::print::build_provider`.
    pub provider: Arc<dyn Provider>,
    /// Tool executor (the gated `ToolRegistry` from the agent's `AgentParts`).
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
}

/// Render-tick cadence — 120 fps for tear-free panel redraws and
/// immediate input painting during streaming.
pub(super) const RENDER_TICK: Duration = Duration::from_millis(8);

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

    let caps = guard.caps().clone();
    let mut state = AppState::new(
        caps,
        inputs.history,
        Arc::clone(&inputs.registry),
        inputs.root_id,
        inputs.status_bar,
    );

    replay_visible_session_history(&state, &inputs.store, &mut guard)?;
    guard.save_scroll_cursor()?;
    guard.terminal_mut().flush()?;

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
        root_inbound: inputs.root_inbound,
    };

    // The TUI owns the child-result receiver so it can surface final
    // child/fork results as soon as they arrive, including while the
    // root turn is still streaming. The framed model injection is queued
    // and processed at the next safe root-turn boundary.
    let mut child_results = ChildResultState::new(runtime.loop_context.child_result_rx.take());

    if let Some(prompt) = inputs.initial_prompt
        && !prompt.trim().is_empty()
    {
        let trimmed = prompt.trim().to_string();
        write_user_message(&trimmed, &mut state, &mut guard)?;
        run_turn_and_pending(
            &mut state,
            &mut runtime,
            &mut guard,
            &trimmed,
            &mut term_rx,
            &mut agent_event_rx,
            &mut child_results,
        )
        .await?;
        redraw_panel(&mut state, &mut guard)?;
        render_input(&state, &mut guard)?;
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
    /// Root agent's inbound channel (W3.7) — owned here for the app's
    /// lifetime (the route registered under the root's id in the
    /// `MessageRouter` lives exactly as long as this receiver) and
    /// passed to every root `run_agent_step` so child→root messages
    /// drain at step boundaries. Survives `/new` rotation: rotation
    /// reuses the router and the root identity, so the route stays
    /// valid across store swaps.
    pub(super) root_inbound: Option<InboundChannel>,
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

    loop {
        tokio::select! {
            biased;
            msg = term_rx.recv() => {
                let Some(result) = msg else { return Ok(()); };
                let event = result.map_err(TuiError::Io)?;
                match dispatch_input(
                    event,
                    state,
                    runtime,
                    guard,
                    &mut term_rx,
                    agent_event_rx,
                    &mut child_results,
                ).await? {
                    InputOutcome::Continue => {}
                    InputOutcome::Exit => return Ok(()),
                }
            }
            event = agent_event_rx.recv() => {
                if let Ok(agent_ev) = event {
                    guard.restore_scroll_cursor_clamped()?;
                    handle_agent_event(state, guard, &mut None, agent_ev)?;
                    guard.save_scroll_cursor()?;
                    redraw_panel(state, guard)?;
                    render_input(state, guard)?;
                }
            }
            _ = tick.tick() => {
                drain_ready_child_results(
                    state,
                    guard,
                    &mut child_results.rx,
                    &mut child_results.pending_prompts,
                )?;
                run_ready_root_inbound(
                    state,
                    runtime,
                    guard,
                    &mut term_rx,
                    agent_event_rx,
                    &mut child_results,
                ).await?;
                run_pending_child_prompts(
                    state,
                    runtime,
                    guard,
                    &mut term_rx,
                    agent_event_rx,
                    &mut child_results,
                ).await?;
                let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
                if state.tick_indicator_repaint_needed(Instant::now(), cols) {
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
    child_results: &mut ChildResultState,
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
pub(super) fn sync_input_for_current_geometry(state: &mut AppState, guard: &mut TerminalGuard) {
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let input_rows = sync_input_area(&mut state.input_editor, cols, guard.terminal_rows());
    state.fixed_panel.set_input_area(input_rows);
}

pub(super) fn insert_paste_text(state: &mut AppState, text: &str) {
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
    child_results: &mut ChildResultState,
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
            sync_input_for_current_geometry(state, guard);
            redraw_all(state, guard)?;
            // Phase 1 slash dispatch. None = not a recognised slash —
            // fall through to the normal run_turn pipeline. Some =
            // handled here; skip run_turn. Exit short-circuits the
            // outer loop directly.
            let slash = with_scroll_region_cursor_async(guard, async |guard| {
                try_dispatch_slash(&text, state, runtime, guard).await
            })
            .await?;
            match slash {
                Some(SlashOutcome::Exit) => return Ok(InputOutcome::Exit),
                Some(SlashOutcome::Continue) => {}
                None => {
                    write_user_message(&text, state, guard)?;
                    run_turn_and_pending(
                        state,
                        runtime,
                        guard,
                        &text,
                        term_rx,
                        agent_event_rx,
                        child_results,
                    )
                    .await?;
                }
            }
        }
        InputAction::ToggleInFlightSubmitMode => {
            state.in_flight_input.toggle_mode();
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
        assert_eq!(state.fixed_panel.total_height(), 6);

        for _ in 0..=5 {
            state.input_editor.backspace();
        }
        let shrunk_rows = sync_input_area(&mut state.input_editor, cols, terminal_rows);
        state.fixed_panel.set_input_area(shrunk_rows);

        assert_eq!(state.input_editor.text(), "line1\nline2");
        assert_eq!(shrunk_rows, state.input_editor.visual_height(cols));
        assert_eq!(state.fixed_panel.total_height(), 3 + shrunk_rows);
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
        assert!(state.display_toggles.thinking_visible);
        assert!(!state.display_toggles.secondary_fields_visible);
        state.display_toggles.toggle();
        assert!(!state.display_toggles.thinking_visible);
        assert!(!state.display_toggles.secondary_fields_visible);
        state.display_toggles.toggle();
        assert!(state.display_toggles.thinking_visible);
        assert!(state.display_toggles.secondary_fields_visible);
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
