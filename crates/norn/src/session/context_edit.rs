//! Context editing operations: suppress, summarize, inject, compact.
//!
//! These operations mark events without deleting them. The [`EventStore`]
//! is never mutated by context editing — only new events (compactions,
//! injections, and the durable
//! [`ContextMark`](crate::session::events::SessionEvent::ContextMark)
//! twins of suppress/inject marks) are appended. Every mark is durable at
//! the moment it is applied, so a resumed session rebuilds the identical
//! prompt view via [`ContextEdits::apply_persisted_marks`] (or the
//! equivalent single-pass
//! [`ReplayArtifacts`](crate::session::ReplayArtifacts) restorers).

use std::collections::{BTreeMap, HashSet};

use crate::error::SessionError;
use crate::session::events::{ContextMarkKind, EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

/// Result of a successful [`ContextEdits::auto_compact_keeping_recent_turns`]
/// call.
///
/// Carries everything a live consumer (the agent loop) needs to apply the
/// compaction to an in-flight prompt without re-deriving state: the ID of the
/// appended [`SessionEvent::Compaction`] and the IDs of the events this
/// specific compaction newly hid from prompt construction.
#[derive(Debug)]
pub struct AutoCompactionOutcome {
    /// ID of the appended [`SessionEvent::Compaction`] event.
    pub compaction_id: EventId,
    /// Events that were visible in the prompt view before this compaction
    /// and are superseded by it. Excludes events that were already
    /// superseded by an earlier compaction or suppressed (those produced no
    /// prompt content to begin with). Ordered by store insertion order.
    pub newly_superseded: Vec<EventId>,
}

/// A computed auto-compaction cut that has not yet been committed.
///
/// Produced by [`ContextEdits::plan_auto_compaction`] and consumed by
/// [`ContextEdits::commit_compaction_plan`]. The two-phase split exists so
/// a caller that owns a provider handle (the agent loop) can generate an
/// LLM-written summary of the events about to be elided *before* the
/// compaction record is appended, while callers without one (manual
/// `/compact` paths) commit the mechanical digest directly.
#[derive(Debug)]
pub struct CompactionPlan {
    /// Exclusive end of the replaced span in store insertion order.
    cut_exclusive: usize,
    /// IDs of every event the compaction will supersede.
    replaced_ids: Vec<EventId>,
    /// Subset of `replaced_ids` that was still visible in the prompt view
    /// when the plan was computed.
    newly_superseded: Vec<EventId>,
}

impl CompactionPlan {
    /// Exclusive end of the replaced span in store insertion order.
    #[must_use]
    pub const fn cut_exclusive(&self) -> usize {
        self.cut_exclusive
    }

    /// IDs of every event the compaction will supersede.
    #[must_use]
    pub fn replaced_ids(&self) -> &[EventId] {
        &self.replaced_ids
    }

    /// Events still visible in the prompt view when the plan was computed.
    #[must_use]
    pub fn newly_superseded(&self) -> &[EventId] {
        &self.newly_superseded
    }
}

/// Tracks context editing marks applied to session events.
///
/// Maintains three disjoint sets: suppressed event IDs, superseded event IDs,
/// and injected event IDs. None of these mutate the underlying [`EventStore`].
///
/// Also carries the **usage floor**: the provider-reported token footprint
/// (`input_tokens + output_tokens`) of the last completed provider call.
/// The client-side character estimate cannot see request content the
/// provider re-bills on every call (e.g. replayed encrypted reasoning items
/// on stateless Responses backends), while the provider's own bill can
/// never understate the next request — it contains at least what the last
/// one did. The auto-compaction trigger and the advisory token warning
/// therefore anchor on `max(estimate, usage_floor)`.
///
/// The floor lives here — on the same struct that owns every
/// conversation-shrinking mutation — so the critical invariant is
/// structural: **any mutation that removes content from the prompt view
/// clears the floor.** A stale floor across a shrink would re-fire the
/// compaction trigger on every step regardless of the (now smaller)
/// conversation. Growth (injection, appends) never clears: the floor
/// remains a valid lower bound for a request that only gained content.
#[derive(Debug, Default)]
pub struct ContextEdits {
    suppressed: HashSet<EventId>,
    superseded: HashSet<EventId>,
    injected: HashSet<EventId>,
    usage_floor: Option<u64>,
}

impl ContextEdits {
    /// Create an empty context edits tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // -- Usage floor -------------------------------------------------------

    /// Record the provider-reported token footprint of the last completed
    /// provider call: its `input_tokens + output_tokens`. Overwrites any
    /// previous floor — "last call's report" semantics, so a provider-side
    /// context reduction (server compaction) is tracked truthfully.
    ///
    /// Deliberately **not** seeded from persisted events on session resume.
    /// The floor is live, per-request accounting, not persisted state: it is
    /// the last provider call's own bill, and no such call has happened yet
    /// on a fresh resume. Seeding a synthetic floor from the persisted
    /// history would be a guess, and a guess that ran high would re-fire the
    /// trigger every step. Instead a resumed session starts with a
    /// conservative cold start — the floor is `None` and the client estimate
    /// governs alone. That estimate now counts the persisted reasoning items
    /// the request will replay (see
    /// [`estimate_prompt_tokens`](crate::agent_loop::tokens::estimate_prompt_tokens)),
    /// so it does not under-count the resumed prompt; the first live provider
    /// call then establishes the true floor.
    pub fn set_usage_floor(&mut self, tokens: u64) {
        self.usage_floor = Some(tokens);
    }

    /// The provider-reported token floor for the next request, when a live
    /// provider call has established one since the last prompt-view shrink.
    #[must_use]
    pub const fn usage_floor(&self) -> Option<u64> {
        self.usage_floor
    }

    /// Clear the usage floor. Invoked by every mutation on this struct
    /// that shrinks the prompt view (suppression and every supersession
    /// path): after a shrink the estimate reflects the smaller
    /// conversation but a stale floor does not, and keeping it would
    /// re-fire the auto-compaction trigger on every subsequent step.
    fn clear_usage_floor(&mut self) {
        self.usage_floor = None;
    }

    /// Restore compaction supersession marks from ids that were already
    /// derived elsewhere — typically
    /// [`ReplayArtifacts::superseded_event_ids`](crate::session::ReplayArtifacts::superseded_event_ids)
    /// from a single-pass session replay. Idempotent.
    pub fn mark_superseded(&mut self, ids: impl IntoIterator<Item = EventId>) {
        self.superseded.extend(ids);
        // Restoration happens before any live provider call, so this is a
        // no-op there (floor is still None) — but the invariant stays
        // total: growing the superseded set shrinks the prompt view.
        self.clear_usage_floor();
    }

    /// Restore suppression marks from ids that were already derived
    /// elsewhere — typically
    /// [`ReplayArtifacts::suppressed_event_ids`](crate::session::ReplayArtifacts::suppressed_event_ids)
    /// from a single-pass session replay. Idempotent. Does **not** append
    /// new [`SessionEvent::ContextMark`] events: the marks being restored
    /// are already durable.
    pub fn mark_suppressed(&mut self, ids: impl IntoIterator<Item = EventId>) {
        self.suppressed.extend(ids);
        // Same invariant as `mark_superseded`: growing the suppressed set
        // shrinks the prompt view, so any established floor must go.
        self.clear_usage_floor();
    }

    /// Restore injection marks from ids that were already derived
    /// elsewhere — typically
    /// [`ReplayArtifacts::injected_event_ids`](crate::session::ReplayArtifacts::injected_event_ids)
    /// from a single-pass session replay. Idempotent. Injection tags never
    /// shrink the prompt view, so the usage floor is untouched.
    pub fn mark_injected(&mut self, ids: impl IntoIterator<Item = EventId>) {
        self.injected.extend(ids);
    }

    /// Rebuild every persisted context-edit mark from the append-only
    /// store: compaction supersession (from [`SessionEvent::Compaction`]
    /// `replaced_event_ids`) plus suppression and injection marks (from
    /// their durable [`SessionEvent::ContextMark`] twins). Idempotent.
    ///
    /// Callers that already hold a single-pass
    /// [`ReplayArtifacts`](crate::session::ReplayArtifacts) should pass its
    /// `superseded_event_ids` / `suppressed_event_ids` /
    /// `injected_event_ids` to the `mark_*` restorers instead — this
    /// method walks the whole store again.
    pub fn apply_persisted_marks(&mut self, store: &EventStore) {
        let mut view_shrank = false;
        store.with_events(|events| {
            for event in events {
                match event {
                    SessionEvent::Compaction {
                        replaced_event_ids, ..
                    } => {
                        view_shrank |= !replaced_event_ids.is_empty();
                        self.superseded.extend(replaced_event_ids.iter().cloned());
                    }
                    SessionEvent::ContextMark {
                        mark,
                        target_event_id,
                        ..
                    } => match mark {
                        ContextMarkKind::Suppress => {
                            view_shrank = true;
                            self.suppressed.insert(target_event_id.clone());
                        }
                        ContextMarkKind::Inject => {
                            self.injected.insert(target_event_id.clone());
                        }
                    },
                    SessionEvent::UserMessage { .. }
                    | SessionEvent::AssistantMessage { .. }
                    | SessionEvent::SpokenResponse { .. }
                    | SessionEvent::ToolResult { .. }
                    | SessionEvent::ModelChange { .. }
                    | SessionEvent::ChildBranch { .. }
                    | SessionEvent::ForkComplete { .. }
                    | SessionEvent::Label { .. }
                    | SessionEvent::Custom { .. }
                    | SessionEvent::RuleInjection { .. } => {}
                }
            }
        });
        // Same floor invariant as the live edit surfaces: supersession and
        // suppression marks shrink the prompt view, so an established usage
        // floor must go — but only when such a mark was actually restored.
        // A store without them (including the runner's one-time first-step
        // walk over a fresh session) must not wipe a floor the current
        // step's provider calls already established. Injection marks never
        // shrink the view and leave the floor alone.
        if view_shrank {
            self.clear_usage_floor();
        }
    }

    // -- Suppress (R4) ---------------------------------------------------

    /// Mark an event as suppressed. Suppressed events are excluded from
    /// prompt construction but remain in the store unchanged.
    ///
    /// The mark is made durable first: a
    /// [`SessionEvent::ContextMark`] twin is appended to `store` before
    /// the live set mutates, so the live prompt view and a resumed one can
    /// never diverge — on append failure *neither* holds the mark and the
    /// error propagates. Returns the ID of the appended mark event.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EventAppendFailed`] if the durable mark
    /// cannot be appended; the event is then **not** suppressed live
    /// either.
    pub fn suppress(
        &mut self,
        store: &EventStore,
        event_id: EventId,
    ) -> Result<EventId, SessionError> {
        let mark_id = store.append(SessionEvent::ContextMark {
            base: EventBase::new(None),
            mark: ContextMarkKind::Suppress,
            target_event_id: event_id.clone(),
        })?;
        self.suppressed.insert(event_id);
        self.clear_usage_floor();
        Ok(mark_id)
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
        self.clear_usage_floor();
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
        self.clear_usage_floor();
        Ok(compaction_id)
    }

    /// Check whether an event has been superseded by a compaction.
    #[must_use]
    pub fn is_superseded(&self, id: &EventId) -> bool {
        self.superseded.contains(id)
    }

    /// Compute the auto-compaction cut that retains the most recent
    /// `keep_turns` assistant turns, without committing it.
    ///
    /// A *turn* is an [`SessionEvent::AssistantMessage`]. The function
    /// counts back from the end of the store, finds the cut position N+1
    /// turns back (extended past any still-pending tool results of that
    /// turn), and reports every event before the cut as replaced.
    ///
    /// Returns `None` when there are not yet `keep_turns + 1` assistant
    /// turns in the store, or when the cut would replace nothing.
    #[must_use]
    pub fn plan_auto_compaction(
        &self,
        store: &EventStore,
        keep_turns: usize,
    ) -> Option<CompactionPlan> {
        let events = store.events();
        let assistant_positions: Vec<usize> = events
            .iter()
            .enumerate()
            .filter_map(|(idx, e)| {
                matches!(e, SessionEvent::AssistantMessage { .. }).then_some(idx)
            })
            .collect();
        if assistant_positions.len() <= keep_turns {
            return None;
        }
        let prior_assistant_idx = assistant_positions[assistant_positions.len() - keep_turns - 1];
        let cut_exclusive = compact_boundary_after_tool_results(&events, prior_assistant_idx);
        let replaced_ids: Vec<EventId> = events[..cut_exclusive]
            .iter()
            .map(|e| e.base().id.clone())
            .collect();
        if replaced_ids.is_empty() {
            return None;
        }
        let newly_superseded: Vec<EventId> = replaced_ids
            .iter()
            .filter(|id| !self.superseded.contains(*id) && !self.suppressed.contains(*id))
            .cloned()
            .collect();
        Some(CompactionPlan {
            cut_exclusive,
            replaced_ids,
            newly_superseded,
        })
    }

    /// Commit a [`CompactionPlan`]: append a [`SessionEvent::Compaction`]
    /// carrying `summary` and mark every planned event as superseded.
    ///
    /// The plan is re-validated against the store before anything is
    /// written: because the store is append-only, the planned prefix must
    /// still be identical, and a mismatch means the plan was computed
    /// against a different store.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EventAppendFailed`] if the plan no longer
    /// matches the store or the compaction event cannot be appended.
    pub fn commit_compaction_plan(
        &mut self,
        store: &EventStore,
        plan: CompactionPlan,
        summary: String,
    ) -> Result<AutoCompactionOutcome, SessionError> {
        let events = store.events();
        let prefix_matches = events.len() >= plan.cut_exclusive
            && events[..plan.cut_exclusive]
                .iter()
                .zip(&plan.replaced_ids)
                .all(|(event, id)| event.base().id == *id)
            && plan.replaced_ids.len() == plan.cut_exclusive;
        if !prefix_matches {
            return Err(SessionError::EventAppendFailed {
                reason: "compaction plan does not match the event store it is committed against"
                    .to_string(),
            });
        }
        let compaction = SessionEvent::Compaction {
            base: EventBase::new(None),
            summary,
            replaced_event_ids: plan.replaced_ids.clone(),
        };
        let compaction_id = store.append(compaction)?;
        for id in plan.replaced_ids {
            self.superseded.insert(id);
        }
        self.clear_usage_floor();
        Ok(AutoCompactionOutcome {
            compaction_id,
            newly_superseded: plan.newly_superseded,
        })
    }

    /// Auto-compact older events while retaining the most recent
    /// `keep_turns` assistant turns, recording the mechanical digest as
    /// the compaction summary.
    ///
    /// This is [`Self::plan_auto_compaction`] followed by
    /// [`Self::commit_compaction_plan`] with the structured JSON digest
    /// from [`build_compaction_digest`] as the summary. It is the path for
    /// callers without a provider handle (manual `/compact` commands); the
    /// agent loop instead commits an LLM-written summary through the
    /// plan/commit pair directly.
    ///
    /// Returns `Ok(None)` when there is nothing to compact.
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
    ) -> Result<Option<AutoCompactionOutcome>, SessionError> {
        let Some(plan) = self.plan_auto_compaction(store, keep_turns) else {
            return Ok(None);
        };
        let replaced = &store.events()[..plan.cut_exclusive];
        let summary_json = build_compaction_digest(replaced, token_estimate_freed);
        let summary =
            serde_json::to_string(&summary_json).map_err(|e| SessionError::EventAppendFailed {
                reason: format!("failed to serialise auto-compaction summary: {e}"),
            })?;
        self.commit_compaction_plan(store, plan, summary).map(Some)
    }

    // -- Inject (R6) -----------------------------------------------------

    /// Inject an external event into the session.
    ///
    /// The event is appended to the store normally, followed by a durable
    /// [`SessionEvent::ContextMark`] twin recording the injection, and its
    /// ID is tracked live so prompt construction can tag it appropriately.
    /// The live set only mutates after both appends succeed, so the live
    /// prompt view and a resumed one can never diverge: on a mark-append
    /// failure the injected event exists in the store as a plain event —
    /// live and resumed views agree on that too — and the error
    /// propagates.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EventAppendFailed`] if the event or its
    /// durable injection mark cannot be appended.
    pub fn inject(
        &mut self,
        store: &EventStore,
        event: SessionEvent,
    ) -> Result<EventId, SessionError> {
        let id = store.append(event)?;
        store.append(SessionEvent::ContextMark {
            base: EventBase::new(None),
            mark: ContextMarkKind::Inject,
            target_event_id: id.clone(),
        })?;
        self.injected.insert(id.clone());
        Ok(id)
    }

    /// Check whether an event was injected.
    #[must_use]
    pub fn is_injected(&self, id: &EventId) -> bool {
        self.injected.contains(id)
    }
}

