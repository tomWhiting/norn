//! Context editing operations: suppress, summarize, inject, compact.
//!
//! These operations mark events without deleting them. The [`EventStore`]
//! is never mutated by context editing — only new events (compactions,
//! injections) are appended.

use std::collections::HashSet;

use crate::error::SessionError;
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

/// Tracks context editing marks applied to session events.
///
/// Maintains three disjoint sets: suppressed event IDs, superseded event IDs,
/// and injected event IDs. None of these mutate the underlying [`EventStore`].
#[derive(Debug, Default)]
pub struct ContextEdits {
    suppressed: HashSet<EventId>,
    superseded: HashSet<EventId>,
    injected: HashSet<EventId>,
}

impl ContextEdits {
    /// Create an empty context edits tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild persisted compaction marks from the append-only store.
    ///
    /// This is idempotent and intentionally only restores compaction
    /// supersession. Suppression and injection marks are live editing state and
    /// are not represented by durable session events.
    pub fn apply_persisted_compactions(&mut self, store: &EventStore) {
        for event in store.events() {
            if let SessionEvent::Compaction {
                replaced_event_ids, ..
            } = event
            {
                self.superseded.extend(replaced_event_ids);
            }
        }
    }

    // -- Suppress (R4) ---------------------------------------------------

    /// Mark an event as suppressed. Suppressed events are excluded from
    /// prompt construction but remain in the store unchanged.
    pub fn suppress(&mut self, event_id: EventId) {
        self.suppressed.insert(event_id);
    }

    /// Check whether an event is suppressed.
    #[must_use]
    pub fn is_suppressed(&self, id: &EventId) -> bool {
        self.suppressed.contains(id)
    }

    // -- Summarize / Compact (R5) ----------------------------------------

    /// Replace a sequence of events with a compaction summary.
    ///
    /// Appends a [`SessionEvent::Compaction`] to `store` and marks the
    /// original `event_ids` as superseded.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EventAppendFailed`] if the compaction event
    /// cannot be appended.
    pub fn summarize(
        &mut self,
        store: &EventStore,
        event_ids: Vec<EventId>,
        summary: String,
    ) -> Result<EventId, SessionError> {
        let compaction = SessionEvent::Compaction {
            base: EventBase::new(None),
            summary,
            replaced_event_ids: event_ids.clone(),
        };
        let compaction_id = store.append(compaction)?;
        for id in event_ids {
            self.superseded.insert(id);
        }
        Ok(compaction_id)
    }

    /// Compact all events before a cut point into a single summary.
    ///
    /// Every event whose insertion position precedes `cut_point` is marked as
    /// superseded, and a [`SessionEvent::Compaction`] is appended.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidEventId`] if `cut_point` is not found
    /// in the store.
    /// Returns [`SessionError::EventAppendFailed`] if the compaction event
    /// cannot be appended.
    pub fn compact(
        &mut self,
        store: &EventStore,
        cut_point: &EventId,
        summary: String,
    ) -> Result<EventId, SessionError> {
        let events = store.events();
        let cut_pos = events
            .iter()
            .position(|e| e.base().id == *cut_point)
            .ok_or_else(|| SessionError::InvalidEventId {
                id: cut_point.to_string(),
            })?;

        let ids_before: Vec<EventId> = events[..cut_pos]
            .iter()
            .map(|e| e.base().id.clone())
            .collect();

        let compaction = SessionEvent::Compaction {
            base: EventBase::new(None),
            summary,
            replaced_event_ids: ids_before.clone(),
        };
        let compaction_id = store.append(compaction)?;
        for id in ids_before {
            self.superseded.insert(id);
        }
        Ok(compaction_id)
    }

    /// Check whether an event has been superseded by a compaction.
    #[must_use]
    pub fn is_superseded(&self, id: &EventId) -> bool {
        self.superseded.contains(id)
    }

