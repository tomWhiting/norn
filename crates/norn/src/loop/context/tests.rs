use super::*;
use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::session::events::{EventBase, EventUsage, ToolCallEvent};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn user_msg(content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: content.to_owned(),
    }
}

fn assistant_msg(content: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
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

fn assistant_with_call(content: &str, call_id: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: content.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: call_id.to_owned(),
            name: "Read".to_owned(),
            arguments: serde_json::json!({"path": "README.md"}),
            kind: ToolCallKind::Function,
            caller: ToolCallCaller::Absent,
        }],
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
fn suppressed_events_excluded() -> TestResult {
    let store = EventStore::new();
    let id1 = store.append(user_msg("keep"))?;
    let id2 = store.append(user_msg("suppress"))?;
    store.append(user_msg("also keep"))?;

    let mut edits = ContextEdits::new();
    edits.suppress(&store, id2)?;

    let view = construct_prompt(&store, &edits);
    assert_eq!(view.events.len(), 2);
    assert_eq!(view.events[0].base().id, id1);
    Ok(())
}

#[test]
fn context_mark_events_are_invisible_to_the_prompt_view() -> TestResult {
    let store = EventStore::new();
    let keep = store.append(user_msg("keep"))?;
    let hide = store.append(user_msg("hide"))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&store, hide)?;
    edits.inject(&store, user_msg("note"))?;

    // Store now holds: keep, hide, ContextMark(suppress), note,
    // ContextMark(inject). The view holds only real content.
    assert_eq!(store.len(), 5);
    let view = construct_prompt(&store, &edits);
    assert_eq!(view.events.len(), 2, "marks must never surface");
    assert_eq!(view.events[0].base().id, keep);
    assert_eq!(view.tags, vec![ContentTag::Message, ContentTag::Injection]);
    Ok(())
}

#[test]
fn incomplete_provider_frame_does_not_hide_application_custom_data() -> TestResult {
    let store = EventStore::new();
    let fixture = crate::session::response_publication_fixture(None, true)?;
    let assistant = SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: fixture.assistant_base,
        content: "unfinished response".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_incomplete".to_owned()),
    };
    let mut publication = crate::session::committed_response_publication(
        fixture.boundary,
        fixture.provenance,
        assistant,
    )?
    .into_iter();
    let boundary = publication
        .next()
        .ok_or_else(|| std::io::Error::other("committed fixture omitted its boundary"))?;
    let boundary_id = boundary.base().id.clone();
    store.append_unvalidated_for_test(boundary)?;
    let custom = SessionEvent::Custom {
        base: EventBase::new(Some(boundary_id)),
        event_type: crate::session::PROVIDER_STATE_PROVENANCE_EVENT_TYPE.to_owned(),
        data: serde_json::json!({"application": "not a complete provider frame"}),
    };
    let custom_id = custom.base().id.clone();
    store.append_unvalidated_for_test(custom)?;

    let view = construct_prompt(&store, &ContextEdits::new());
    assert_eq!(view.events.len(), 1);
    assert_eq!(view.events[0].base().id, custom_id);
    assert_eq!(
        view.tags,
        vec![ContentTag::Custom(
            crate::session::PROVIDER_STATE_PROVENANCE_EVENT_TYPE.to_owned()
        )]
    );
    Ok(())
}

