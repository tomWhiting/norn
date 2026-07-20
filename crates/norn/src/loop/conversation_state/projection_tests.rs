use super::test_support::{append_stored_assistant, config, message, stored_assistant_events};
use super::*;
use crate::provider::request::MessageRole;
use crate::rules::types::{DeliveryMode, TriggerTiming};
use crate::session::ProviderStateProvenance;
use crate::session::events::{EventBase, EventUsage, ProviderEpochBoundaryReason};
use crate::session::store::EventStore;

/// `event_produces_prompt_message` must agree exactly with the message
/// projection in `session::conversion` for every delivery mode: a
/// `RuleInjection` that `conversion` renders to a live message must be
/// counted, and one it drops must not be. Divergence silently breaks
/// in-flight compaction and the Responses-API thread anchor, which
/// count messages through this predicate while the message list is
/// built by `conversion`.
#[test]
fn rule_injection_prompt_message_predicate_mirrors_conversion() {
    for delivery in [
        DeliveryMode::SystemContextAppend,
        DeliveryMode::ContextInjection,
        DeliveryMode::MessageDelivery,
    ] {
        let label = format!("{delivery:?}");
        let event = SessionEvent::RuleInjection {
            base: EventBase::new(None),
            rule_id: "rust-conventions".to_owned(),
            delivery,
            timing: TriggerTiming::After,
            content: "Follow conventions.".to_owned(),
        };
        let rendered =
            !crate::session::conversion::events_to_messages(std::slice::from_ref(&event))
                .is_empty();
        assert_eq!(
            event_produces_prompt_message(&event, true),
            rendered,
            "predicate diverges from conversion for delivery {label}",
        );
    }
}

#[test]
fn response_ids_without_matching_durable_provenance_are_not_anchors()
-> Result<(), Box<dyn std::error::Error>> {
    let unproven = SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: "stateless answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_not_stored".to_owned()),
    };
    assert!(latest_response_anchor(std::slice::from_ref(&unproven), 1, false)?.is_none());

    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let orphan = ProviderStateProvenance::new(crate::session::events::EventId::new(), true)
        .into_custom_event(EventBase::new(Some(boundary.base().id.clone())))?;
    let wrongly_targeted = SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(Some(orphan.base().id.clone())),
        content: "wrongly targeted answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_wrong_target".to_owned()),
    };
    let events = vec![boundary, orphan.clone(), wrongly_targeted];
    assert!(matches!(
        latest_response_anchor(&events, 1, false),
        Err(ProviderError::ProviderStateProvenanceInvalid)
    ));
    assert!(!event_produces_prompt_message(&orphan, false));
    assert!(crate::session::conversion::events_to_messages(&[orphan]).is_empty());
    Ok(())
}

#[test]
fn threaded_request_replaces_and_removes_managed_instructions()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    append_stored_assistant(&store, "old answer", "resp_old")?;

    let state = ConversationRequestState::new(
        &config(ConversationStateMode::ProviderThreaded),
        ProviderCapabilities::openai_responses(),
        1,
        latest_response_anchor(&store.events(), 1, false)?,
    )?;
    // The live layout remains event-backed. Dynamic context is supplied
    // separately so public threading projects it into replaceable
    // top-level instructions instead of durable provider input.
    let messages = vec![
        message(MessageRole::System, "system"),
        message(MessageRole::Assistant, "old answer"),
        message(MessageRole::User, "new"),
    ];

    let request_messages =
        state.request_messages_with_managed_instructions(&messages, Some("dynamic".to_owned()));

    assert_eq!(state.previous_response_id().as_deref(), Some("resp_old"));
    assert_eq!(request_messages.len(), 3);
    assert_eq!(request_messages[0].role, MessageRole::System);
    assert_eq!(request_messages[1].content.as_deref(), Some("new"));
    assert_eq!(
        request_messages[2].role,
        MessageRole::System,
        "the managed context must project into top-level instructions",
    );
    assert_eq!(request_messages[2].content.as_deref(), Some("dynamic"));

    let request_without_managed_context =
        state.request_messages_with_managed_instructions(&messages, None);

    assert_eq!(state.previous_response_id().as_deref(), Some("resp_old"));
    assert_eq!(request_without_managed_context.len(), 2);
    assert_eq!(request_without_managed_context[0].role, MessageRole::System);
    assert_eq!(
        request_without_managed_context[0].content.as_deref(),
        Some("system")
    );
    assert_eq!(request_without_managed_context[1].role, MessageRole::User);
    assert_eq!(
        request_without_managed_context[1].content.as_deref(),
        Some("new")
    );
    assert!(
        request_without_managed_context
            .iter()
            .all(|message| message.content.as_deref() != Some("dynamic"))
    );
    Ok(())
}

