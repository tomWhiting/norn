//! Prompt construction as a read-only view over the session event stream.

use crate::session::context_edit::ContextEdits;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

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

/// Construct a prompt view from an event store and context edits.
///
/// This is a pure function: it takes only shared references and never
/// mutates its inputs. Suppressed and superseded events are excluded.
/// Injected events are included and tagged with [`ContentTag::Injection`].
#[must_use]
pub fn construct_prompt(store: &EventStore, edits: &ContextEdits) -> PromptView {
    store.with_events(|events| {
        let mut included = Vec::new();
        let mut tags = Vec::new();
        for_each_visible_event(events, edits, |event, tag| {
            tags.push(tag);
            included.push(event.clone());
        });
        PromptView {
            events: included,
            tags,
        }
    })
}

/// Visit each event that the prompt view includes, in insertion order,
/// without cloning event bodies.
///
/// This is the single source of truth for prompt visibility: suppressed
/// and superseded events are skipped, injected events are tagged
/// [`ContentTag::Injection`], and everything else is tagged via
/// [`tag_for_event`]. [`construct_prompt`] materializes owned events on top
/// of this; callers that only need tags or a filtered subset (the rules
/// engine's presence rebuild and system-context re-materialization) walk it
/// directly and pay no per-event body clone.
pub fn for_each_visible_event(
    events: &[SessionEvent],
    edits: &ContextEdits,
    mut visit: impl FnMut(&SessionEvent, ContentTag),
) {
    for event in events {
        let id = &event.base().id;

        if edits.is_suppressed(id) || edits.is_superseded(id) {
            continue;
        }

        let tag = if edits.is_injected(id) {
            ContentTag::Injection
        } else {
            tag_for_event(event)
        };

        visit(event, tag);
    }
}

