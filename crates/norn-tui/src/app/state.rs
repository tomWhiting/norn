//! Application state — central state object connecting all TUI subsystems.
//!
//! [`AppState`] owns the editor, display toggles, streaming indicator,
//! agent panel, tab state, fixed-panel compositor, and terminal capability
//! snapshot. The event loop in [`super::event_loop`] reads and mutates
//! these fields; `AppState` itself has no I/O — every public method either
//! mutates state or returns owned/borrowed data.
//!
//! Construction is infallible (CO5): every subsystem starts in a known
//! default-valid state and any I/O (history file loading, registry
//! construction) happens in the caller and is passed in via parameters.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use uuid::Uuid;

use norn::agent::registry::AgentRegistry;

use super::active_input::InFlightInputState;
use crate::agents::activity_log::ActivityLog;
use crate::agents::status_line::AgentStatusPanel;
use crate::agents::tabs::TabState;
use crate::events::DisplayToggles;
use crate::input::autocomplete::AutocompletePopup;
use crate::input::editor::InputEditor;
use crate::input::history::InputHistory;
use crate::render::SyntaxHighlighter;
use crate::render::fixed_panel::{FixedPanel, StatusBar, StreamingIndicator, ToolUseInFlight};
use crate::terminal::caps::TerminalCaps;
use crate::tools::VerbosityState;

/// Delay after which a completed streaming indicator transitions back to
/// idle. The value mirrors the brief's "5-second delay" guidance (R5/R7).
pub const STREAMING_COMPLETE_HOLD: Duration = Duration::from_secs(5);

/// Bytes-per-token approximation for the live output-token estimate
/// shown on the streaming indicator. Matches the `OpenAI` published
/// rule-of-thumb for English; JSON-heavy traffic skews a touch denser
/// but the `~` prefix on the display advertises the approximation.
const BYTES_PER_TOKEN: u64 = 4;

/// Convert an accumulated UTF-8 byte count to an estimated token count
/// for live display. Saturates on the `usize → u64` widen so the value
/// never wraps even on a 128-bit machine.
#[must_use]
fn estimated_tokens(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX) / BYTES_PER_TOKEN
}

/// Pending tool call accumulator — argument fragments stream in via
/// [`norn::provider::events::ProviderEvent::ToolCallDelta`] until either
/// `ToolCallComplete` (which finalises name + arguments) or a
/// `ToolResult` arrives.
#[derive(Clone, Debug, Default)]
pub struct PendingToolCall {
    /// Tool name — set from the first delta that carries it, then
    /// finalised by `ToolCallComplete` when it arrives.
    pub name: Option<String>,
    /// Concatenated argument fragments. May not be valid JSON until
    /// `ToolCallComplete` arrives.
    pub arguments: String,
}

