//! Multi-agent tab state and `EventStore` replay.
//!
//! [`TabState`] tracks which agent owns the scroll region and which
//! agents are available as background tabs the user can switch to.
//! [`replay_events`] re-renders the last N events of a target agent's
//! [`EventStore`] through the same [`crate::events::render_event`]
//! dispatch the live event loop uses, prefixed with a dim
//! `════════ switched to: {name} ════════` separator (see
//! [`write_switch_separator_and_replay`]).
//!
//! The module owns *state* and *pure rendering helpers* only. Wiring
//! Tab/Enter keystrokes, looking display names up from the
//! [`norn::agent::registry::AgentRegistry`], and triggering
//! [`TabState::remove_agent`] on hold-window expiry all live in NT-011
//! (event loop) and a future NT-009 amendment for the visual highlight.
//!
//! ## Cycle semantics (R2)
//!
//! The cycle order is `[active_agent_id, ..background_agents]`. The
//! first [`TabState::cycle_focus`] from a cleared focus lands on index
//! `0` (the active tab), then advances modulo the list length on each
//! subsequent press. Single-agent sessions (`background_agents.is_empty()`
//! and the active is the only tracked id) cycle to nothing — the
//! method is a no-op and [`TabState::focused_agent_id`] returns `None`.
//!
//! ## Switch semantics (R3)
//!
//! [`TabState::switch_to`] is unconditional with respect to user input:
//! the empty-input gate (`Enter on focused agent only when input buffer
//! is empty`) is the event loop's job. The method itself returns the
//! previously active id when a switch happened so the caller can prove
//! the state transition for tests.

use std::io;

use termina::escape::csi::{Csi, Sgr};
use termina::style::Intensity;
use uuid::Uuid;

use norn::session::store::EventStore;

use crate::events::{DisplayToggles, render_event};
use crate::render::scroll_region::{write_separator, write_to_scroll};
use crate::terminal::caps::TerminalCaps;

/// Default number of events replayed when switching tabs.
///
/// The brief pins 20 — small enough to stay within a typical terminal
/// pane on the slow path (large code-block dumps), large enough to give
/// the user back the immediate context when they switch.
pub const DEFAULT_REPLAY_COUNT: usize = 20;

/// State for the multi-agent tab strip.
///
/// `TabState` does not hold the [`norn::agent::registry::AgentRegistry`]
/// — display names live with the registry and the caller threads it
/// separately. Keeping state to bare [`Uuid`]s lets the type stay
/// `Send + Sync` without an `Arc<RwLock<_>>` on every field.
#[derive(Clone, Debug)]
pub struct TabState {
    root_id: Uuid,
    active_agent_id: Uuid,
    background_agents: Vec<Uuid>,
    focused_index: Option<usize>,
}

impl TabState {
    /// Construct with `root_id` as the initial active tab and an empty
    /// background-agent list.
    #[must_use]
    pub const fn new(root_id: Uuid) -> Self {
        Self {
            root_id,
            active_agent_id: root_id,
            background_agents: Vec::new(),
            focused_index: None,
        }
    }

    /// The id originally registered as the root for this session.
    ///
    /// Stored separately from `active_agent_id` so that
    /// [`Self::remove_agent`] can fall back to the root even after the
    /// user switched the active tab to a child.
    #[must_use]
    pub const fn root_id(&self) -> Uuid {
        self.root_id
    }

    /// The currently active agent — the one whose live output streams
    /// into the scroll region.
    #[must_use]
    pub const fn active_agent_id(&self) -> Uuid {
        self.active_agent_id
    }

    /// Background-agent ids in insertion order.
    #[must_use]
    pub fn background_agents(&self) -> &[Uuid] {
        &self.background_agents
    }

    /// The id of the currently focused agent, if any.
    ///
    /// Focus is independent of `active_agent_id`: it is the cursor
    /// position the next [`Self::cycle_focus`] will advance from. The
    /// visual highlight that exposes focus to the user is wired in
    /// NT-011.
    #[must_use]
    pub fn focused_agent_id(&self) -> Option<Uuid> {
        let i = self.focused_index?;
        self.cycle_list().get(i).copied()
    }

    /// Add a child agent to the background-tab set.
    ///
    /// No-op when `id` is already tracked (active or background) — the
    /// brief explicitly forbids duplicates so the cycle list stays
    /// 1:1 with the agent set.
    pub fn add_agent(&mut self, id: Uuid) {
        if id == self.active_agent_id {
            return;
        }
        if self.background_agents.contains(&id) {
            return;
        }
        self.background_agents.push(id);
    }

