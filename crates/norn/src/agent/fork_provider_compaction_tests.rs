use crate::agent::ContextFilter;
use crate::provider::openai::output_item_test_fixtures::response_items_named;
use crate::provider::openai::request::build_payload;
use crate::provider::request::ProviderRequest;
use crate::session::ProviderFilteredForkBoundary;
use crate::session::ReplayArtifacts;
use crate::session::conversion::events_to_messages;
use crate::session::events::{ContextMarkKind, EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn provider_compaction_event() -> TestResult<SessionEvent> {
    let response_items = response_items_named("d3_lifecycle", &["compaction"])?;
    Ok(SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items,
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    })
}

fn payload_for(events: &[SessionEvent]) -> Result<serde_json::Value, crate::error::ProviderError> {
    build_payload(
        &ProviderRequest {
            messages: events_to_messages(events),
            tools: Vec::new(),
            model: "gpt-test".to_owned(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        },
        "test",
    )
}

#[test]
fn provider_compaction_survives_prompt_resume_and_identity_fork_verbatim() -> TestResult {
    let store = EventStore::new();
    store.append(provider_compaction_event()?)?;
    let canonical_before = serde_json::to_vec(&store.events())?;
    let original_payload = payload_for(&store.events())?;
    let original_item = original_payload["input"][0].clone();
    assert_eq!(original_item["type"], "compaction");
    assert_eq!(original_item["encrypted_content"], "opaque-compaction");
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        canonical_before,
        "prompt projection must not rewrite the audit row",
    );

    let recovered = ReplayArtifacts::from_events(store.events());
    let resumed = EventStore::new();
    for event in recovered.events {
        resumed.append(event)?;
    }
    let resumed_payload = payload_for(&resumed.events())?;
    assert_eq!(resumed_payload["input"][0], original_item);

    let identity_fork = ContextFilter::default().apply(&resumed.events())?;
    assert_eq!(
        serde_json::to_vec(&identity_fork)?,
        serde_json::to_vec(&resumed.events())?,
        "identity fork must preserve the complete durable event",
    );
    let fork_payload = payload_for(&identity_fork)?;
    assert_eq!(fork_payload["input"][0], original_item);
    Ok(())
}

#[test]
fn identity_filter_preserves_complete_durable_marked_history() -> TestResult {
    let user = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "superseded detail".to_owned(),
    };
    let provider_compaction = provider_compaction_event()?;
    let local_compaction = SessionEvent::Compaction {
        base: EventBase::new(None),
        summary: "local summary".to_owned(),
        replaced_event_ids: vec![user.base().id.clone()],
    };
    let suppression = SessionEvent::ContextMark {
        base: EventBase::new(None),
        mark: ContextMarkKind::Suppress,
        target_event_id: provider_compaction.base().id.clone(),
    };
    let injection = SessionEvent::ContextMark {
        base: EventBase::new(None),
        mark: ContextMarkKind::Inject,
        target_event_id: local_compaction.base().id.clone(),
    };
    let boundary = ProviderFilteredForkBoundary::into_event(EventBase::new(None));
    let source = vec![
        user,
        provider_compaction,
        local_compaction,
        suppression,
        injection,
        boundary,
    ];

    let copied = ContextFilter::default().apply(&source)?;
    assert_eq!(
        serde_json::to_vec(&copied)?,
        serde_json::to_vec(&source)?,
        "identity filtering must preserve content and durable bookkeeping rows",
    );
    Ok(())
}