/// Central TUI state.
///
/// Every field is owned (no borrows of the runtime) so `AppState` can be
/// constructed once at startup and threaded through the event loop by
/// `&mut`.
pub struct AppState {
    /// Multi-line input editor in the fixed panel.
    pub input_editor: InputEditor,
    /// Visibility toggles for thinking and secondary structured-output
    /// fields. Flipped by Ctrl+E.
    pub display_toggles: DisplayToggles,
    /// Global tool-call verbosity (collapsed vs expanded). Flipped by
    /// Ctrl+O.
    pub verbosity: VerbosityState,
    /// Live streaming indicator state — the single source of truth.
    /// Copied into [`Self::fixed_panel`] before each redraw.
    pub streaming_indicator: StreamingIndicator,
    /// Agent status panel reading the shared registry.
    pub agent_panel: AgentStatusPanel,
    /// Multi-agent tab state.
    pub tab_state: TabState,
    /// Fixed-panel compositor (status bar, agent rows, indicator row,
    /// popup, input area).
    pub fixed_panel: FixedPanel,
    /// Cached terminal capabilities. Cloned from the [`TerminalGuard`]
    /// at startup; rendering helpers borrow this rather than the guard.
    pub terminal_caps: TerminalCaps,
    /// Pending tool calls keyed by provider tool-call id. Holds
    /// accumulated argument deltas until `ToolResult` arrives.
    pub pending_tools: HashMap<String, PendingToolCall>,
    /// Wall-clock instant the current turn began, set on the first
    /// `ProviderEvent` of a turn and cleared after the indicator's hold
    /// window elapses.
    pub turn_start: Option<Instant>,
    /// Wall-clock instant the streaming indicator transitioned to
    /// [`StreamingIndicator::Complete`]. The render tick uses this to
    /// drop the indicator back to [`StreamingIndicator::Idle`] after
    /// [`STREAMING_COMPLETE_HOLD`].
    pub complete_at: Option<Instant>,
    /// Whether any `TextDelta` has been written in the current turn.
    /// Used to decide whether to append a trailing newline after the
    /// turn completes.
    pub text_streamed_this_turn: bool,
    /// Whether the last scroll-region write was a tool result. Used to
    /// insert spacing on tool→text and tool→tool transitions.
    pub last_was_tool_result: bool,
    /// Live autocomplete popup, populated by the event loop's
    /// `refresh_autocomplete` helper. `None` when no trigger is active.
    /// Owned by `AppState` so the popup survives across event-loop
    /// iterations and the fixed panel's reported popup-row height stays
    /// in sync with the visible candidate count.
    pub autocomplete: Option<AutocompletePopup>,
    /// Running output-byte counter for the current turn, accumulated
    /// from every `TextDelta`, `ThinkingDelta`, and `ToolCallDelta`
    /// fragment as their UTF-8 byte length. The streaming indicator's
    /// estimated token figure is derived as `est_output_bytes / 4`
    /// (`OpenAI`'s published rule-of-thumb; the `~` prefix on the display
    /// advertises the approximation). Reset to zero at every turn
    /// boundary by [`Self::note_event_received`].
    pub est_output_bytes: usize,
    /// Tool call currently between `ToolCallComplete` (or the first
    /// `ToolCallDelta` carrying a name) and its matching `ToolResult`.
    /// Cloned into the streaming indicator each render tick so the
    /// "● {tool}: '{desc}'" form stays in sync without storing the
    /// rendering state on the dispatch path.
    pub current_tool_use: Option<ToolUseInFlight>,
    /// Number of terminal lines the current dim preview occupies after
    /// soft-wrapping at the terminal width. Used by `handle_text_delta`
    /// and the tick handler to move the cursor back to the start of the
    /// dim region before erasing — `\r\x1b[2K` only clears one line.
    pub dim_wrapped_lines: u16,
    /// Raw thinking text accumulated during the current turn. A
    /// non-empty buffer means a reasoning summary is pending and must be
    /// rendered before answer text, tool output, or final status.
    pub thinking_buffer: String,
    /// `true` when the last styled write did not end with `\n`, meaning
    /// the cursor is mid-line after committed content. Dim preview must
    /// start on a fresh line to avoid `erase_dim_lines` destroying the
    /// styled text via `\r\x1b[2K`.
    pub styled_mid_line: bool,
    /// Session-scoped syntax highlighter for tool output content blocks.
    /// Loaded once with syntect's ~100 bundled grammars; shared across
    /// all tool result renders via [`crate::render::content::render_blocks`].
    pub highlighter: SyntaxHighlighter,
    /// Activity log — backing ring of recent tool-call initiations.
    /// Dispatch still records entries for diagnostics/replay, while the
    /// fixed panel folds live work into per-agent rows.
    pub activity_log: ActivityLog,
    /// Usage totals already reflected in per-agent rows, keyed by agent
    /// id. Provider `Done` events arrive as deltas, while final
    /// step/lifecycle records carry cumulative totals; this cache lets
    /// the TUI reconcile the latter without double-counting the former.
    usage_totals: HashMap<Uuid, (u64, u64)>,
    /// In-flight human input state for active-turn steering and queued
    /// follow-up prompts.
    pub in_flight_input: InFlightInputState,
}

