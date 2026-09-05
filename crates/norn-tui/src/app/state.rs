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
use norn::session_view::ViewSource;

use super::transcript::Transcript;

use super::active_input::InFlightInputState;
use crate::agents::activity_log::ActivityLog;
use crate::agents::status_line::AgentStatusPanel;
use crate::agents::tabs::TabState;
use crate::events::DisplayToggles;
use crate::input::autocomplete::AutocompletePopup;
use crate::input::editor::InputEditor;
use crate::input::history::InputHistory;
use crate::render::fixed_panel::{FixedPanel, StatusBar};
use crate::render::streaming_indicator::{StreamingIndicator, ToolUseInFlight};
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

/// Central TUI state.
///
/// Every field is owned (no borrows of the runtime) so `AppState` can be
/// constructed once at startup and threaded through the event loop by
/// `&mut`.
pub struct AppState {
    /// Retained semantic state bound to the actual store/agent source.
    pub transcript: Transcript,
    /// Retained full-screen geometry, focus and presentation cache.
    pub screen: super::render::ScreenState,
    /// Accepted explicit exports remain supervised across view/source replacement.
    pub(in crate::app) export_tasks:
        tokio::task::JoinSet<super::view_actions::reading::ExportResult>,
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
    /// Monotonic whole-turn admission instant, preserved across provider responses and retries.
    /// An isolated event consumer without an admission uses its first observed event;
    /// completion expiry clears the instant only once the root turn is no longer running.
    pub turn_start: Option<Instant>,
    /// Wall-clock instant the streaming indicator transitioned to
    /// [`StreamingIndicator::Complete`]. The render tick uses this to
    /// drop the indicator back to [`StreamingIndicator::Idle`] after
    /// [`STREAMING_COMPLETE_HOLD`].
    pub complete_at: Option<Instant>,
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
    /// Activity log — backing ring of recent tool-call initiations.
    /// Dispatch still records entries for diagnostics/replay, while the
    /// fixed panel folds live work into per-agent rows.
    pub activity_log: ActivityLog,
    /// Usage totals already reflected in per-agent rows, keyed by agent
    /// id. Provider `Done` events arrive as deltas, while final
    /// step/lifecycle records carry cumulative totals; this cache lets
    /// the TUI reconcile the latter without double-counting the former.
    usage_totals: HashMap<Uuid, (u64, u64)>,
    /// Actual provider usage accumulated during the current root turn.
    ///
    /// The top divider displays this turn-scoped total plus any currently
    /// in-flight input/output estimate. It is deliberately separate from
    /// [`Self::usage_totals`], which is the session-cumulative ledger used
    /// for per-agent rows.
    live_root_usage: (u64, u64),
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
        source: ViewSource,
        status_bar: StatusBar,
    ) -> Self {
        let root_id = source.agent_id;
        Self {
            transcript: Transcript::new(source.clone()),
            screen: super::render::ScreenState::new(source),
            export_tasks: tokio::task::JoinSet::new(),
            input_editor: InputEditor::new(history),
            display_toggles: DisplayToggles::default(),
            verbosity: VerbosityState::default(),
            streaming_indicator: StreamingIndicator::Idle,
            agent_panel: AgentStatusPanel::new(registry),
            tab_state: TabState::new(root_id),
            fixed_panel: FixedPanel::new(status_bar),
            terminal_caps: caps,
            turn_start: None,
            complete_at: None,
            autocomplete: None,
            est_output_bytes: 0,
            current_tool_use: None,
            activity_log: ActivityLog::new(),
            usage_totals: HashMap::new(),
            live_root_usage: (0, 0),
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
    ///
    /// A retry wait counts as a boundary: the replayed attempt re-streams
    /// the turn from the start, so the failed attempt's byte estimate and
    /// in-flight tool are discarded here exactly as they are between
    /// turns — and the retry row is replaced by the live generating
    /// state, which is how the wait disappears the moment the next
    /// attempt produces anything.
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

    /// Record the provider-retry marker the engine emits immediately
    /// BEFORE the backoff wait it announces.
    ///
    /// The indicator becomes the retry row for the duration of the wait,
    /// counting down from `delay` against `now`. The turn is not over —
    /// `turn_start` is preserved so the divider keeps its elapsed segment
    /// — and any completion hold is cleared, because a retry means the
    /// turn is running again.
    ///
    /// `error_class` is the engine's taxonomy label, carried through
    /// verbatim; this method never sees provider free text.
    pub fn note_retry(
        &mut self,
        attempt: u32,
        max_attempts: Option<u32>,
        delay: Duration,
        error_class: String,
        now: Instant,
    ) {
        let start = self.turn_start.unwrap_or(now);
        self.turn_start = Some(start);
        self.complete_at = None;
        // The failed attempt's partial output never lands in the
        // transcript, so its running estimate and in-flight tool go with
        // it — the replay starts the turn again from the top.
        self.est_output_bytes = 0;
        self.current_tool_use = None;
        self.streaming_indicator = StreamingIndicator::Retrying {
            attempt,
            max_attempts,
            error_class,
            // A delay that overflows the monotonic clock (only reachable
            // from an absurd `delay_ms`) degrades the countdown to "now"
            // rather than wrapping into the past; the announced
            // `remaining` below is still painted as the engine stated it.
            wait_until: now.checked_add(delay).unwrap_or(now),
            remaining: delay,
            elapsed: now.saturating_duration_since(start),
        };
    }

    /// Refresh the streaming indicator's elapsed time on a render tick.
    ///
    /// While generating, recomputes the elapsed time against `now` and
    /// refreshes the token estimate + in-flight tool snapshot from the
    /// dispatch-layer state. While retrying, counts the announced wait
    /// down against `now` (and keeps the turn's elapsed current) so the
    /// row stays honest for a wait that can be minutes long. While
    /// complete, transitions back to idle once [`STREAMING_COMPLETE_HOLD`]
    /// has passed. While idle, this is a no-op.
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
            StreamingIndicator::Retrying {
                attempt,
                max_attempts,
                ref error_class,
                wait_until,
                ..
            } => {
                let error_class = error_class.clone();
                let elapsed = self
                    .turn_start
                    .map_or(Duration::ZERO, |start| now.saturating_duration_since(start));
                self.streaming_indicator = StreamingIndicator::Retrying {
                    attempt,
                    max_attempts,
                    error_class,
                    wait_until,
                    remaining: wait_until.saturating_duration_since(now),
                    elapsed,
                };
            }
            StreamingIndicator::Complete { .. } => {
                if let Some(at) = self.complete_at
                    && now.saturating_duration_since(at) >= STREAMING_COMPLETE_HOLD
                {
                    self.streaming_indicator = StreamingIndicator::Idle;
                    self.complete_at = None;
                    if !self.in_flight_input.is_running() {
                        self.turn_start = None;
                    }
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
        status.input_tokens = self.live_root_usage.0.saturating_add(input_tokens);
        status.input_tokens_estimated = true;
        status.output_tokens = self.live_root_usage.1;
        status.output_tokens_estimated = false;
    }

    /// Add one root usage delta to the root agent's cumulative row total
    /// and the current root turn's live top-chip total.
    pub fn record_root_provider_usage(
        &mut self,
        agent_id: Uuid,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        self.record_agent_usage_delta(agent_id, input_tokens, output_tokens);
        self.live_root_usage.0 = self.live_root_usage.0.saturating_add(input_tokens);
        self.live_root_usage.1 = self.live_root_usage.1.saturating_add(output_tokens);
        let status = self.fixed_panel.status_bar_mut();
        status.input_tokens = self.live_root_usage.0;
        status.input_tokens_estimated = false;
        status.output_tokens = self.live_root_usage.1;
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

    /// Reconcile this root turn against its own observed provider usage, then publish actual totals.
    /// Earlier turns remain in the agent ledger and never offset a later equal-cost turn.
    pub fn reconcile_root_turn_usage(&mut self, input_tokens: u64, output_tokens: u64) {
        let delta_input = input_tokens.saturating_sub(self.live_root_usage.0);
        let delta_output = output_tokens.saturating_sub(self.live_root_usage.1);
        self.record_agent_usage_delta(self.tab_state.root_id(), delta_input, delta_output);
        self.live_root_usage = (input_tokens, output_tokens);
        let status = self.fixed_panel.status_bar_mut();
        status.input_tokens = input_tokens;
        status.output_tokens = output_tokens;
        status.input_tokens_estimated = false;
        status.output_tokens_estimated = false;
    }

    /// Reset the top-chip live counters for a new root turn.
    pub fn reset_live_usage(&mut self) {
        self.live_root_usage = (0, 0);
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
pub(crate) fn test_view_source(agent_id: Uuid) -> ViewSource {
    ViewSource {
        session: norn::session_view::SessionIdentity::Ephemeral(Uuid::new_v4()),
        agent_id,
        parent_agent_id: None,
        store_generation: Uuid::new_v4(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn new_state_has_default_subsystems() -> Result<(), Box<dyn std::error::Error>> {
        let state = fresh_state()?;
        assert!(state.input_editor.is_empty());
        assert_eq!(state.display_toggles, DisplayToggles::default());
        assert_eq!(state.verbosity, VerbosityState::Expanded);
        assert!(matches!(
            state.streaming_indicator,
            StreamingIndicator::Idle
        ));
        assert!(state.turn_start.is_none());
        assert!(state.complete_at.is_none());
        assert_eq!(state.transcript.projection.items().len(), 0);
        Ok(())
    }

    #[test]
    fn note_event_received_transitions_idle_to_generating() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut state = fresh_state()?;
        state.note_event_received(Instant::now());
        assert!(matches!(
            state.streaming_indicator,
            StreamingIndicator::Generating { .. }
        ));
        assert!(state.turn_start.is_some());
        Ok(())
    }

    #[test]
    fn note_event_received_keeps_turn_start_across_calls() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_event_received(t0);
        let first = state.turn_start;
        state.note_event_received(t0 + Duration::from_millis(500));
        assert_eq!(state.turn_start, first);
        Ok(())
    }

    #[test]
    fn mark_complete_transitions_to_complete() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let now = Instant::now();
        state.note_event_received(now);
        state.mark_complete("[1 in / 2 out, 0.5s]".to_string(), now);
        assert!(matches!(
            state.streaming_indicator,
            StreamingIndicator::Complete { .. }
        ));
        assert!(state.complete_at.is_some());
        Ok(())
    }

    #[test]
    fn tick_drops_complete_to_idle_after_hold() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let now = Instant::now();
        state.note_event_received(now);
        state.mark_complete("done".to_string(), now);
        // Just before the hold expires — still complete.
        state.tick(
            (now + STREAMING_COMPLETE_HOLD)
                .checked_sub(Duration::from_millis(1))
                .ok_or("fixture instant underflow")?,
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
        Ok(())
    }

    #[test]
    fn tick_advances_generating_elapsed() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
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
        Ok(())
    }

    #[test]
    fn tick_repaint_ignores_subsecond_elapsed() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_event_received(t0);

        assert!(!state.tick_indicator_repaint_needed(t0 + Duration::from_millis(8), 80));
        Ok(())
    }

    #[test]
    fn tick_repaint_reports_elapsed_second_boundary() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_event_received(t0);

        assert!(state.tick_indicator_repaint_needed(t0 + Duration::from_secs(1), 80));
        Ok(())
    }

    #[test]
    fn tick_repaint_ignores_token_estimate_churn() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.est_output_bytes = 4_000;

        assert!(!state.tick_indicator_repaint_needed(t0 + Duration::from_millis(8), 80));
        Ok(())
    }

    #[test]
    fn tick_repaint_reports_complete_to_idle_transition() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.mark_complete("done".to_string(), t0);

        assert!(
            state.tick_indicator_repaint_needed(t0 + STREAMING_COMPLETE_HOLD, 80),
            "dropping the visible complete row must repaint the panel"
        );
        Ok(())
    }

    #[test]
    fn note_event_received_resets_byte_estimate_at_turn_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
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
        Ok(())
    }

    #[test]
    fn tick_threads_byte_estimate_into_generating_token_field()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
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
        Ok(())
    }

    #[test]
    fn tick_threads_current_tool_use_into_generating_in_flight()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
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
        Ok(())
    }

    #[test]
    fn root_provider_usage_accumulates_live_status_totals() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut state = fresh_state()?;
        let agent_id = state.tab_state.root_id();
        state.record_root_provider_usage(agent_id, 1_000, 2_000);
        state.record_root_provider_usage(agent_id, 3_000, 4_000);

        let status = state.fixed_panel.status_bar();
        assert_eq!(status.input_tokens, 4_000);
        assert_eq!(status.output_tokens, 6_000);
        assert!(!status.input_tokens_estimated);
        assert!(!status.output_tokens_estimated);
        assert_eq!(
            state.usage_totals.get(&agent_id).copied(),
            Some((4_000, 6_000)),
            "agent-row ledger still accumulates actual provider spend"
        );
        Ok(())
    }

    #[test]
    fn root_input_estimate_preserves_turn_output_so_far() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut state = fresh_state()?;
        let agent_id = state.tab_state.root_id();
        state.record_root_provider_usage(agent_id, 1_000, 2_000);
        state.set_root_input_estimate(12_345);

        let status = state.fixed_panel.status_bar();
        assert_eq!(status.input_tokens, 13_345);
        assert!(status.input_tokens_estimated);
        assert_eq!(status.output_tokens, 2_000);
        assert!(!status.output_tokens_estimated);
        Ok(())
    }

    #[test]
    fn child_usage_delta_does_not_update_live_status_totals()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let child_id = Uuid::new_v4();
        state.record_agent_usage_delta(child_id, 1_000, 2_000);

        let status = state.fixed_panel.status_bar();
        assert_eq!(status.input_tokens, 0);
        assert_eq!(status.output_tokens, 0);
        Ok(())
    }

    #[test]
    fn usage_reconcile_records_only_missing_positive_delta()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
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
        Ok(())
    }

    // ---------------- Retry visibility (C6 / DESIGN D8) ----------------

    /// The marker arrives BEFORE the wait, so the row must show the full
    /// announced delay immediately — and keep the turn running.
    #[test]
    fn note_retry_shows_the_announced_wait_and_keeps_the_turn_alive()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.note_retry(
            3,
            Some(5),
            Duration::from_secs(8),
            "server_error".to_string(),
            t0 + Duration::from_secs(2),
        );

        let shown = matches!(
            &state.streaming_indicator,
            StreamingIndicator::Retrying {
                attempt: 3,
                max_attempts: Some(5),
                error_class,
                remaining,
                ..
            } if error_class == "server_error" && *remaining == Duration::from_secs(8)
        );
        assert!(
            shown,
            "expected the announced 8s wait, got {:?}",
            state.streaming_indicator
        );
        assert_eq!(
            state.turn_start,
            Some(t0),
            "a retry is the same turn, not a new one"
        );
        assert!(state.complete_at.is_none());
        Ok(())
    }

    /// A long wait counts down on the render tick instead of freezing at
    /// the announced value.
    #[test]
    fn tick_counts_the_retry_wait_down_and_then_reads_as_now()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.note_retry(2, None, Duration::from_secs(30), "timeout".to_string(), t0);

        state.tick(t0 + Duration::from_secs(20));
        let counted_down = matches!(
            &state.streaming_indicator,
            StreamingIndicator::Retrying { remaining, elapsed, .. }
                if *remaining == Duration::from_secs(10) && *elapsed == Duration::from_secs(20)
        );
        assert!(
            counted_down,
            "expected 10s remaining after 20s, got {:?}",
            state.streaming_indicator
        );

        state.tick(t0 + Duration::from_secs(45));
        let elapsed_wait = matches!(
            &state.streaming_indicator,
            StreamingIndicator::Retrying { remaining, .. } if remaining.is_zero()
        );
        assert!(
            elapsed_wait,
            "a wait that has run out must not go negative or vanish: {:?}",
            state.streaming_indicator
        );
        Ok(())
    }

    /// Each whole second of the countdown repaints the panel; sub-second
    /// tick noise does not.
    #[test]
    fn retry_countdown_repaints_once_per_second() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_retry(2, None, Duration::from_secs(30), "timeout".to_string(), t0);

        assert!(
            !state.tick_indicator_repaint_needed(t0 + Duration::from_millis(8), 80),
            "sub-second countdown movement must not repaint"
        );
        assert!(
            state.tick_indicator_repaint_needed(t0 + Duration::from_secs(1), 80),
            "each second of the wait must repaint the countdown"
        );
        Ok(())
    }

    /// The wait vanishes the moment the replayed attempt produces
    /// anything — and the failed attempt's estimate goes with it.
    #[test]
    fn the_next_attempts_first_event_clears_the_retry_row() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_event_received(t0);
        state.est_output_bytes = 4_000;
        state.note_retry(2, None, Duration::from_secs(5), "timeout".to_string(), t0);
        assert_eq!(
            state.est_output_bytes, 0,
            "the failed attempt's output never landed; its estimate must not carry over"
        );

        state.note_event_received(t0 + Duration::from_secs(5));
        assert!(
            matches!(
                state.streaming_indicator,
                StreamingIndicator::Generating { .. }
            ),
            "got {:?}",
            state.streaming_indicator
        );
        Ok(())
    }

    /// Success ends the turn: the retry row is replaced by the usage
    /// summary, never left behind.
    #[test]
    fn completion_replaces_the_retry_row() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let t0 = Instant::now();
        state.note_retry(2, None, Duration::from_secs(5), "timeout".to_string(), t0);
        state.mark_complete("[1 in / 2 out, 0.5s]".to_string(), t0);
        assert!(matches!(
            state.streaming_indicator,
            StreamingIndicator::Complete { .. }
        ));
        Ok(())
    }

    #[test]
    fn sync_indicator_into_panel_mirrors_state() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        state.note_event_received(Instant::now());
        state.sync_indicator_into_panel();
        // Generating lives in the top chip, so mirroring state should
        // keep the fixed panel at least at its always-present minimum.
        assert!(state.fixed_panel.total_height() >= 4);
        Ok(())
    }
    #[test]
    fn final_usage_counts_equal_successive_turns_and_only_missing_live_amounts()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let root = state.tab_state.root_id();
        state.reconcile_root_turn_usage(3, 4);
        state.reconcile_root_turn_usage(3, 4);
        assert_eq!(state.usage_totals.get(&root), Some(&(3, 4)));
        state.reset_live_usage();
        state.set_root_input_estimate(99);
        state.record_root_provider_usage(root, 1, 2);
        state.reconcile_root_turn_usage(3, 4);
        assert_eq!(
            state.usage_totals.get(&root),
            Some(&(6, 8)),
            "second equal-cost turn must add its own missing usage"
        );
        assert_eq!(state.fixed_panel.status_bar().input_tokens, 3);
        assert_eq!(state.fixed_panel.status_bar().output_tokens, 4);
        assert!(!state.fixed_panel.status_bar().input_tokens_estimated);
        assert!(!state.fixed_panel.status_bar().output_tokens_estimated);
        state.reset_live_usage();
        state.reconcile_root_turn_usage(3, 4);
        assert_eq!(
            state.usage_totals.get(&root),
            Some(&(9, 12)),
            "entirely missed live usage still counts after prior turns"
        );
        Ok(())
    }
    #[test]
    fn running_turn_keeps_admission_time_across_response_hold_and_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let admitted = Instant::now();
        state.turn_start = Some(admitted);
        state.in_flight_input.set_running(true);
        state.note_event_received(admitted + Duration::from_secs(1));
        state.mark_complete(
            "response completed".to_owned(),
            admitted + Duration::from_secs(2),
        );
        let later = admitted + Duration::from_secs(2) + STREAMING_COMPLETE_HOLD;
        state.tick(later);
        assert_eq!(state.turn_start, Some(admitted));
        state.note_retry(2, None, Duration::from_secs(1), "fixture".to_owned(), later);
        assert_eq!(state.turn_start, Some(admitted));
        state.note_event_received(later + Duration::from_secs(1));
        assert_eq!(state.turn_start, Some(admitted));
        state.in_flight_input.set_running(false);
        state.mark_complete("turn completed".to_owned(), later + Duration::from_secs(2));
        state.tick(later + Duration::from_secs(2) + STREAMING_COMPLETE_HOLD);
        assert!(state.turn_start.is_none());
        Ok(())
    }
    #[tokio::test]
    async fn buffered_duplicate_done_cannot_recount_usage_after_publication()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::app::dispatch::{finalise_turn, handle_agent_event};
        use norn::agent_loop::runner::{AgentStepRequest, ToolExecutor, run_agent_step};
        use norn::agent_loop::{LoopContext, config::AgentLoopConfig};
        use norn::provider::agent_event::AgentEventKind;
        use norn::provider::{AgentEventSender, StopReason, mock::MockProvider};
        use norn::provider::{ProviderEvent, Usage};
        use norn::session::{EventStore, SessionBinding};
        let mut state = fresh_state()?;
        let root_id = state.tab_state.root_id();
        let store = EventStore::new();
        let source = store.bind_view_source(&SessionBinding::ephemeral_root(), root_id, None)?;
        state.transcript = crate::app::transcript::Transcript::new(source);
        let input = crate::app::notices::input(&mut state, "You · submitted", "fixture prompt")?;
        let model = norn::model_selection::ModelRuntime::new(
            None,
            "gpt-5.5",
            Some(272_000),
            None,
            None,
            std::collections::BTreeMap::new(),
        )?;
        // Explicit fixture capacity is larger than the finite scripted native event sequence.
        let (tx, mut receiver) = tokio::sync::broadcast::channel(32);
        let sender = state.transcript.observe_execution(
            &AgentEventSender::new(tx, root_id, "root".to_owned()),
            &store,
            &model,
            Some(input),
        )?;
        let provider = MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "fixture answer".to_owned(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                response_id: None,
                usage: Usage {
                    input_tokens: 3,
                    output_tokens: 4,
                    ..Usage::default()
                },
            },
        ]]);
        let executor: Arc<dyn ToolExecutor> = Arc::new(norn::tool::ToolRegistry::new());
        let mut context = LoopContext::new("deterministic fixture");
        let result = run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &executor,
            store: &store,
            user_prompt: "fixture prompt",
            tools: &[],
            output_schema: None,
            model: "gpt-5.5",
            config: &AgentLoopConfig::default(),
            event_tx: Some(&sender),
            inbound: None,
            loop_context: &mut context,
            cancel: None,
        })
        .await?;
        let mut duplicate = None;
        loop {
            match receiver.try_recv() {
                Ok(event) => {
                    if matches!(&event.event, AgentEventKind::Observed(observed) if matches!(observed.event(), AgentEventKind::Provider(ProviderEvent::Done { .. })))
                    {
                        duplicate = Some(event.clone());
                    }
                    handle_agent_event(&mut state, event)?;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(error) => return Err(error.into()),
            }
        }
        handle_agent_event(&mut state, duplicate.ok_or("missing actual Done envelope")?)?;
        assert_eq!(
            state.usage_totals.get(&root_id),
            None,
            "retired attempt data must not create live usage side effects"
        );
        finalise_turn(&mut state, Some(Ok(result)))?;
        assert_eq!(state.usage_totals.get(&root_id), Some(&(3, 4)));
        assert_eq!(state.fixed_panel.status_bar().input_tokens, 3);
        assert_eq!(state.fixed_panel.status_bar().output_tokens, 4);
        Ok(())
    }
}
