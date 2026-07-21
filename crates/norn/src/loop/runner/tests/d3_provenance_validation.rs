use std::io;

use super::*;
use crate::session::{PROVIDER_STATE_PROVENANCE_EVENT_TYPE, ProviderStateProvenance};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const MALFORMED_PAYLOAD_SENTINEL: &str = "PROVENANCE-PAYLOAD-MUST-NOT-LEAK";

fn unbound_fixture() -> (EventStore, MockProvider) {
    let identity = crate::provider::ProviderStateIdentity::derive(
        "norn.runner.provenance-validation",
        b"provenance-validation-fixture",
    );
    let provider =
        MockProvider::with_capabilities(Vec::new(), ProviderCapabilities::openai_responses())
            .with_state_identity(identity);
    let store = EventStore::new();
    (store, provider)
}

fn bound_fixture() -> Result<(EventStore, MockProvider), crate::session::SessionPersistError> {
    let (store, provider) = unbound_fixture();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    Ok((store, provider))
}

fn append_user(store: &EventStore) -> Result<(), crate::error::SessionError> {
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(store.last_event_id()),
        content: "persisted prompt".to_owned(),
    })?;
    Ok(())
}

fn append_provenance(
    store: &EventStore,
    boundary_base: EventBase,
    target: crate::session::events::EventId,
    stored: bool,
) -> TestResult<crate::session::events::EventId> {
    let boundary_id = boundary_base.id.clone();
    store.append_unvalidated_for_test(SessionEvent::ProviderEpochBoundary {
        base: boundary_base,
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    })?;
    let provenance_base = EventBase::new(Some(boundary_id));
    let provenance_id = provenance_base.id.clone();
    let event = ProviderStateProvenance::new(target, stored).into_custom_event(provenance_base)?;
    store.append_unvalidated_for_test(event)?;
    Ok(provenance_id)
}

fn append_malformed_framed_provenance(store: &EventStore) -> TestResult {
    let boundary = EventBase::new(store.last_event_id());
    let boundary_id = boundary.id.clone();
    store.append_unvalidated_for_test(SessionEvent::ProviderEpochBoundary {
        base: boundary,
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    })?;
    store.append_unvalidated_for_test(SessionEvent::Custom {
        base: EventBase::new(Some(boundary_id)),
        event_type: PROVIDER_STATE_PROVENANCE_EVENT_TYPE.to_owned(),
        data: serde_json::json!({
            "version": 1,
            "assistant_event_id": MALFORMED_PAYLOAD_SENTINEL,
            "stored": true,
        }),
    })?;
    Ok(())
}

fn append_suppression_cut(store: &EventStore) -> TestResult {
    let target_event_id = store
        .last_event_id()
        .ok_or_else(|| io::Error::other("suppression cut requires a preceding event"))?;
    store.append_unvalidated_for_test(SessionEvent::ContextMark {
        base: EventBase::new(Some(target_event_id.clone())),
        mark: crate::session::events::ContextMarkKind::Suppress,
        target_event_id,
    })?;
    Ok(())
}

fn assistant_event(base: EventBase, response_id: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base,
        content: "persisted answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    }
}

fn append_duplicate_target_records(
    store: &EventStore,
    dispositions: (bool, bool),
    response_id: &str,
) -> TestResult<crate::session::events::EventId> {
    let head = EventBase::new(store.last_event_id());
    let target = EventBase::new(None);
    let target_id = target.id.clone();
    append_provenance(store, head, target.id.clone(), dispositions.0)?;
    let tail = EventBase::new(store.last_event_id());
    let tail_id = append_provenance(store, tail, target.id.clone(), dispositions.1)?;
    let mut target = target;
    target.parent_id = Some(tail_id);
    store.append_unvalidated_for_test(assistant_event(target, response_id))?;
    Ok(target_id)
}