impl AppState {
    /// Construct a fresh state.
    ///
    /// All subsystems land in default-valid states. The caller threads
    /// the runtime-shaped inputs (`history`, `registry`, `root_id`,
    /// `status_bar`) so `AppState` itself remains free of I/O.
    pub fn new(
        caps: TerminalCaps,
        history: InputHistory,
        registry: Arc<RwLock<AgentRegistry>>,
        root_id: Uuid,
        status_bar: StatusBar,
    ) -> Self {
        Self {
            input_editor: InputEditor::new(history),
            display_toggles: DisplayToggles::default(),
            verbosity: VerbosityState::default(),
            streaming_indicator: StreamingIndicator::Idle,
            agent_panel: AgentStatusPanel::new(registry),
            tab_state: TabState::new(root_id),
            fixed_panel: FixedPanel::new(status_bar),
            terminal_caps: caps,
            pending_tools: HashMap::new(),
            turn_start: None,
            complete_at: None,
            text_streamed_this_turn: false,
            last_was_tool_result: false,
            autocomplete: None,
            est_output_bytes: 0,
            current_tool_use: None,
            dim_wrapped_lines: 0,
            thinking_buffer: String::new(),
            styled_mid_line: false,
            highlighter: SyntaxHighlighter::new(),
            activity_log: ActivityLog::new(),
            usage_totals: HashMap::new(),
            in_flight_input: InFlightInputState::default(),
        }
    }

    /// Mark that a provider event was received.
    ///
    /// On the first event of a turn this transitions the streaming
    /// indicator from [`StreamingIndicator::Idle`] (or a stale
    /// [`StreamingIndicator::Complete`]) to
    /// [`StreamingIndicator::Generating`] and records the turn start.
    /// Crossing a turn boundary (from `Idle` or `Complete` into
    /// `Generating`) zeroes [`Self::est_output_bytes`] so each turn's
    /// estimate starts at zero. Subsequent calls within the same turn
    /// preserve `turn_start` and only refresh the indicator's
    /// `elapsed` and `est_output_tokens` snapshot against `now`.
    pub fn note_event_received(&mut self, now: Instant) {
        let turn_boundary = !matches!(
            self.streaming_indicator,
            StreamingIndicator::Generating { .. }
        );
        if turn_boundary {
            self.est_output_bytes = 0;
            self.current_tool_use = None;
        }
        let start = self.turn_start.unwrap_or(now);
        self.turn_start = Some(start);
        self.complete_at = None;
        let elapsed = now.saturating_duration_since(start);
        self.streaming_indicator = StreamingIndicator::Generating {
            elapsed,
            est_output_tokens: estimated_tokens(self.est_output_bytes),
            in_flight: self.current_tool_use.clone(),
        };
    }

    /// Refresh the streaming indicator's elapsed time on a render tick.
    ///
    /// While generating, recomputes the elapsed time against `now` and
    /// refreshes the token estimate + in-flight tool snapshot from the
    /// dispatch-layer state. While complete, transitions back to idle
    /// once [`STREAMING_COMPLETE_HOLD`] has passed. While idle, this is
    /// a no-op.
    pub fn tick(&mut self, now: Instant) {
        match self.streaming_indicator {
            StreamingIndicator::Generating { .. } => {
                if let Some(start) = self.turn_start {
                    self.streaming_indicator = StreamingIndicator::Generating {
                        elapsed: now.saturating_duration_since(start),
                        est_output_tokens: estimated_tokens(self.est_output_bytes),
                        in_flight: self.current_tool_use.clone(),
                    };
                }
            }
            StreamingIndicator::Complete { .. } => {
                if let Some(at) = self.complete_at
                    && now.saturating_duration_since(at) >= STREAMING_COMPLETE_HOLD
                {
                    self.streaming_indicator = StreamingIndicator::Idle;
                    self.complete_at = None;
                    self.turn_start = None;
                }
            }
            StreamingIndicator::Idle => {}
        }
    }