    /// Auto-compact older events while retaining the most recent
    /// `keep_turns` assistant turns.
    ///
    /// A *turn* is an [`SessionEvent::AssistantMessage`]. The function
    /// counts back from the end of the store, finds the cut position N+1
    /// turns back, marks every event before the cut as superseded, and
    /// appends a single [`SessionEvent::Compaction`] whose summary is a
    /// JSON object describing the compaction (event count, freed token
    /// estimate, turn range).
    ///
    /// Returns `Ok(None)` when there are not yet `keep_turns + 1`
    /// assistant turns in the store (nothing to compact). Returns the
    /// `EventId` of the appended `Compaction` event on success.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EventAppendFailed`] if either the
    /// compaction event cannot be appended or its summary cannot be
    /// serialised to JSON.
    pub fn auto_compact_keeping_recent_turns(
        &mut self,
        store: &EventStore,
        keep_turns: usize,
        token_estimate_freed: usize,
    ) -> Result<Option<EventId>, SessionError> {
        let events = store.events();
        let assistant_positions: Vec<usize> = events
            .iter()
            .enumerate()
            .filter_map(|(idx, e)| {
                matches!(e, SessionEvent::AssistantMessage { .. }).then_some(idx)
            })
            .collect();
        if assistant_positions.len() <= keep_turns {
            return Ok(None);
        }
        let prior_assistant_idx = assistant_positions[assistant_positions.len() - keep_turns - 1];
        let cut_exclusive = compact_boundary_after_tool_results(&events, prior_assistant_idx);
        let ids_before: Vec<EventId> = events[..cut_exclusive]
            .iter()
            .map(|e| e.base().id.clone())
            .collect();
        if ids_before.is_empty() {
            return Ok(None);
        }
        let start_id = if let Some(id) = ids_before.first() {
            id.to_string()
        } else {
            return Ok(None);
        };
        let end_id = if let Some(id) = ids_before.last() {
            id.to_string()
        } else {
            return Ok(None);
        };
        let summary_json = serde_json::json!({
            "event_count_suppressed": ids_before.len(),
            "token_estimate_freed": token_estimate_freed,
            "turn_range": {
                "start": start_id,
                "end": end_id,
            }
        });
        let summary =
            serde_json::to_string(&summary_json).map_err(|e| SessionError::EventAppendFailed {
                reason: format!("failed to serialise auto-compaction summary: {e}"),
            })?;
        let compaction = SessionEvent::Compaction {
            base: EventBase::new(None),
            summary,
            replaced_event_ids: ids_before.clone(),
        };
        let compaction_id = store.append(compaction)?;
        for id in ids_before {
            self.superseded.insert(id);
        }
        Ok(Some(compaction_id))
    }

    // -- Inject (R6) -----------------------------------------------------

    /// Inject an external event into the session.
    ///
    /// The event is appended to the store normally. Its ID is tracked as
    /// injected so prompt construction can tag it appropriately.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EventAppendFailed`] if the event cannot be
    /// appended.
    pub fn inject(
        &mut self,
        store: &EventStore,
        event: SessionEvent,
    ) -> Result<EventId, SessionError> {
        let id = store.append(event)?;
        self.injected.insert(id.clone());
        Ok(id)
    }

    /// Check whether an event was injected.
    #[must_use]
    pub fn is_injected(&self, id: &EventId) -> bool {
        self.injected.contains(id)
    }
}

