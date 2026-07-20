//! Prompt construction as a read-only view over the session event stream.

use crate::session::context_edit::ContextEdits;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;
use crate::session::{PROVIDER_STATE_PROVENANCE_EVENT_TYPE, response_publication_group_len};

/// Tag describing a piece of content included in the prompt.
///
/// Returned by [`construct_prompt`] so consumers (e.g. the rules engine)
/// can track what is currently in context without coupling to prompt
/// construction internals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentTag {
    /// A user or assistant message.
    Message,
    /// A tool result.
    ToolResult,
    /// A compaction summary.
    Compaction,
    /// An injected external event.
    Injection,
    /// A rule identified by its rule ID string.
    Rule(String),
    /// An application-defined custom tag.
    Custom(String),
}

/// The result of prompt construction: an ordered list of events to include
/// and the content tags describing what was included.
#[derive(Debug)]
pub struct PromptView {
    /// Events to include in the prompt, in insertion order.
    pub events: Vec<SessionEvent>,
    /// Tags describing each included piece of content.
    pub tags: Vec<ContentTag>,
}

/// Run a prompt-view operation against the installed context edits, or a
/// transient projection rebuilt from durable marks when no tracker exists.
///
/// The installed tracker is authoritative and is borrowed directly: callers
/// that keep one on [`LoopContext`](crate::agent_loop::LoopContext) retain the
/// runner's one-time-load path without another store walk. Tracker-free
/// embedders still receive the persisted compaction, suppression, and
/// injection view without mutating either the store or their loop context.
pub(crate) fn with_prompt_context_edits<T>(
    store: &EventStore,
    installed: Option<&ContextEdits>,
    use_edits: impl FnOnce(&ContextEdits) -> T,
) -> T {
    if let Some(edits) = installed {
        return use_edits(edits);
    }

    let mut projected = ContextEdits::new();
    projected.apply_persisted_marks(store);
    use_edits(&projected)
}

/// Construct a prompt view from an event store and context edits.
///
/// This is a pure function: it takes only shared references and never
/// mutates its inputs. Suppressed and superseded events are excluded, then
/// local tool calls and outputs split by those event-level marks are removed
/// atomically. Injected events are included and tagged with
/// [`ContentTag::Injection`].
#[must_use]
pub fn construct_prompt(store: &EventStore, edits: &ContextEdits) -> PromptView {
    store.with_events(|events| {
        let mut included = Vec::new();
        for_each_visible_event(events, edits, |event, _tag| {
            included.push(event.clone());
        });
        let included = crate::session::atomic_local_tool_projection(events, included);
        let tags = included
            .iter()
            .filter_map(|event| {
                let base_tag = tag_for_event(event)?;
                Some(if edits.is_injected(&event.base().id) {
                    ContentTag::Injection
                } else {
                    base_tag
                })
            })
            .collect();
        PromptView {
            events: included,
            tags,
        }
    })
}

/// Visit each event that the prompt view includes, in insertion order,
/// without cloning event bodies.
///
/// This is the single source of truth for event-level prompt visibility:
/// suppressed and superseded events are skipped, injected events are tagged
/// [`ContentTag::Injection`], and everything else is tagged via
/// [`tag_for_event`]. [`construct_prompt`] materializes owned events on top of
/// this and then enforces call/output atomicity. Callers that only need tags
/// or a filtered subset (the rules engine's presence rebuild and
/// system-context re-materialization) walk it directly and pay no per-event
/// body clone.
pub fn for_each_visible_event(
    events: &[SessionEvent],
    edits: &ContextEdits,
    mut visit: impl FnMut(&SessionEvent, ContentTag),
) {
    for (event_index, event) in events.iter().enumerate() {
        let id = &event.base().id;

        if edits.is_suppressed(id) || edits.is_superseded(id) {
            continue;
        }

        if is_framed_provider_provenance(events, event_index, event) {
            continue;
        }

        // Events with no tag are bookkeeping (durable context-mark
        // twins); they are structurally invisible to the prompt view —
        // even the injected-tag override never applies to them.
        let Some(base_tag) = tag_for_event(event) else {
            continue;
        };
        let tag = if edits.is_injected(id) {
            ContentTag::Injection
        } else {
            base_tag
        };

        visit(event, tag);
    }
}

/// The content tag an event carries in the prompt view, or `None` for
/// events the view structurally excludes:
/// [`SessionEvent::ContextMark`] is the durable twin of a live edit mark
/// — a record *about* the view, never content *in* it.
fn tag_for_event(event: &SessionEvent) -> Option<ContentTag> {
    match event {
        SessionEvent::UserMessage { .. }
        | SessionEvent::AssistantMessage { .. }
        | SessionEvent::SpokenResponse { .. }
        | SessionEvent::ModelChange { .. }
        | SessionEvent::ChildBranch { .. }
        | SessionEvent::ForkComplete { .. }
        | SessionEvent::Label { .. } => Some(ContentTag::Message),
        SessionEvent::ToolResult { .. } => Some(ContentTag::ToolResult),
        SessionEvent::Compaction { .. } => Some(ContentTag::Compaction),
        SessionEvent::Custom { event_type, .. } => Some(ContentTag::Custom(event_type.clone())),
        SessionEvent::RuleInjection { rule_id, .. } => Some(ContentTag::Rule(rule_id.clone())),
        SessionEvent::ContextMark { .. } | SessionEvent::ProviderEpochBoundary { .. } => None,
    }
}

fn is_framed_provider_provenance(
    events: &[SessionEvent],
    event_index: usize,
    event: &SessionEvent,
) -> bool {
    matches!(
        event,
        SessionEvent::Custom { event_type, .. }
            if event_type == PROVIDER_STATE_PROVENANCE_EVENT_TYPE
    ) && event_index.checked_sub(1).is_some_and(|boundary_index| {
        response_publication_group_len(events, boundary_index).is_ok_and(|length| length.is_some())
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tool_pair_projection_tests;