#[test]
fn provider_epoch_boundary_clears_old_anchor_and_allows_new_anchor()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    append_stored_assistant(&store, "legacy answer", "resp_legacy")?;
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: ProviderEpochBoundaryReason::MigratedLegacy,
    };
    store.append(boundary.clone())?;

    assert!(!event_produces_prompt_message(&boundary, true));
    assert!(crate::session::conversion::events_to_messages(&[boundary]).is_empty());
    assert!(latest_response_anchor(&store.events(), 1, false)?.is_none());

    append_stored_assistant(&store, "native answer", "resp_native")?;
    let Some(anchor) = latest_response_anchor(&store.events(), 1, false)? else {
        return Err(std::io::Error::other("native response did not establish an anchor").into());
    };
    assert_eq!(anchor.response_id, "resp_native");
    assert_eq!(anchor.input_start, 3);
    Ok(())
}

#[test]
fn compaction_cuts_old_anchor_and_allows_new_anchor() -> Result<(), Box<dyn std::error::Error>> {
    let mut events = stored_assistant_events("old answer", "resp_old")?;
    events.push(SessionEvent::Compaction {
        base: EventBase::new(None),
        summary: "compacted history".to_owned(),
        replaced_event_ids: Vec::new(),
    });

    assert!(latest_response_anchor(&events, 1, true)?.is_none());

    events.extend(stored_assistant_events("new answer", "resp_new")?);
    let Some(anchor) = latest_response_anchor(&events, 1, true)? else {
        return Err(
            std::io::Error::other("post-compaction response did not establish an anchor").into(),
        );
    };
    assert_eq!(anchor.response_id, "resp_new");
    assert_eq!(
        anchor.input_start, 4,
        "message indexing must include the visible pre-cut response and summary",
    );
    Ok(())
}

#[test]
fn suppress_cuts_old_anchor_while_inject_does_not() -> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    append_stored_assistant(&store, "old answer", "resp_old")?;
    let mut edits = crate::session::context_edit::ContextEdits::new();
    let injected_id = edits.inject(
        &store,
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "injected context".to_owned(),
        },
    )?;

    let injected_view = crate::r#loop::context::construct_prompt(&store, &edits);
    let Some(anchor) =
        latest_response_anchor_for_prompt_view(&injected_view.events, &store, 1, true)?
    else {
        return Err(std::io::Error::other("injection unexpectedly cut the anchor").into());
    };
    assert_eq!(anchor.response_id, "resp_old");
    assert_eq!(anchor.input_start, 2);

    edits.suppress(&store, injected_id)?;
    let suppressed_view = crate::r#loop::context::construct_prompt(&store, &edits);
    assert!(
        latest_response_anchor_for_prompt_view(&suppressed_view.events, &store, 1, true)?.is_none(),
        "the invisible durable suppression mark must cut the old anchor",
    );

    append_stored_assistant(&store, "new answer", "resp_new")?;
    let resumed_view = crate::r#loop::context::construct_prompt(&store, &edits);
    let Some(anchor) =
        latest_response_anchor_for_prompt_view(&resumed_view.events, &store, 1, true)?
    else {
        return Err(
            std::io::Error::other("post-suppression response did not establish an anchor").into(),
        );
    };
    assert_eq!(anchor.response_id, "resp_new");
    assert_eq!(
        anchor.input_start, 3,
        "message indexing must retain the visible pre-cut response",
    );
    Ok(())
}