    /// Refresh tick-driven state and report whether the fixed-panel
    /// indicator should repaint at the current terminal width.
    ///
    /// This lets render ticks keep elapsed-time and completion-hold
    /// bookkeeping current without repainting the fixed panel every few
    /// milliseconds when only sub-second internal state changed.
    #[must_use]
    pub fn tick_indicator_repaint_needed(&mut self, now: Instant, terminal_cols: u16) -> bool {
        let before = self.streaming_indicator.repaint_key(terminal_cols);
        self.tick(now);
        before != self.streaming_indicator.repaint_key(terminal_cols)
    }

    /// Transition the indicator to `Complete { usage_summary }` and
    /// arm the idle-hold timer.
    ///
    /// Clears the in-flight tool use and zeroes the running byte
    /// estimate so the next turn starts from a clean baseline — the
    /// `usage_summary` carries the real numbers, so the byte estimate
    /// has done its job for this turn.
    pub fn mark_complete(&mut self, usage_summary: String, now: Instant) {
        self.streaming_indicator = StreamingIndicator::Complete { usage_summary };
        self.complete_at = Some(now);
        self.current_tool_use = None;
        self.est_output_bytes = 0;
    }

    /// Push the live indicator state into the fixed panel ahead of a
    /// redraw. The brief makes the fixed panel the renderer; `AppState`
    /// owns the live state.
    pub fn sync_indicator_into_panel(&mut self) {
        self.fixed_panel
            .set_streaming_indicator(self.streaming_indicator.clone());
    }

    /// Add one usage delta to the per-agent status row.
    pub fn record_agent_usage_delta(
        &mut self,
        agent_id: Uuid,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        if input_tokens == 0 && output_tokens == 0 {
            return;
        }
        let total = self.usage_totals.entry(agent_id).or_insert((0, 0));
        total.0 = total.0.saturating_add(input_tokens);
        total.1 = total.1.saturating_add(output_tokens);
        self.agent_panel
            .set_tokens(agent_id, input_tokens, output_tokens);
    }

    /// Show the estimated input size for the root provider request that is
    /// about to stream.
    pub fn set_root_input_estimate(&mut self, input_tokens: u64) {
        let status = self.fixed_panel.status_bar_mut();
        status.input_tokens = input_tokens;
        status.input_tokens_estimated = true;
        status.output_tokens = 0;
        status.output_tokens_estimated = false;
    }

    /// Add one root usage delta to the root agent's cumulative row total
    /// and replace the top-chip counters with that provider request's
    /// actual usage.
    pub fn record_root_provider_usage(
        &mut self,
        agent_id: Uuid,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        self.record_agent_usage_delta(agent_id, input_tokens, output_tokens);
        let status = self.fixed_panel.status_bar_mut();
        status.input_tokens = input_tokens;
        status.input_tokens_estimated = false;
        status.output_tokens = output_tokens;
        status.output_tokens_estimated = false;
    }

    /// Reconcile a cumulative usage total for an agent by recording only
    /// the missing positive delta.
    pub fn reconcile_usage_total(&mut self, agent_id: Uuid, input_tokens: u64, output_tokens: u64) {
        let (seen_input, seen_output) = self.usage_totals.get(&agent_id).copied().unwrap_or((0, 0));
        let delta_input = input_tokens.saturating_sub(seen_input);
        let delta_output = output_tokens.saturating_sub(seen_output);
        self.record_agent_usage_delta(agent_id, delta_input, delta_output);
    }

    /// Reset the top-chip live counters for a new root turn.
    pub fn reset_live_usage(&mut self) {
        let status = self.fixed_panel.status_bar_mut();
        status.input_tokens = 0;
        status.input_tokens_estimated = false;
        status.output_tokens = 0;
        status.output_tokens_estimated = false;
    }