fn tag_for_event(event: &SessionEvent) -> ContentTag {
    match event {
        SessionEvent::UserMessage { .. }
        | SessionEvent::AssistantMessage { .. }
        | SessionEvent::SpokenResponse { .. }
        | SessionEvent::ModelChange { .. }
        | SessionEvent::Fork { .. }
        | SessionEvent::ForkComplete { .. }
        | SessionEvent::Label { .. } => ContentTag::Message,
        SessionEvent::ToolResult { .. } => ContentTag::ToolResult,
        SessionEvent::Compaction { .. } => ContentTag::Compaction,
        SessionEvent::Custom { event_type, .. } => ContentTag::Custom(event_type.clone()),
        SessionEvent::RuleInjection { rule_id, .. } => ContentTag::Rule(rule_id.clone()),
    }
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
    use crate::session::events::{EventBase, EventUsage};

    fn user_msg(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    fn assistant_msg(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }
    }

    #[test]
    fn empty_store_produces_empty_view() {
        let store = EventStore::new();
        let edits = ContextEdits::new();
        let view = construct_prompt(&store, &edits);
        assert!(view.events.is_empty());
        assert!(view.tags.is_empty());
    }

    #[test]
    fn suppressed_events_excluded() {
        let store = EventStore::new();
        let id1 = store.append(user_msg("keep")).expect("append");
        let id2 = store.append(user_msg("suppress")).expect("append");
        let _id3 = store.append(user_msg("also keep")).expect("append");

        let mut edits = ContextEdits::new();
        edits.suppress(id2);

        let view = construct_prompt(&store, &edits);
        assert_eq!(view.events.len(), 2);
        assert_eq!(view.events[0].base().id, id1);
    }

    #[test]
    fn suppressed_assistant_reasoning_absent_from_rebuilt_prompt() {
        // Compaction/suppression of an AssistantMessage must take its
        // reasoning with it: conversion reads events post-suppression, so a
        // suppressed turn contributes no reasoning to the rebuilt prompt
        // view. Without this, resume would re-inject encrypted reasoning for
        // a turn the model no longer sees.
        use crate::provider::reasoning::{ReasoningItem, ReasoningSummaryPart};
        use crate::session::conversion::prompt_events_to_messages;

        let store = EventStore::new();
        let keep = store.append(user_msg("keep")).expect("append");
        let suppressed = store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(Some(keep)),
                content: "reasoned answer".to_owned(),
                thinking: String::new(),
                reasoning: vec![ReasoningItem {
                    id: "rs_sup".to_owned(),
                    summary: vec![ReasoningSummaryPart::SummaryText {
                        text: "to be suppressed".to_owned(),
                    }],
                    content: None,
                    encrypted_content: Some("suppressed-blob".to_owned()),
                }],
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_owned(),
                response_id: None,
            })
            .expect("append");

        let mut edits = ContextEdits::new();
        edits.suppress(suppressed);

        let view = construct_prompt(&store, &edits);
        let msgs = prompt_events_to_messages(&view.events);
        assert!(
            msgs.iter().all(|m| m.reasoning.is_empty()),
            "suppressed turn's reasoning must not survive into the prompt view",
        );
    }

    #[test]
    fn superseded_events_excluded_compaction_included() {
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..10 {
            ids.push(store.append(user_msg(&format!("msg {i}"))).expect("append"));
        }

        let mut edits = ContextEdits::new();
        let comp_ids = ids[0..3].to_vec();
        let comp_id = edits
            .summarize(&store, comp_ids, "summary".to_owned())
            .expect("summarize");

        let view = construct_prompt(&store, &edits);

        let view_ids: Vec<_> = view.events.iter().map(|e| e.base().id.clone()).collect();
        for id in &ids[0..3] {
            assert!(
                !view_ids.contains(id),
                "superseded event should be excluded"
            );
        }
        assert!(view_ids.contains(&comp_id), "compaction should be included");
        for id in &ids[3..] {
            assert!(view_ids.contains(id), "remaining events should be included");
        }

        assert!(
            view.tags.contains(&ContentTag::Compaction),
            "compaction tag should be present"
        );
    }

    #[test]
    fn injected_events_tagged() {
        let store = EventStore::new();
        store.append(user_msg("normal")).expect("append");

        let mut edits = ContextEdits::new();
        edits.inject(&store, user_msg("injected")).expect("inject");

        let view = construct_prompt(&store, &edits);
        assert_eq!(view.events.len(), 2);
        assert_eq!(view.tags[0], ContentTag::Message);
        assert_eq!(view.tags[1], ContentTag::Injection);
    }

    #[test]
    fn combined_suppress_and_compact() {
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..10 {
            ids.push(store.append(user_msg(&format!("msg {i}"))).expect("append"));
        }

        let mut edits = ContextEdits::new();
        edits.suppress(ids[4].clone());
        edits.suppress(ids[8].clone());
        edits
            .summarize(&store, ids[0..3].to_vec(), "compact summary".to_owned())
            .expect("summarize");

        let view = construct_prompt(&store, &edits);

        let expected_excluded: Vec<_> = ids[0..3]
            .iter()
            .chain(std::iter::once(&ids[4]))
            .chain(std::iter::once(&ids[8]))
            .collect();
        let view_ids: Vec<_> = view.events.iter().map(|e| e.base().id.clone()).collect();
        for id in &expected_excluded {
            assert!(
                !view_ids.contains(id),
                "event {id} should be excluded from view"
            );
        }

        assert_eq!(
            view.events.len(),
            // 10 original - 3 superseded - 2 suppressed + 1 compaction = 6
            6,
            "should have 6 events in view"
        );
    }

    #[test]
    fn construct_prompt_does_not_mutate() {
        let store = EventStore::new();
        for i in 0..3 {
            store.append(user_msg(&format!("msg {i}"))).expect("append");
        }
        let edits = ContextEdits::new();

        let len_before = store.len();
        let _view = construct_prompt(&store, &edits);
        assert_eq!(store.len(), len_before, "store must not be mutated");
    }

    #[test]
    fn tags_match_events() {
        let store = EventStore::new();
        store.append(user_msg("user")).expect("append");
        store.append(assistant_msg("assistant")).expect("append");
        store
            .append(SessionEvent::ToolResult {
                base: EventBase::new(None),
                tool_call_id: "tc1".to_owned(),
                tool_name: "Read".to_owned(),
                output: serde_json::json!({}),
                spool_ref: None,
                duration_ms: 10,
            })
            .expect("append");

        let edits = ContextEdits::new();
        let view = construct_prompt(&store, &edits);

        assert_eq!(view.tags.len(), 3);
        assert_eq!(view.tags[0], ContentTag::Message);
        assert_eq!(view.tags[1], ContentTag::Message);
        assert_eq!(view.tags[2], ContentTag::ToolResult);
    }
}