    /// Remove an agent from the tab set.
    ///
    /// When the removed id matches the active tab, fall back to the
    /// root id when it is still tracked, then to the first background
    /// agent, otherwise leave the active id unchanged (nothing to
    /// switch to). Focus is cleared because the cycle list shrank.
    pub fn remove_agent(&mut self, id: Uuid) {
        self.background_agents.retain(|x| *x != id);
        if id == self.active_agent_id {
            self.fall_back_active(id);
        }
        self.focused_index = None;
    }

    fn fall_back_active(&mut self, removed: Uuid) {
        if removed != self.root_id
            && let Some(pos) = self
                .background_agents
                .iter()
                .position(|x| *x == self.root_id)
        {
            self.background_agents.remove(pos);
            self.active_agent_id = self.root_id;
            return;
        }
        if !self.background_agents.is_empty() {
            self.active_agent_id = self.background_agents.remove(0);
        }
    }

    /// Advance focus to the next agent in the cycle list.
    ///
    /// Cycle order is `[active, ..background_agents]`. First call from
    /// a cleared focus lands on index `0`. Wraps after the last entry.
    /// Single-tracked-agent sessions are a no-op (focus stays cleared).
    pub fn cycle_focus(&mut self) {
        let len = self.cycle_list().len();
        if len <= 1 {
            self.focused_index = None;
            return;
        }
        let next = match self.focused_index {
            None => 0,
            Some(i) => (i + 1) % len,
        };
        self.focused_index = Some(next);
    }

    /// Switch the active tab to `target`.
    ///
    /// Returns the previously active id when a switch happens. Returns
    /// `None` and is a no-op when `target == active_agent_id`. Focus
    /// is cleared — the cycle order changes when the active id moves.
    ///
    /// The empty-input gate (`Enter on focused agent only when the
    /// input buffer is empty`) is the event loop's responsibility
    /// (NT-011); this method runs the state transition unconditionally.
    pub fn switch_to(&mut self, target: Uuid) -> Option<Uuid> {
        if target == self.active_agent_id {
            return None;
        }
        let previous = self.active_agent_id;
        self.background_agents.retain(|x| *x != target);
        self.background_agents.push(previous);
        self.active_agent_id = target;
        self.focused_index = None;
        Some(previous)
    }

    fn cycle_list(&self) -> Vec<Uuid> {
        let mut v = Vec::with_capacity(self.background_agents.len() + 1);
        v.push(self.active_agent_id);
        v.extend(self.background_agents.iter().copied());
        v
    }
}

/// Replay the last `count` events from `store` through
/// [`render_event`].
///
/// Slices `store.events()` from `len.saturating_sub(count)` so the
/// view is always a valid Rust subslice — no panic when `count`
/// exceeds the store length. Each rendered string is appended via
/// [`write_to_scroll`] so bare `\n`s become `\r\n` for the raw-mode
/// terminal (CO7 — append-only at the cursor).
///
/// `toggles.thinking_visible` controls whether persisted
/// `AssistantMessage.thinking` content surfaces in the scrollback. The
/// underlying renderer ([`crate::events::render_assistant_message`])
/// reads the field from the event itself so replay sees the same
/// thinking the live session recorded, including GPT-style `Thought
/// about ...` summary blocks.
///
/// # Errors
///
/// Returns the first I/O error from `writer`.
pub fn replay_events<W: io::Write>(
    store: &EventStore,
    count: usize,
    caps: &TerminalCaps,
    toggles: DisplayToggles,
    terminal_width: u16,
    writer: &mut W,
) -> io::Result<()> {
    for event in store.last_events(count) {
        let rendered = render_event(&event, caps, toggles, terminal_width);
        write_to_scroll(&rendered, writer)?;
    }
    Ok(())
}

/// Write a dim-styled tab-switch separator to the scroll region.
///
/// Renders `════════ switched to: {agent_name} ════════` bracketed by
/// a dim SGR (entry) and a normal-intensity SGR (exit) so the line
/// reads as muted on the user's terminal. The width-padding behaviour
/// comes from [`write_separator`] — the helper falls back to printing
/// the bare label on its own line when `terminal_width` is too small.
///
/// # Errors
///
/// Returns the first I/O error from `writer`.
pub fn write_switch_separator<W: io::Write>(
    agent_name: &str,
    terminal_width: u16,
    writer: &mut W,
) -> io::Result<()> {
    write!(writer, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Dim)))?;
    let label = format!("switched to: {agent_name}");
    write_separator(&label, terminal_width, writer)?;
    write!(writer, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Normal)))
}