async fn assert_setup_rejects_without_effects(
    store: &EventStore,
    provider: &MockProvider,
    forbidden_values: &[String],
) -> TestResult {
    let before = serde_json::to_vec(&store.events())?;
    let identity_before = store.provider_state_identity();
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();
    let mut loop_context = LoopContext::new("system");

    let result = run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt: "must not persist",
        tools: &[],
        output_schema: None,
        model: "gpt-test",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await;
    let Err(error) = result else {
        return Err(io::Error::other("invalid provenance unexpectedly reached the runner").into());
    };

    assert!(matches!(
        &error,
        NornError::Provider(ProviderError::ProviderStateProvenanceInvalid)
    ));
    let rendered = error.to_string();
    let debug = format!("{error:?}");
    assert_eq!(
        rendered,
        "provider error: provider state provenance is invalid"
    );
    for forbidden in forbidden_values {
        assert!(!rendered.contains(forbidden.as_str()));
        assert!(!debug.contains(forbidden.as_str()));
    }
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        before,
        "provenance validation must precede prompt persistence",
    );
    assert_eq!(
        store.provider_state_identity(),
        identity_before,
        "provenance validation must precede provider-affinity adoption",
    );
    assert_eq!(
        provider.call_count(),
        0,
        "provenance validation must precede provider dispatch",
    );
    assert!(provider.requests()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn malformed_reserved_provenance_fails_closed_before_mutation_or_dispatch() -> TestResult {
    let (store, provider) = bound_fixture()?;
    append_user(&store)?;
    append_malformed_framed_provenance(&store)?;

    assert_setup_rejects_without_effects(
        &store,
        &provider,
        &[MALFORMED_PAYLOAD_SENTINEL.to_owned()],
    )
    .await
}

#[tokio::test]
async fn malformed_provenance_cannot_bind_an_unbound_store() -> TestResult {
    let (store, provider) = unbound_fixture();
    append_user(&store)?;
    append_malformed_framed_provenance(&store)?;
    assert!(store.provider_state_identity().is_none());

    assert_setup_rejects_without_effects(
        &store,
        &provider,
        &[MALFORMED_PAYLOAD_SENTINEL.to_owned()],
    )
    .await
}

#[tokio::test]
async fn malformed_provenance_before_a_cut_fails_globally_before_effects() -> TestResult {
    let (store, provider) = bound_fixture()?;
    append_user(&store)?;
    append_malformed_framed_provenance(&store)?;
    append_suppression_cut(&store)?;

    assert_setup_rejects_without_effects(
        &store,
        &provider,
        &[MALFORMED_PAYLOAD_SENTINEL.to_owned()],
    )
    .await
}

#[test]
fn unframed_legacy_discriminators_remain_application_data_after_a_publication() -> TestResult {
    let (store, _provider) = bound_fixture()?;
    append_user(&store)?;
    let mut assistant_base = EventBase::new(None);
    let provenance_id = append_provenance(
        &store,
        EventBase::new(store.last_event_id()),
        assistant_base.id.clone(),
        true,
    )?;
    assistant_base.parent_id = Some(provenance_id);
    store.append_unvalidated_for_test(assistant_event(assistant_base, "resp_valid-publication"))?;
    for event_type in [
        PROVIDER_STATE_PROVENANCE_EVENT_TYPE,
        "provider.epoch.filtered_fork",
    ] {
        store.append(SessionEvent::Custom {
            base: EventBase::new(store.last_event_id()),
            event_type: event_type.to_owned(),
            data: serde_json::json!({"application": MALFORMED_PAYLOAD_SENTINEL}),
        })?;
    }
    crate::session::validate_provider_state_provenance(&store.events())?;
    let view = crate::r#loop::context::construct_prompt(
        &store,
        &crate::session::context_edit::ContextEdits::new(),
    );
    assert_eq!(view.events.len(), 4);
    assert_eq!(view.tags.len(), 4);
    assert!(view.tags.iter().any(|tag| matches!(
        tag,
        crate::r#loop::context::ContentTag::Custom(name)
            if name == PROVIDER_STATE_PROVENANCE_EVENT_TYPE
    )));
    Ok(())
}

async fn assert_orphan_rejected(stored: bool) -> TestResult {
    let (store, provider) = bound_fixture()?;
    append_user(&store)?;
    let target = crate::session::events::EventId::new();
    append_provenance(
        &store,
        EventBase::new(store.last_event_id()),
        target.clone(),
        stored,
    )?;

    assert_setup_rejects_without_effects(&store, &provider, &[target.to_string()]).await
}