/// Gap 8 closure: suppress and injection marks applied live must
/// survive a process restart. The live history is round-tripped
/// through the JSONL wire format and the strict reader (the same
/// path `SessionManager::resume` uses), a fresh store and tracker are
/// rebuilt from the [`ReplayArtifacts`], and the *effective* prompt
/// view — event ids, tags, and rendered provider messages — must be
/// identical to the pre-restart one.
#[test]
fn restart_shaped_resume_rebuilds_identical_prompt_view() -> Result<(), Box<dyn std::error::Error>>
{
    use crate::session::conversion::prompt_events_to_messages;
    use crate::session::persistence::io::read_session_events_from;
    use crate::session::{SESSION_FORMAT_VERSION, SessionFileHeader};

    // Live session: content, a suppression, an injection, and a
    // compaction, all through the real edit surfaces.
    let store = EventStore::new();
    store.append(user_msg("q0"))?;
    store.append(assistant_msg("a0"))?;
    let noisy = store.append(user_msg("noisy aside"))?;
    store.append(user_msg("q1"))?;
    store.append(assistant_msg("a1"))?;

    let mut edits = ContextEdits::new();
    edits.suppress(&store, noisy)?;
    edits.inject(&store, user_msg("operator note"))?;
    let old_ids: Vec<_> = store
        .events()
        .iter()
        .take(2)
        .map(|e| e.base().id.clone())
        .collect();
    edits.summarize(&store, old_ids, "summary of turn 0".to_owned())?;

    let live_view = construct_prompt(&store, &edits);

    // Process restart: serialize every event as a JSONL line and read
    // it back through the strict reader.
    let mut file = serde_json::to_vec(&SessionFileHeader {
        version: SESSION_FORMAT_VERSION,
    })?;
    file.push(b'\n');
    for event in store.events() {
        serde_json::to_writer(&mut file, &event)?;
        file.push(b'\n');
    }
    let artifacts = read_session_events_from(std::io::Cursor::new(file), "restart-test")?;

    let resumed_store = EventStore::new();
    for event in artifacts.events.clone() {
        resumed_store.append(event)?;
    }
    let mut resumed_edits = ContextEdits::new();
    resumed_edits.mark_superseded(artifacts.superseded_event_ids.iter().cloned());
    resumed_edits.mark_suppressed(artifacts.suppressed_event_ids.iter().cloned());
    resumed_edits.mark_injected(artifacts.injected_event_ids.iter().cloned());

    let resumed_view = construct_prompt(&resumed_store, &resumed_edits);

    let live_ids: Vec<_> = live_view
        .events
        .iter()
        .map(|e| e.base().id.clone())
        .collect();
    let resumed_ids: Vec<_> = resumed_view
        .events
        .iter()
        .map(|e| e.base().id.clone())
        .collect();
    assert_eq!(live_ids, resumed_ids, "same events, same order");
    assert_eq!(live_view.tags, resumed_view.tags, "same tags");

    // The effective conversation the model would see is identical.
    let live_msgs = prompt_events_to_messages(&live_view.events);
    let resumed_msgs = prompt_events_to_messages(&resumed_view.events);
    assert_eq!(live_msgs.len(), resumed_msgs.len());
    for (live, resumed) in live_msgs.iter().zip(&resumed_msgs) {
        assert_eq!(live.role, resumed.role);
        assert_eq!(live.content, resumed.content);
    }
    // And the suppressed event genuinely stays out after restart.
    assert!(
        live_msgs
            .iter()
            .all(|m| m.content.as_deref() != Some("noisy aside")),
        "the suppressed event must not resurface",
    );
    Ok(())
}

/// The other resume shape: a fresh tracker over the same live store
/// (no file round-trip), restored through
/// [`ContextEdits::apply_persisted_marks`] — the walk the step runner
/// performs when `context_marks_loaded` is false.
#[test]
fn apply_persisted_marks_rebuilds_identical_prompt_view() -> TestResult {
    let store = EventStore::new();
    store.append(user_msg("kept"))?;
    let hidden = store.append(user_msg("hidden"))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&store, hidden)?;
    edits.inject(&store, user_msg("injected"))?;
    let live_view = construct_prompt(&store, &edits);

    let mut rebuilt = ContextEdits::new();
    rebuilt.apply_persisted_marks(&store);
    let rebuilt_view = construct_prompt(&store, &rebuilt);

    let live_ids: Vec<_> = live_view
        .events
        .iter()
        .map(|e| e.base().id.clone())
        .collect();
    let rebuilt_ids: Vec<_> = rebuilt_view
        .events
        .iter()
        .map(|e| e.base().id.clone())
        .collect();
    assert_eq!(live_ids, rebuilt_ids);
    assert_eq!(live_view.tags, rebuilt_view.tags);
    Ok(())
}

#[test]
fn tracker_free_projection_restores_durable_view_without_rewriting_audit_rows() -> TestResult {
    let store = EventStore::new();
    let compacted = store.append(user_msg("compacted source"))?;
    let suppressed = store.append(user_msg("suppressed source"))?;
    let kept = store.append(user_msg("kept source"))?;
    let mut live = ContextEdits::new();
    let summary = live.summarize(
        &store,
        vec![compacted.clone()],
        "durable summary".to_owned(),
    )?;
    live.suppress(&store, suppressed.clone())?;
    let injected = live.inject(&store, user_msg("durable injection"))?;

    let view = with_prompt_context_edits(&store, None, |edits| construct_prompt(&store, edits));
    let ids: Vec<_> = view
        .events
        .iter()
        .map(|event| event.base().id.clone())
        .collect();

    assert_eq!(ids, vec![kept, summary, injected]);
    assert_eq!(
        view.tags,
        vec![
            ContentTag::Message,
            ContentTag::Compaction,
            ContentTag::Injection,
        ],
    );
    let canonical = store.events();
    assert!(
        canonical.iter().any(|event| event.base().id == compacted),
        "compacted source remains in the canonical audit log",
    );
    assert!(
        canonical.iter().any(|event| event.base().id == suppressed),
        "suppressed source remains in the canonical audit log",
    );
    Ok(())
}