    /// Clear all usage totals after a session reset.
    pub fn clear_usage_totals(&mut self) {
        self.usage_totals.clear();
        self.agent_panel.reset_all_tokens();
        self.reset_live_usage();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
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

    #[test]
    fn new_state_has_default_subsystems() {
        let state = fresh_state();
        assert!(state.input_editor.is_empty());
        assert_eq!(state.display_toggles, DisplayToggles::default());
        assert_eq!(state.verbosity, VerbosityState::Expanded);
        assert!(matches!(
            state.streaming_indicator,
            StreamingIndicator::Idle
        ));
        assert!(state.pending_tools.is_empty());
        assert!(state.turn_start.is_none());
        assert!(state.complete_at.is_none());
        assert!(!state.text_streamed_this_turn);
    }

    #[test]
    fn note_event_received_transitions_idle_to_generating() {
        let mut state = fresh_state();
        state.note_event_received(Instant::now());
        assert!(matches!(
            state.streaming_indicator,
            StreamingIndicator::Generating { .. }
        ));
        assert!(state.turn_start.is_some());
    }

    #[test]
    fn note_event_received_keeps_turn_start_across_calls() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.note_event_received(t0);
        let first = state.turn_start;
        state.note_event_received(t0 + Duration::from_millis(500));
        assert_eq!(state.turn_start, first);
    }

    #[test]
    fn mark_complete_transitions_to_complete() {
        let mut state = fresh_state();
        let now = Instant::now();
        state.note_event_received(now);
        state.mark_complete("[1 in / 2 out, 0.5s]".to_string(), now);
        assert!(matches!(
            state.streaming_indicator,
            StreamingIndicator::Complete { .. }
        ));
        assert!(state.complete_at.is_some());
    }

    #[test]
    fn tick_drops_complete_to_idle_after_hold() {
        let mut state = fresh_state();
        let now = Instant::now();
        state.note_event_received(now);
        state.mark_complete("done".to_string(), now);
        // Just before the hold expires — still complete.
        state.tick(
            (now + STREAMING_COMPLETE_HOLD)
                .checked_sub(Duration::from_millis(1))
                .unwrap(),
        );
        assert!(matches!(
            state.streaming_indicator,
            StreamingIndicator::Complete { .. }
        ));
        // After the hold — drops back to idle.
        state.tick(now + STREAMING_COMPLETE_HOLD + Duration::from_millis(1));
        assert!(matches!(
            state.streaming_indicator,
            StreamingIndicator::Idle
        ));
        assert!(state.complete_at.is_none());
        assert!(state.turn_start.is_none());
    }