/// Build the structured JSON digest recorded as an auto-compaction summary.
///
/// Describes the replaced span concretely so the model retains a factual
/// account of what was elided: per-role event counts, the tool calls made
/// (`name → count`, deterministically ordered), prior compactions folded
/// into this one, the freed-token estimate, the first/last event
/// timestamps (RFC 3339), and the replaced event-ID range.
///
/// The digest is mechanically derived, not semantic; the emitted object
/// carries `"summary_kind": "mechanical_digest"` so consumers can tell it
/// apart from an LLM-written summary. The agent loop's fallback path
/// overrides this marker with `"mechanical_digest_fallback"` plus the
/// summarization error when an LLM summary was attempted and failed.
#[must_use]
pub fn build_compaction_digest(
    replaced: &[SessionEvent],
    token_estimate_freed: usize,
) -> serde_json::Value {
    let mut user_messages = 0_usize;
    let mut assistant_messages = 0_usize;
    let mut tool_results = 0_usize;
    let mut prior_compactions = 0_usize;
    let mut other_events = 0_usize;
    let mut tool_calls: BTreeMap<String, usize> = BTreeMap::new();
    for event in replaced {
        match event {
            SessionEvent::UserMessage { .. } => user_messages += 1,
            SessionEvent::AssistantMessage {
                tool_calls: calls, ..
            } => {
                assistant_messages += 1;
                for call in calls {
                    *tool_calls.entry(call.name.clone()).or_insert(0) += 1;
                }
            }
            SessionEvent::ToolResult { .. } => tool_results += 1,
            SessionEvent::Compaction { .. } => prior_compactions += 1,
            SessionEvent::Custom { .. }
            | SessionEvent::ModelChange { .. }
            | SessionEvent::ChildBranch { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::RuleInjection { .. }
            | SessionEvent::ContextMark { .. }
            | SessionEvent::SpokenResponse { .. } => other_events += 1,
        }
    }
    let first = replaced.first().map(SessionEvent::base);
    let last = replaced.last().map(SessionEvent::base);
    serde_json::json!({
        "summary_kind": "mechanical_digest",
        "event_count_suppressed": replaced.len(),
        "token_estimate_freed": token_estimate_freed,
        "user_messages": user_messages,
        "assistant_messages": assistant_messages,
        "tool_results": tool_results,
        "prior_compactions": prior_compactions,
        "other_events": other_events,
        "tool_calls": tool_calls,
        "first_timestamp": first.map(|b| b.timestamp.to_rfc3339()),
        "last_timestamp": last.map(|b| b.timestamp.to_rfc3339()),
        "turn_range": {
            "start": first.map(|b| b.id.to_string()),
            "end": last.map(|b| b.id.to_string()),
        }
    })
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
            | SessionEvent::ChildBranch { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::RuleInjection { .. }
            | SessionEvent::ContextMark { .. }
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
        let mark_id = edits.suppress(&store, id.clone()).expect("suppress");
        assert!(edits.is_suppressed(&id));

        let original = store.get(&id).expect("still in store");
        match original {
            SessionEvent::UserMessage { content, .. } => assert_eq!(content, "hello"),
            _ => panic!("wrong variant"),
        }

        // The durable twin landed in the store, targeting the event.
        let mark = store.get(&mark_id).expect("mark persisted");
        match mark {
            SessionEvent::ContextMark {
                mark,
                target_event_id,
                ..
            } => {
                assert_eq!(mark, ContextMarkKind::Suppress);
                assert_eq!(target_event_id, id);
            }
            _ => panic!("expected ContextMark"),
        }
    }

    #[test]
    fn apply_persisted_marks_rebuilds_suppress_and_inject() {
        let store = EventStore::new();
        let kept = store.append(user_msg("kept")).expect("append");
        let mut edits = ContextEdits::new();
        let suppressed = store.append(user_msg("suppressed")).expect("append");
        edits
            .suppress(&store, suppressed.clone())
            .expect("suppress");
        let injected = edits.inject(&store, user_msg("injected")).expect("inject");

        // Process-restart shape: a fresh tracker over the same store.
        let mut rebuilt = ContextEdits::new();
        rebuilt.apply_persisted_marks(&store);

        assert!(rebuilt.is_suppressed(&suppressed));
        assert!(!rebuilt.is_suppressed(&kept));
        assert!(rebuilt.is_injected(&injected));
        assert!(!rebuilt.is_injected(&kept));

        // Idempotent: a second application changes nothing.
        rebuilt.apply_persisted_marks(&store);
        assert!(rebuilt.is_suppressed(&suppressed));
        assert!(rebuilt.is_injected(&injected));
    }

    #[test]
    fn apply_persisted_marks_touches_floor_only_when_the_view_shrinks() {
        // No supersession or suppression marks in the store: the walk must
        // leave an established floor alone (the runner's one-time
        // first-step walk runs on stores like this).
        let store = EventStore::new();
        store.append(user_msg("plain")).expect("append");
        let mut edits = ContextEdits::new();
        edits.inject(&store, user_msg("injected")).expect("inject");
        let mut walked = ContextEdits::new();
        walked.set_usage_floor(9_999);
        walked.apply_persisted_marks(&store);
        assert_eq!(
            walked.usage_floor(),
            Some(9_999),
            "inject-only restoration grows the view and must keep the floor",
        );

        // A restored suppress mark shrinks the view: the floor must go.
        let target = store.append(user_msg("noisy")).expect("append");
        edits.suppress(&store, target).expect("suppress");
        walked.apply_persisted_marks(&store);
        assert_eq!(
            walked.usage_floor(),
            None,
            "suppression restoration shrinks the view and must clear the floor",
        );
    }

    #[test]
    fn mark_suppressed_clears_floor_and_mark_injected_does_not() {
        let mut edits = ContextEdits::new();
        edits.set_usage_floor(100_000);
        edits.mark_injected([EventId::new()]);
        assert_eq!(
            edits.usage_floor(),
            Some(100_000),
            "injection restore grows the view and must keep the floor",
        );
        edits.mark_suppressed([EventId::new()]);
        assert_eq!(
            edits.usage_floor(),
            None,
            "suppression restore shrinks the view and must clear the floor",
        );
    }

    // -- Usage floor -------------------------------------------------------

    /// The floor is "last provider report wins": a later, smaller report
    /// (e.g. after provider-side/server compaction) overwrites a larger one.
    #[test]
    fn usage_floor_overwrites_with_the_latest_report() {
        let mut edits = ContextEdits::new();
        assert_eq!(edits.usage_floor(), None, "floor starts unset");
        edits.set_usage_floor(100_000);
        assert_eq!(edits.usage_floor(), Some(100_000));
        edits.set_usage_floor(60_000);
        assert_eq!(
            edits.usage_floor(),
            Some(60_000),
            "the last report wins, even when smaller",
        );
    }

    /// Every prompt-view-shrinking mutation clears the floor — this is the
    /// choke point that prevents the compaction death spiral (a stale floor
    /// re-firing the trigger against an already-compacted conversation).
    #[test]
    fn every_shrinking_mutation_clears_the_usage_floor() {
        // suppress
        let store = EventStore::new();
        let id = store.append(user_msg("m")).expect("append");
        let mut edits = ContextEdits::new();
        edits.set_usage_floor(100_000);
        edits.suppress(&store, id).expect("suppress");
        assert_eq!(edits.usage_floor(), None, "suppress must clear the floor");

        // summarize
        let store = EventStore::new();
        let a = store.append(user_msg("a")).expect("append");
        let mut edits = ContextEdits::new();
        edits.set_usage_floor(100_000);
        edits
            .summarize(&store, vec![a], "s".to_owned())
            .expect("summarize");
        assert_eq!(edits.usage_floor(), None, "summarize must clear the floor");

        // compact
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..3 {
            ids.push(store.append(user_msg(&format!("m{i}"))).expect("append"));
        }
        let mut edits = ContextEdits::new();
        edits.set_usage_floor(100_000);
        edits
            .compact(&store, &ids[2], "c".to_owned())
            .expect("compact");
        assert_eq!(edits.usage_floor(), None, "compact must clear the floor");

        // mark_superseded (resume-restoration path; floor is normally None
        // there, but the invariant stays total)
        let store = EventStore::new();
        let id = store.append(user_msg("m")).expect("append");
        let mut edits = ContextEdits::new();
        edits.set_usage_floor(100_000);
        edits.mark_superseded([id]);
        assert_eq!(
            edits.usage_floor(),
            None,
            "mark_superseded must clear the floor",
        );
    }

    /// The manual `/compact` path (`auto_compact_keeping_recent_turns`,
    /// which commits through `commit_compaction_plan`) clears the floor —
    /// the same choke point the loop's auto-compaction commit flows through.
    #[test]
    fn manual_compact_path_clears_the_usage_floor() {
        let store = EventStore::new();
        for i in 0..4 {
            store
                .append(assistant_msg(&format!("t{i}")))
                .expect("append");
        }
        let mut edits = ContextEdits::new();
        edits.set_usage_floor(100_000);
        let outcome = edits
            .auto_compact_keeping_recent_turns(&store, 1, 500)
            .expect("compaction runs")
            .expect("compaction fires");
        assert!(!outcome.newly_superseded.is_empty());
        assert_eq!(
            edits.usage_floor(),
            None,
            "the manual compact path must clear the floor",
        );
    }

    /// Injection grows the prompt view; the floor remains a valid lower
    /// bound and must survive.
    #[test]
    fn inject_does_not_clear_the_usage_floor() {
        let store = EventStore::new();
        let mut edits = ContextEdits::new();
        edits.set_usage_floor(100_000);
        edits.inject(&store, user_msg("injected")).expect("inject");
        assert_eq!(
            edits.usage_floor(),
            Some(100_000),
            "growth must not clear the floor",
        );
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
        edits.suppress(&store, id1.clone()).expect("suppress");
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
            reasoning: Vec::new(),
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
            reasoning: Vec::new(),
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
            spool_ref: None,
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
        let outcome = result.expect("expected compaction event");

        let comp = store
            .get(&outcome.compaction_id)
            .expect("compaction stored");
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
        // Nothing was previously superseded, so the newly-superseded set is
        // the full replaced set, in order.
        assert_eq!(outcome.newly_superseded, replaced_event_ids);
        let parsed: serde_json::Value = serde_json::from_str(&summary).expect("summary parses");
        assert_eq!(parsed["event_count_suppressed"], 15);
        assert_eq!(parsed["token_estimate_freed"], 4096);
        assert!(parsed["turn_range"]["start"].is_string());
        assert!(parsed["turn_range"]["end"].is_string());
    }

    // -- Review item 6a: the auto-compaction summary is a content digest --

    #[test]
    fn auto_compact_summary_is_structured_digest() {
        let store = EventStore::new();
        store.append(user_msg("old question")).expect("append");
        store
            .append(assistant_tool_call("call_old"))
            .expect("append");
        store.append(tool_result("call_old")).expect("append");
        store.append(assistant_msg("old answer")).expect("append");
        store.append(user_msg("newer question")).expect("append");
        store.append(assistant_msg("newer answer")).expect("append");

        let mut edits = ContextEdits::new();
        let outcome = edits
            .auto_compact_keeping_recent_turns(&store, 1, 512)
            .expect("compaction runs")
            .expect("compaction fires");

        let comp = store
            .get(&outcome.compaction_id)
            .expect("compaction stored");
        let SessionEvent::Compaction { summary, .. } = comp else {
            panic!("expected Compaction variant");
        };
        let parsed: serde_json::Value = serde_json::from_str(&summary).expect("summary parses");
        // keep_turns=1 cuts after "old answer" (the second-to-last
        // assistant turn): the replaced span is [old question,
        // assistant_tool_call, tool_result, old answer]. "newer question"
        // and "newer answer" stay live.
        assert_eq!(parsed["event_count_suppressed"], 4, "digest: {parsed}");
        assert_eq!(parsed["user_messages"], 1, "digest: {parsed}");
        assert_eq!(parsed["assistant_messages"], 2, "digest: {parsed}");
        assert_eq!(parsed["tool_results"], 1, "digest: {parsed}");
        assert_eq!(parsed["tool_calls"]["read"], 1, "digest: {parsed}");
        assert_eq!(parsed["prior_compactions"], 0, "digest: {parsed}");
        assert_eq!(parsed["token_estimate_freed"], 512);
        let first = parsed["first_timestamp"].as_str().expect("first ts");
        let last = parsed["last_timestamp"].as_str().expect("last ts");
        assert!(
            chrono::DateTime::parse_from_rfc3339(first).is_ok(),
            "first timestamp must be RFC 3339: {first}"
        );
        assert!(
            chrono::DateTime::parse_from_rfc3339(last).is_ok(),
            "last timestamp must be RFC 3339: {last}"
        );
    }

    #[test]
    fn auto_compact_newly_superseded_excludes_prior_marks() {
        let store = EventStore::new();
        let oldest = store.append(user_msg("oldest")).expect("append");
        let suppressed = store.append(user_msg("suppressed")).expect("append");

        let mut edits = ContextEdits::new();
        edits
            .suppress(&store, suppressed.clone())
            .expect("suppress");
        let first = edits
            .summarize(&store, vec![oldest.clone()], "first summary".to_owned())
            .expect("summarize");

        for i in 0..4 {
            store
                .append(assistant_msg(&format!("turn {i}")))
                .expect("append");
        }

        let outcome = edits
            .auto_compact_keeping_recent_turns(&store, 1, 100)
            .expect("compaction runs")
            .expect("compaction fires");

        assert!(
            !outcome.newly_superseded.contains(&oldest),
            "already-superseded events must not be reported as newly hidden"
        );
        assert!(
            !outcome.newly_superseded.contains(&suppressed),
            "suppressed events must not be reported as newly hidden"
        );
        assert!(
            outcome.newly_superseded.contains(&first),
            "the prior compaction event itself was visible and is newly hidden"
        );
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

    // -- Track L finding 1: plan/commit two-phase compaction ---------------

    #[test]
    fn plan_exposes_cut_and_replaced_ids_without_mutating() {
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..5 {
            ids.push(
                store
                    .append(assistant_msg(&format!("t{i}")))
                    .expect("append"),
            );
        }
        let edits = ContextEdits::new();

        let plan = edits
            .plan_auto_compaction(&store, 2)
            .expect("plan computed");
        assert_eq!(plan.cut_exclusive(), 3);
        assert_eq!(plan.replaced_ids(), &ids[..3]);
        assert_eq!(plan.newly_superseded(), &ids[..3]);
        // Planning is read-only: nothing is superseded yet and no
        // compaction event was appended.
        for id in &ids {
            assert!(!edits.is_superseded(id));
        }
        assert_eq!(store.len(), 5);
    }

    #[test]
    fn commit_records_caller_summary_and_supersedes() {
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..5 {
            ids.push(
                store
                    .append(assistant_msg(&format!("t{i}")))
                    .expect("append"),
            );
        }
        let mut edits = ContextEdits::new();
        let plan = edits.plan_auto_compaction(&store, 2).expect("plan");

        let outcome = edits
            .commit_compaction_plan(&store, plan, "an LLM-written summary".to_owned())
            .expect("commit");

        let comp = store.get(&outcome.compaction_id).expect("stored");
        let SessionEvent::Compaction {
            summary,
            replaced_event_ids,
            ..
        } = comp
        else {
            panic!("expected Compaction variant");
        };
        assert_eq!(summary, "an LLM-written summary");
        assert_eq!(replaced_event_ids, ids[..3].to_vec());
        for id in &ids[..3] {
            assert!(edits.is_superseded(id));
        }
        assert!(!edits.is_superseded(&ids[3]));
        assert_eq!(outcome.newly_superseded, ids[..3].to_vec());
    }

    #[test]
    fn commit_rejects_a_plan_from_a_different_store() {
        let store_a = EventStore::new();
        let store_b = EventStore::new();
        for i in 0..5 {
            store_a
                .append(assistant_msg(&format!("a{i}")))
                .expect("append");
            store_b
                .append(assistant_msg(&format!("b{i}")))
                .expect("append");
        }
        let mut edits = ContextEdits::new();
        let plan = edits.plan_auto_compaction(&store_a, 2).expect("plan");

        let err = edits
            .commit_compaction_plan(&store_b, plan, "summary".to_owned())
            .expect_err("a plan must only commit against its own store");
        assert!(matches!(err, SessionError::EventAppendFailed { .. }));
        // Nothing was appended or superseded on the mismatching store.
        assert_eq!(store_b.len(), 5);
    }

    #[test]
    fn digest_is_marked_as_mechanical() {
        let store = EventStore::new();
        store.append(user_msg("hello")).expect("append");
        let digest = build_compaction_digest(&store.events(), 64);
        assert_eq!(digest["summary_kind"], "mechanical_digest");
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
        // The injected event plus its durable ContextMark twin.
        assert_eq!(store.len(), 2);
        match &store.events()[1] {
            SessionEvent::ContextMark {
                mark,
                target_event_id,
                ..
            } => {
                assert_eq!(*mark, ContextMarkKind::Inject);
                assert_eq!(*target_event_id, id);
            }
            other => panic!("expected the durable inject mark, got {other:?}"),
        }
    }

    #[test]
    fn non_injected_events_not_marked() {
        let store = EventStore::new();
        let edits = ContextEdits::new();
        let id = store.append(user_msg("normal")).expect("append");
        assert!(!edits.is_injected(&id));
    }
}