#[test]
fn installed_projection_is_borrowed_without_rewalking_persisted_marks() -> TestResult {
    let store = EventStore::new();
    let event_id = store.append(user_msg("visible to installed tracker"))?;
    let mut persisted = ContextEdits::new();
    persisted.suppress(&store, event_id.clone())?;
    let installed = ContextEdits::new();

    let view = with_prompt_context_edits(&store, Some(&installed), |edits| {
        construct_prompt(&store, edits)
    });

    assert!(
        view.events.iter().any(|event| event.base().id == event_id),
        "an installed tracker is authoritative and must not trigger another store walk",
    );
    Ok(())
}

#[test]
fn suppressed_assistant_reasoning_absent_from_rebuilt_prompt() -> TestResult {
    // Compaction/suppression of an AssistantMessage must take its
    // reasoning with it: conversion reads events post-suppression, so a
    // suppressed turn contributes no reasoning to the rebuilt prompt
    // view. Without this, resume would re-inject encrypted reasoning for
    // a turn the model no longer sees.
    use crate::provider::reasoning::{ReasoningItem, ReasoningSummaryPart};
    use crate::session::conversion::prompt_events_to_messages;

    let store = EventStore::new();
    let keep = store.append(user_msg("keep"))?;
    let suppressed = store.append(SessionEvent::AssistantMessage {
        response_items: Vec::new(),
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
    })?;

    let mut edits = ContextEdits::new();
    edits.suppress(&store, suppressed)?;

    let view = construct_prompt(&store, &edits);
    let msgs = prompt_events_to_messages(&view.events);
    assert!(
        msgs.iter().all(|m| m.reasoning.is_empty()),
        "suppressed turn's reasoning must not survive into the prompt view",
    );
    Ok(())
}

#[test]
fn superseded_events_excluded_compaction_included() -> TestResult {
    let store = EventStore::new();
    let mut ids = Vec::new();
    for i in 0..10 {
        ids.push(store.append(user_msg(&format!("msg {i}")))?);
    }

    let mut edits = ContextEdits::new();
    let comp_ids = ids[0..3].to_vec();
    let comp_id = edits.summarize(&store, comp_ids, "summary".to_owned())?;

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
    Ok(())
}

#[test]
fn injected_events_tagged() -> TestResult {
    let store = EventStore::new();
    store.append(user_msg("normal"))?;

    let mut edits = ContextEdits::new();
    edits.inject(&store, user_msg("injected"))?;

    let view = construct_prompt(&store, &edits);
    assert_eq!(view.events.len(), 2);
    assert_eq!(view.tags[0], ContentTag::Message);
    assert_eq!(view.tags[1], ContentTag::Injection);
    Ok(())
}

#[test]
fn combined_suppress_and_compact() -> TestResult {
    let store = EventStore::new();
    let mut ids = Vec::new();
    for i in 0..10 {
        ids.push(store.append(user_msg(&format!("msg {i}")))?);
    }

    let mut edits = ContextEdits::new();
    edits.suppress(&store, ids[4].clone())?;
    edits.suppress(&store, ids[8].clone())?;
    edits.summarize(&store, ids[0..3].to_vec(), "compact summary".to_owned())?;

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
    Ok(())
}

#[test]
fn construct_prompt_does_not_mutate() -> TestResult {
    let store = EventStore::new();
    for i in 0..3 {
        store.append(user_msg(&format!("msg {i}")))?;
    }
    let edits = ContextEdits::new();

    let len_before = store.len();
    let _view = construct_prompt(&store, &edits);
    assert_eq!(store.len(), len_before, "store must not be mutated");
    Ok(())
}

#[test]
fn tags_match_events() -> TestResult {
    let store = EventStore::new();
    store.append(user_msg("user"))?;
    store.append(assistant_with_call("assistant", "tc1"))?;
    store.append(SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: "tc1".to_owned(),
        tool_name: "Read".to_owned(),
        output: serde_json::json!({}),
        spool_ref: None,
        duration_ms: 10,
    })?;

    let edits = ContextEdits::new();
    let view = construct_prompt(&store, &edits);

    assert_eq!(view.tags.len(), 3);
    assert_eq!(view.tags[0], ContentTag::Message);
    assert_eq!(view.tags[1], ContentTag::Message);
    assert_eq!(view.tags[2], ContentTag::ToolResult);
    Ok(())
}
