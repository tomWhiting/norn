//! Summary input follows prompt visibility; durable replacement IDs remain an audit record.

use std::collections::HashSet;

use super::context::for_each_visible_event;
use crate::session::context_edit::{CompactionPlan, ContextEdits};
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

/// Select only the planned, currently visible events, preserving tool-call atomicity.
/// The raw history and retained recent bodies are never cloned into this input.
pub(super) fn summary_prompt_events(
    store: &EventStore,
    edits: &ContextEdits,
    plan: &CompactionPlan,
) -> Vec<SessionEvent> {
    let replaced: HashSet<_> = plan.newly_superseded().iter().collect();
    store.with_events(|events| {
        let mut included = Vec::new();
        for_each_visible_event(events, edits, |event, _| {
            if replaced.contains(&event.base().id) {
                included.push(event.clone());
            }
        });
        crate::session::atomic_local_tool_projection(events, included)
    })
}

#[cfg(test)]
#[path = "compaction_prompt_tests.rs"]
mod tests;