/// Write a tab-switch separator, then replay the last `count` events.
///
/// The separator is always written before any event byte hits the
/// writer — that ordering is the R5 invariant the brief pins.
///
/// # Errors
///
/// Returns the first I/O error from `writer`.
pub fn write_switch_separator_and_replay<W: io::Write>(
    agent_name: &str,
    terminal_width: u16,
    store: &EventStore,
    count: usize,
    caps: &TerminalCaps,
    toggles: DisplayToggles,
    writer: &mut W,
) -> io::Result<()> {
    write_switch_separator(agent_name, terminal_width, writer)?;
    replay_events(store, count, caps, toggles, terminal_width, writer)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::similar_names,
    clippy::too_many_arguments
)]
mod tests {
    use std::collections::HashSet;

    use chrono::Utc;
    use serde_json::json;

    use norn::session::events::{EventBase, EventId, EventUsage, SessionEvent};
    use norn::session::store::EventStore;

    use super::*;

    fn caps() -> TerminalCaps {
        TerminalCaps::baseline()
    }

    fn base() -> EventBase {
        EventBase {
            id: EventId::new(),
            parent_id: None,
            timestamp: Utc::now(),
        }
    }

    fn user(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: base(),
            content: content.to_owned(),
        }
    }

    fn assistant(content: &str, thinking: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: base(),
            content: content.to_owned(),
            thinking: thinking.to_owned(),
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }
    }

    // ---------------- TabState construction (R1) ----------------

    #[test]
    fn new_marks_root_as_active_and_keeps_background_empty() {
        let root = Uuid::new_v4();
        let tabs = TabState::new(root);
        assert_eq!(tabs.active_agent_id(), root);
        assert_eq!(tabs.root_id(), root);
        assert!(tabs.background_agents().is_empty());
        assert!(tabs.focused_agent_id().is_none());
    }

    // ---------------- add_agent (R1) ----------------

    #[test]
    fn add_agent_appends_to_background_in_insertion_order() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(b);
        assert_eq!(tabs.background_agents(), &[a, b]);
    }

    #[test]
    fn add_agent_skips_when_id_matches_active() {
        let root = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(root);
        assert!(tabs.background_agents().is_empty());
    }

    #[test]
    fn add_agent_skips_when_id_already_in_background() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(a);
        assert_eq!(tabs.background_agents(), &[a]);
    }

    #[test]
    fn spawning_a_child_adds_it_to_background_agents() {
        // Brief R1 acceptance test, verbatim.
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(child);
        assert!(tabs.background_agents().contains(&child));
    }

    // ---------------- remove_agent (R1) ----------------

    #[test]
    fn remove_agent_drops_from_background() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(b);
        tabs.remove_agent(a);
        assert_eq!(tabs.background_agents(), &[b]);
        assert_eq!(tabs.active_agent_id(), root);
    }

    #[test]
    fn remove_agent_falls_back_to_root_when_active_removed() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(child);
        // Promote child to active (root pushed to background).
        tabs.switch_to(child);
        assert_eq!(tabs.active_agent_id(), child);
        tabs.remove_agent(child);
        assert_eq!(
            tabs.active_agent_id(),
            root,
            "removing the active child must promote the known root"
        );
        assert!(
            !tabs.background_agents().contains(&root),
            "root must be removed from background when promoted"
        );
    }

    #[test]
    fn remove_active_root_falls_back_to_first_background() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(b);
        // Active is root, root_id is root — removing root falls back
        // to the first background entry.
        tabs.remove_agent(root);
        assert_eq!(tabs.active_agent_id(), a);
        assert_eq!(tabs.background_agents(), &[b]);
    }

    #[test]
    fn remove_only_active_with_no_background_leaves_active_unchanged() {
        let root = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        // Removing the only known agent — nothing to fall back to.
        tabs.remove_agent(root);
        assert_eq!(tabs.active_agent_id(), root);
        assert!(tabs.background_agents().is_empty());
    }

    // ---------------- cycle_focus (R2) ----------------

    #[test]
    fn cycle_focus_single_agent_is_noop() {
        // Brief R2 acceptance: 'Tab on a single-agent session is a no-op'.
        let root = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.cycle_focus();
        assert!(tabs.focused_agent_id().is_none());
    }

    #[test]
    fn cycle_focus_three_agent_tree_visits_all_three() {
        // Brief R2 acceptance: 'Tab on 3-agent tree cycles through all three'.
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(b);

        tabs.cycle_focus();
        let first = tabs.focused_agent_id().expect("first cycle");
        tabs.cycle_focus();
        let second = tabs.focused_agent_id().expect("second cycle");
        tabs.cycle_focus();
        let third = tabs.focused_agent_id().expect("third cycle");

        let visited: HashSet<Uuid> = [first, second, third].into_iter().collect();
        assert_eq!(visited, HashSet::from([root, a, b]));
    }

    #[test]
    fn cycle_focus_wraps_after_last_entry() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);

        tabs.cycle_focus();
        let first = tabs.focused_agent_id();
        tabs.cycle_focus();
        let second = tabs.focused_agent_id();
        tabs.cycle_focus();
        let third = tabs.focused_agent_id();
        assert_eq!(first, third, "third press wraps back to first");
        assert_ne!(first, second, "second press differs from first");
    }

    // ---------------- switch_to (R3) ----------------

    #[test]
    fn switch_to_root_to_child_changes_active_agent_id() {
        // Brief R3 acceptance: 'switching from root to child changes
        // active_agent_id'.
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(child);
        let previous = tabs.switch_to(child);
        assert_eq!(previous, Some(root));
        assert_eq!(tabs.active_agent_id(), child);
        assert!(
            tabs.background_agents().contains(&root),
            "previously active root must move into the background tabs"
        );
    }

    #[test]
    fn switch_to_currently_active_agent_is_noop() {
        // Brief R3 acceptance: 'Enter on the currently active agent is
        // a no-op'.
        let root = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        let result = tabs.switch_to(root);
        assert_eq!(result, None);
        assert_eq!(tabs.active_agent_id(), root);
    }

    #[test]
    fn switch_to_clears_focus() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(child);
        tabs.cycle_focus();
        assert!(tabs.focused_agent_id().is_some());
        tabs.switch_to(child);
        assert!(tabs.focused_agent_id().is_none());
    }

    // ---------------- replay_events (R4) ----------------

    #[test]
    fn replay_last_five_from_ten_includes_only_the_last_five() {
        // Brief R4 acceptance / verification: 'replay last 5 from store
        // with 10 UserMessages contains content of messages 6-10 and
        // not 1-5'.
        let store = EventStore::new();
        for i in 1..=10 {
            store
                .append(user(&format!("msg-{i:02}")))
                .expect("append user");
        }

        let mut buf: Vec<u8> = Vec::new();
        replay_events(&store, 5, &caps(), DisplayToggles::default(), 80, &mut buf).expect("replay");
        let out = String::from_utf8(buf).expect("utf8");
        for i in 1..=5 {
            assert!(
                !out.contains(&format!("msg-{i:02}")),
                "msg-{i:02} must not appear in last-5 replay; out={out:?}"
            );
        }
        for i in 6..=10 {
            assert!(
                out.contains(&format!("msg-{i:02}")),
                "msg-{i:02} must appear in last-5 replay; out={out:?}"
            );
        }
    }

    #[test]
    fn replay_count_larger_than_store_length_renders_all_events() {
        let store = EventStore::new();
        store.append(user("only")).expect("append");
        let mut buf: Vec<u8> = Vec::new();
        replay_events(
            &store,
            DEFAULT_REPLAY_COUNT,
            &caps(),
            DisplayToggles::default(),
            80,
            &mut buf,
        )
        .expect("replay");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(out.contains("only"));
    }

    #[test]
    fn replay_assistant_with_thinking_renders_thinking_when_visible() {
        // Brief R4 acceptance / verification: 'replayed AssistantMessage
        // with thinking="deliberating" contains "thinking: deliberating"
        // when toggles.thinking_visible == true'.
        let store = EventStore::new();
        store
            .append(assistant("answer", "deliberating"))
            .expect("append");

        let toggles = DisplayToggles::default();
        assert!(toggles.thinking_visible);

        let mut buf: Vec<u8> = Vec::new();
        replay_events(&store, 5, &caps(), toggles, 80, &mut buf).expect("replay");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(
            out.contains("thinking: deliberating"),
            "thinking text must surface in replay; out={out:?}"
        );
        assert!(
            out.contains("answer"),
            "assistant content must appear: {out:?}"
        );
    }

    #[test]
    fn replay_assistant_with_markdown_summary_renders_thought_block() {
        let store = EventStore::new();
        store
            .append(assistant(
                "answer",
                "**Creating a markdown table**\n\nI need to prepare an answer.",
            ))
            .expect("append");

        let mut buf: Vec<u8> = Vec::new();
        replay_events(&store, 5, &caps(), DisplayToggles::default(), 80, &mut buf).expect("replay");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(out.contains("Thought about"), "got: {out:?}");
        assert!(out.contains("Creating a markdown table"), "got: {out:?}");
        assert!(out.contains("I need to prepare an answer."), "got: {out:?}");
        assert!(!out.contains("thinking:"), "got: {out:?}");
        assert!(out.contains("answer"), "got: {out:?}");
    }

    #[test]
    fn replay_assistant_without_thinking_visible_omits_thinking() {
        let store = EventStore::new();
        store
            .append(assistant("answer", "deliberating"))
            .expect("append");
        let toggles = DisplayToggles {
            thinking_visible: false,
            secondary_fields_visible: false,
        };
        let mut buf: Vec<u8> = Vec::new();
        replay_events(&store, 5, &caps(), toggles, 80, &mut buf).expect("replay");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(
            !out.contains("deliberating"),
            "thinking text must NOT surface when toggle is off; out={out:?}"
        );
    }

    #[test]
    fn replay_routes_tool_results_through_per_tool_renderer() {
        let store = EventStore::new();
        store
            .append(SessionEvent::ToolResult {
                base: base(),
                tool_call_id: "tc_1".to_owned(),
                tool_name: "bash".to_owned(),
                output: json!({"exit_code": 0, "stdout": "ok\n", "stderr": ""}),
                duration_ms: 12,
            })
            .expect("append");
        let mut buf: Vec<u8> = Vec::new();
        replay_events(&store, 5, &caps(), DisplayToggles::default(), 80, &mut buf).expect("replay");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(
            out.contains("0.01s"),
            "bash renderer duration must appear: {out:?}"
        );
    }

    // ---------------- separator (R5) ----------------

    #[test]
    fn switch_separator_contains_label_box_char_and_dim_sgr() {
        // Brief R5 verification: 'switch separator output contains
        // "switched to: {name}", the "═" box-drawing char, and the dim
        // SGR escape "\x1b[2m"'.
        let mut buf: Vec<u8> = Vec::new();
        write_switch_separator("researcher", 60, &mut buf).expect("separator");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(out.contains("switched to: researcher"), "label: {out:?}");
        assert!(out.contains('═'), "box char: {out:?}");
        assert!(out.contains("\x1b[2m"), "dim SGR: {out:?}");
    }

    #[test]
    fn switch_separator_emits_normal_sgr_to_close_dim() {
        let mut buf: Vec<u8> = Vec::new();
        write_switch_separator("tester", 50, &mut buf).expect("separator");
        let out = String::from_utf8(buf).expect("utf8");
        // termina renders normal-intensity as `\x1b[22m`.
        assert!(out.contains("\x1b[22m"), "normal SGR: {out:?}");
    }

    // ---------------- ordering invariant (R3 + R4 + R5) ----------------

    #[test]
    fn separator_byte_offset_precedes_first_replayed_event() {
        // Brief verification: 'in replay_with_separator output, the
        // "switched to:" substring byte offset is strictly less than
        // the byte offset of the first replayed event's content'.
        let store = EventStore::new();
        store.append(user("hello-replay")).expect("append");

        let mut buf: Vec<u8> = Vec::new();
        write_switch_separator_and_replay(
            "researcher",
            60,
            &store,
            DEFAULT_REPLAY_COUNT,
            &caps(),
            DisplayToggles::default(),
            &mut buf,
        )
        .expect("separator+replay");
        let out = String::from_utf8(buf).expect("utf8");
        let sep_at = out.find("switched to:").expect("separator label present");
        let event_at = out.find("hello-replay").expect("replayed event present");
        assert!(
            sep_at < event_at,
            "separator must precede the first replayed event byte: sep={sep_at} event={event_at} out={out:?}"
        );
    }
}