#[tokio::test]
async fn orphan_stored_provenance_fails_closed_before_mutation_or_dispatch() -> TestResult {
    assert_orphan_rejected(true).await
}

#[tokio::test]
async fn orphan_not_stored_provenance_fails_closed_before_mutation_or_dispatch() -> TestResult {
    assert_orphan_rejected(false).await
}

#[tokio::test]
async fn orphan_provenance_before_a_cut_fails_globally_before_effects() -> TestResult {
    let (store, provider) = bound_fixture()?;
    append_user(&store)?;
    let target = crate::session::events::EventId::new();
    append_provenance(
        &store,
        EventBase::new(store.last_event_id()),
        target.clone(),
        true,
    )?;
    append_suppression_cut(&store)?;

    assert_setup_rejects_without_effects(&store, &provider, &[target.to_string()]).await
}

#[tokio::test]
async fn cut_between_provenance_and_target_fails_before_effects() -> TestResult {
    let (store, provider) = bound_fixture()?;
    append_user(&store)?;
    let user_id = store
        .last_event_id()
        .ok_or_else(|| io::Error::other("fixture user event must exist"))?;
    let mut assistant_base = EventBase::new(None);
    let target = assistant_base.id.clone();
    let boundary_base = EventBase::new(Some(user_id.clone()));
    let provenance_id = append_provenance(&store, boundary_base, target.clone(), true)?;
    let cut_base = EventBase::new(Some(provenance_id.clone()));
    assistant_base.parent_id = Some(cut_base.id.clone());
    store.append_unvalidated_for_test(SessionEvent::ContextMark {
        base: cut_base,
        mark: crate::session::events::ContextMarkKind::Suppress,
        target_event_id: user_id,
    })?;
    store.append_unvalidated_for_test(assistant_event(
        assistant_base,
        "resp_cut-between-provenance-and-target",
    ))?;

    assert_setup_rejects_without_effects(&store, &provider, &[target.to_string()]).await
}

#[tokio::test]
async fn input_interleaved_between_provenance_and_target_fails_before_effects() -> TestResult {
    let (store, provider) = bound_fixture()?;
    append_user(&store)?;
    let boundary_base = EventBase::new(store.last_event_id());
    let assistant_base = EventBase::new(None);
    let target = assistant_base.id.clone();
    let provenance_id = append_provenance(&store, boundary_base, target.clone(), true)?;
    store.append_unvalidated_for_test(SessionEvent::UserMessage {
        base: EventBase::new(store.last_event_id()),
        content: MALFORMED_PAYLOAD_SENTINEL.to_owned(),
    })?;
    let mut assistant_base = assistant_base;
    assistant_base.parent_id = Some(provenance_id);
    store.append_unvalidated_for_test(assistant_event(
        assistant_base,
        "resp_interleaved-provider-state",
    ))?;

    assert_setup_rejects_without_effects(
        &store,
        &provider,
        &[MALFORMED_PAYLOAD_SENTINEL.to_owned(), target.to_string()],
    )
    .await
}

#[tokio::test]
async fn duplicate_same_disposition_fails_closed_before_mutation_or_dispatch() -> TestResult {
    let (store, provider) = bound_fixture()?;
    append_user(&store)?;
    let response_id = "resp_duplicate-provenance-secret";
    let target = append_duplicate_target_records(&store, (true, true), response_id)?;
    append_suppression_cut(&store)?;

    assert_setup_rejects_without_effects(
        &store,
        &provider,
        &[response_id.to_owned(), target.to_string()],
    )
    .await
}

#[tokio::test]
async fn conflicting_dispositions_fail_closed_before_mutation_or_dispatch() -> TestResult {
    let (store, provider) = bound_fixture()?;
    append_user(&store)?;
    let response_id = "resp_conflicting-provenance-secret";
    let target = append_duplicate_target_records(&store, (true, false), response_id)?;
    append_suppression_cut(&store)?;

    assert_setup_rejects_without_effects(
        &store,
        &provider,
        &[response_id.to_owned(), target.to_string()],
    )
    .await
}