fn compact_boundary_after_tool_results(events: &[SessionEvent], assistant_idx: usize) -> usize {
    let call_ids = match events.get(assistant_idx) {
        Some(SessionEvent::AssistantMessage { tool_calls, .. }) => tool_calls
            .iter()
            .map(|call| call.call_id.clone())
            .collect::<HashSet<_>>(),
        _ => HashSet::new(),
    };
    if call_ids.is_empty() {
        return assistant_idx.saturating_add(1);
    }

    let mut unresolved = call_ids;
    let mut idx = assistant_idx.saturating_add(1);
    while idx < events.len() {
        match &events[idx] {
            SessionEvent::ToolResult { tool_call_id, .. } => {
                unresolved.remove(tool_call_id);
                if unresolved.is_empty() {
                    return idx.saturating_add(1);
                }
            }
            SessionEvent::AssistantMessage { .. } => {
                return assistant_idx;
            }
            SessionEvent::UserMessage { .. }
            | SessionEvent::Custom { .. }
            | SessionEvent::ModelChange { .. }
            | SessionEvent::Fork { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::SpokenResponse { .. }
            | SessionEvent::Compaction { .. } => {}
        }
        idx += 1;
    }
    assistant_idx
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use crate::session::events::{EventUsage, ToolCallEvent};

    fn user_msg(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    // -- R4: Suppress tests -----------------------------------------------

    #[test]
    fn suppress_marks_event() {
        let store = EventStore::new();
        let id = store.append(user_msg("hello")).expect("append");
        let mut edits = ContextEdits::new();

        assert!(!edits.is_suppressed(&id));
        edits.suppress(id.clone());
        assert!(edits.is_suppressed(&id));

        let original = store.get(&id).expect("still in store");
        match original {
            SessionEvent::UserMessage { content, .. } => assert_eq!(content, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    // -- R5: Summarize / compact tests ------------------------------------

    #[test]
    fn summarize_supersedes_originals() {
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..5 {
            ids.push(store.append(user_msg(&format!("msg {i}"))).expect("append"));
        }

        let mut edits = ContextEdits::new();
        let target_ids = ids[0..3].to_vec();
        let comp_id = edits
            .summarize(&store, target_ids.clone(), "summary of 0-2".to_owned())
            .expect("summarize");

        for id in &target_ids {
            assert!(edits.is_superseded(id));
        }
        assert!(!edits.is_superseded(&ids[3]));
        assert!(!edits.is_superseded(&ids[4]));

        let comp = store.get(&comp_id).expect("compaction exists");
        match comp {
            SessionEvent::Compaction {
                summary,
                replaced_event_ids,
                ..
            } => {
                assert_eq!(summary, "summary of 0-2");
                assert_eq!(replaced_event_ids.len(), 3);
            }
            _ => panic!("wrong variant"),
        }

        for id in &target_ids {
            assert!(store.get(id).is_some(), "original still in store");
        }
    }

    #[test]
    fn compact_at_cut_point() {
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..10 {
            ids.push(store.append(user_msg(&format!("msg {i}"))).expect("append"));
        }

        let mut edits = ContextEdits::new();
        let comp_id = edits
            .compact(&store, &ids[7], "compacted 0-6".to_owned())
            .expect("compact");

        for id in &ids[0..7] {
            assert!(
                edits.is_superseded(id),
                "event before cut should be superseded"
            );
        }
        assert!(
            !edits.is_superseded(&ids[7]),
            "cut point itself should not be superseded"
        );
        for id in &ids[8..] {
            assert!(
                !edits.is_superseded(id),
                "events after cut should not be superseded"
            );
        }

        let comp = store.get(&comp_id).expect("compaction exists");
        match comp {
            SessionEvent::Compaction {
                replaced_event_ids, ..
            } => {
                assert_eq!(replaced_event_ids.len(), 7);
            }
            _ => panic!("wrong variant"),
        }

        for id in &ids {
            assert!(store.get(id).is_some(), "all originals remain in store");
        }
    }

    #[test]
    fn compact_invalid_cut_point() {
        let store = EventStore::new();
        store.append(user_msg("a")).expect("append");
        let mut edits = ContextEdits::new();
        let result = edits.compact(&store, &EventId::new(), "summary".to_owned());
        assert!(result.is_err());
    }

    #[test]
    fn superseded_separate_from_suppressed() {
        let store = EventStore::new();
        let id1 = store.append(user_msg("a")).expect("append");
        let id2 = store.append(user_msg("b")).expect("append");

        let mut edits = ContextEdits::new();
        edits.suppress(id1.clone());
        edits
            .summarize(&store, vec![id2.clone()], "sum".to_owned())
            .expect("summarize");

        assert!(edits.is_suppressed(&id1));
        assert!(!edits.is_superseded(&id1));
        assert!(edits.is_superseded(&id2));
        assert!(!edits.is_suppressed(&id2));
    }

    // -- N-023 R4: auto_compact_keeping_recent_turns ---------------------

    fn assistant_msg(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }
    }

    fn assistant_tool_call(call_id: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: String::new(),
            thinking: String::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: call_id.to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "a"}),
                kind: crate::provider::request::ToolCallKind::Function,
            }],
            usage: EventUsage::default(),
            stop_reason: "tool_use".to_string(),
            response_id: Some("resp_tool".to_string()),
        }
    }

    fn tool_result(call_id: &str) -> SessionEvent {
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: call_id.to_string(),
            tool_name: "read".to_string(),
            output: serde_json::json!({"ok": true}),
            duration_ms: 1,
        }
    }

    #[test]
    fn auto_compact_keeps_most_recent_turns() {
        let store = EventStore::new();
        for i in 0..25 {
            store
                .append(assistant_msg(&format!("turn {i}")))
                .expect("append");
        }
        let mut edits = ContextEdits::new();
        let result = edits
            .auto_compact_keeping_recent_turns(&store, 10, 4096)
            .expect("compaction");
        let comp_id = result.expect("expected compaction event");

        let comp = store.get(&comp_id).expect("compaction stored");
        let SessionEvent::Compaction {
            replaced_event_ids,
            summary,
            ..
        } = comp
        else {
            panic!("expected Compaction variant");
        };
        // 25 turns; keep last 10 → drop everything from position 0..=14 → 15 ids.
        assert_eq!(replaced_event_ids.len(), 15);
        for id in &replaced_event_ids {
            assert!(edits.is_superseded(id));
        }
        let parsed: serde_json::Value = serde_json::from_str(&summary).expect("summary parses");
        assert_eq!(parsed["event_count_suppressed"], 15);
        assert_eq!(parsed["token_estimate_freed"], 4096);
        assert!(parsed["turn_range"]["start"].is_string());
        assert!(parsed["turn_range"]["end"].is_string());
    }

    #[test]
    fn auto_compact_returns_none_below_threshold() {
        let store = EventStore::new();
        for i in 0..5 {
            store
                .append(assistant_msg(&format!("t{i}")))
                .expect("append");
        }
        let mut edits = ContextEdits::new();
        let result = edits
            .auto_compact_keeping_recent_turns(&store, 10, 0)
            .expect("compaction");
        assert!(result.is_none());
    }

    #[test]
    fn auto_compact_does_not_leave_tool_result_orphaned() {
        let store = EventStore::new();
        let user1 = store.append(user_msg("old")).expect("append");
        let assistant = store
            .append(assistant_tool_call("call_old"))
            .expect("append");
        let result = store.append(tool_result("call_old")).expect("append");
        let retained_user = store.append(user_msg("new")).expect("append");
        let retained_assistant = store.append(assistant_msg("new answer")).expect("append");

        let mut edits = ContextEdits::new();
        edits
            .auto_compact_keeping_recent_turns(&store, 1, 100)
            .expect("compaction")
            .expect("compaction id");

        assert!(edits.is_superseded(&user1));
        assert!(edits.is_superseded(&assistant));
        assert!(edits.is_superseded(&result));
        assert!(!edits.is_superseded(&retained_user));
        assert!(!edits.is_superseded(&retained_assistant));
    }

    #[test]
    fn auto_compact_keeps_interleaved_tool_batch_together() {
        let store = EventStore::new();
        let user1 = store.append(user_msg("old")).expect("append");
        let assistant = store
            .append(assistant_tool_call("call_old"))
            .expect("append");
        let handoff = store.append(user_msg("handoff guidance")).expect("append");
        let result = store.append(tool_result("call_old")).expect("append");
        let retained_assistant = store.append(assistant_msg("new answer")).expect("append");

        let mut edits = ContextEdits::new();
        edits
            .auto_compact_keeping_recent_turns(&store, 1, 100)
            .expect("compaction")
            .expect("compaction id");

        assert!(edits.is_superseded(&user1));
        assert!(edits.is_superseded(&assistant));
        assert!(edits.is_superseded(&handoff));
        assert!(edits.is_superseded(&result));
        assert!(!edits.is_superseded(&retained_assistant));
    }

    #[test]
    fn auto_compact_does_not_suppress_unresolved_tool_batch() {
        let store = EventStore::new();
        let user1 = store.append(user_msg("old")).expect("append");
        let assistant = store
            .append(assistant_tool_call("call_old"))
            .expect("append");
        let retained_assistant = store.append(assistant_msg("new answer")).expect("append");

        let mut edits = ContextEdits::new();
        edits
            .auto_compact_keeping_recent_turns(&store, 1, 100)
            .expect("compaction")
            .expect("compaction id");

        assert!(edits.is_superseded(&user1));
        assert!(!edits.is_superseded(&assistant));
        assert!(!edits.is_superseded(&retained_assistant));
    }

    #[test]
    fn auto_compact_does_not_cross_later_assistant_to_resolve_tool_batch() {
        let store = EventStore::new();
        let user1 = store.append(user_msg("old")).expect("append");
        let assistant = store
            .append(assistant_tool_call("call_old"))
            .expect("append");
        let retained_assistant = store.append(assistant_msg("new answer")).expect("append");
        let late_result = store.append(tool_result("call_old")).expect("append");

        let mut edits = ContextEdits::new();
        edits
            .auto_compact_keeping_recent_turns(&store, 1, 100)
            .expect("compaction")
            .expect("compaction id");

        assert!(edits.is_superseded(&user1));
        assert!(!edits.is_superseded(&assistant));
        assert!(!edits.is_superseded(&retained_assistant));
        assert!(!edits.is_superseded(&late_result));
    }

    // -- R6: Inject tests -------------------------------------------------

    #[test]
    fn inject_appends_and_marks() {
        let store = EventStore::new();
        let mut edits = ContextEdits::new();

        let injected_event = user_msg("injected context");
        let id = edits.inject(&store, injected_event).expect("inject");

        assert!(edits.is_injected(&id));
        assert!(store.get(&id).is_some());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn non_injected_events_not_marked() {
        let store = EventStore::new();
        let edits = ContextEdits::new();
        let id = store.append(user_msg("normal")).expect("append");
        assert!(!edits.is_injected(&id));
    }
}