    #[test]
    fn tick_advances_generating_elapsed() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.tick(t0 + Duration::from_secs(3));
        assert!(
            matches!(
                state.streaming_indicator,
                StreamingIndicator::Generating { elapsed, .. } if elapsed >= Duration::from_secs(3)
            ),
            "expected Generating with elapsed >= 3s, got {:?}",
            state.streaming_indicator,
        );
    }

    #[test]
    fn tick_repaint_ignores_subsecond_elapsed() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.note_event_received(t0);

        assert!(!state.tick_indicator_repaint_needed(t0 + Duration::from_millis(8), 80));
    }

    #[test]
    fn tick_repaint_reports_elapsed_second_boundary() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.note_event_received(t0);

        assert!(state.tick_indicator_repaint_needed(t0 + Duration::from_secs(1), 80));
    }

    #[test]
    fn tick_repaint_ignores_token_estimate_churn() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.est_output_bytes = 4_000;

        assert!(!state.tick_indicator_repaint_needed(t0 + Duration::from_millis(8), 80));
    }

    #[test]
    fn tick_repaint_reports_complete_to_idle_transition() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.mark_complete("done".to_string(), t0);

        assert!(
            state.tick_indicator_repaint_needed(t0 + STREAMING_COMPLETE_HOLD, 80),
            "dropping the visible complete row must repaint the panel"
        );
    }

    #[test]
    fn note_event_received_resets_byte_estimate_at_turn_boundary() {
        let mut state = fresh_state();
        // First turn — bytes accumulate.
        state.note_event_received(Instant::now());
        state.est_output_bytes = 1_024;
        state.tick(Instant::now());
        // Mark complete — clears the estimate.
        state.mark_complete("done".to_string(), Instant::now());
        assert_eq!(state.est_output_bytes, 0);
        // New turn starts from a clean baseline.
        state.est_output_bytes = 200; // pretend dispatch wrote some pre-tick
        state.note_event_received(Instant::now());
        assert_eq!(
            state.est_output_bytes, 0,
            "turn boundary must zero the estimate"
        );
    }

    #[test]
    fn tick_threads_byte_estimate_into_generating_token_field() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.est_output_bytes = 4_000; // → ~1000 tokens
        state.tick(t0 + Duration::from_millis(10));
        assert!(
            matches!(
                state.streaming_indicator,
                StreamingIndicator::Generating {
                    est_output_tokens: 1_000,
                    ..
                }
            ),
            "expected Generating with 1_000 estimated tokens, got {:?}",
            state.streaming_indicator,
        );
    }

    #[test]
    fn tick_threads_current_tool_use_into_generating_in_flight() {
        let mut state = fresh_state();
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.current_tool_use = Some(ToolUseInFlight {
            tool_name: "bash".to_string(),
            description: Some("listing".to_string()),
        });
        state.tick(t0 + Duration::from_millis(10));
        // Use `matches!` with field-shape patterns so the test does not
        // need expect/panic on the enum variant — clippy::panic and
        // clippy::expect_used are denied workspace-wide.
        let in_flight_ok = matches!(
            &state.streaming_indicator,
            StreamingIndicator::Generating {
                in_flight: Some(t),
                ..
            } if t.tool_name == "bash" && t.description.as_deref() == Some("listing")
        );
        assert!(
            in_flight_ok,
            "expected Generating with in_flight = bash/'listing', got {:?}",
            state.streaming_indicator,
        );
    }

    #[test]
    fn root_provider_usage_replaces_live_status_totals() {
        let mut state = fresh_state();
        let agent_id = state.tab_state.root_id();
        state.record_root_provider_usage(agent_id, 1_000, 2_000);
        state.record_root_provider_usage(agent_id, 3_000, 4_000);

        let status = state.fixed_panel.status_bar();
        assert_eq!(status.input_tokens, 3_000);
        assert_eq!(status.output_tokens, 4_000);
        assert!(!status.input_tokens_estimated);
        assert!(!status.output_tokens_estimated);
        assert_eq!(
            state.usage_totals.get(&agent_id).copied(),
            Some((4_000, 6_000)),
            "agent-row ledger still accumulates actual provider spend"
        );
    }

    #[test]
    fn root_input_estimate_marks_live_status_as_estimated() {
        let mut state = fresh_state();
        state.set_root_input_estimate(12_345);

        let status = state.fixed_panel.status_bar();
        assert_eq!(status.input_tokens, 12_345);
        assert!(status.input_tokens_estimated);
        assert_eq!(status.output_tokens, 0);
        assert!(!status.output_tokens_estimated);
    }

    #[test]
    fn child_usage_delta_does_not_update_live_status_totals() {
        let mut state = fresh_state();
        let child_id = Uuid::new_v4();
        state.record_agent_usage_delta(child_id, 1_000, 2_000);

        let status = state.fixed_panel.status_bar();
        assert_eq!(status.input_tokens, 0);
        assert_eq!(status.output_tokens, 0);
    }

    #[test]
    fn usage_reconcile_records_only_missing_positive_delta() {
        let mut state = fresh_state();
        let agent_id = state.tab_state.root_id();
        state.record_agent_usage_delta(agent_id, 1_000, 2_000);
        state.reconcile_usage_total(agent_id, 1_500, 2_500);
        state.reconcile_usage_total(agent_id, 1_200, 2_100);

        assert_eq!(
            state.usage_totals.get(&agent_id).copied(),
            Some((1_500, 2_500))
        );
        let status = state.fixed_panel.status_bar();
        assert_eq!(status.input_tokens, 0);
        assert_eq!(status.output_tokens, 0);
    }

    #[test]
    fn sync_indicator_into_panel_mirrors_state() {
        let mut state = fresh_state();
        state.note_event_received(Instant::now());
        state.sync_indicator_into_panel();
        // Generating lives in the top chip, so mirroring state should
        // keep the fixed panel at least at its always-present minimum.
        assert!(state.fixed_panel.total_height() >= 4);
    }
}
