//! Failed automatic summaries preserve context, known spend, and the ability to retry.

use crate::error::{
    CompactionFailure, CompactionFailureReason, ErrorClass, NornError, ProviderError, SessionError,
};
use crate::r#loop::compaction::{
    AutoCompactArgs, AutoCompactDecision, CompactionState, maybe_auto_compact,
};
use crate::r#loop::retry::RetryPolicy;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::usage::Usage;
use crate::session::context_edit::ContextEdits;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn attempt(
    store: &EventStore,
    edits: &mut ContextEdits,
    state: &mut CompactionState,
    provider: &MockProvider,
) -> Result<AutoCompactDecision, SessionError> {
    maybe_auto_compact(AutoCompactArgs {
        state,
        edits: Some(edits),
        store,
        provider,
        model: "test-model",
        estimated_tokens: 100,
        usage_floor: None,
        context_window_limit: Some(100),
        reserve_tokens: Some(20),
        keep_recent_turns: 1,
        hooks: None,
        cancel: None,
        retry_policy: &RetryPolicy::default(),
        event_tx: None,
    })
    .await
}

fn response(text: &str, stop_reason: StopReason) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::TextDelta {
            text: text.to_owned(),
        },
        ProviderEvent::Done {
            stop_reason,
            usage: Usage {
                input_tokens: 23,
                output_tokens: 7,
                ..Usage::default()
            },
            response_id: None,
        },
    ]
}

async fn check_rejection(responses: Vec<Vec<ProviderEvent>>, has_usage: bool) -> TestResult {
    let store = EventStore::new();
    for content in ["FIRST USER", "MIDDLE USER", "LATEST USER"] {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        })?;
        store.append(SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            response_items: Vec::new(),
            content: format!("answer to {content}"),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_owned(),
            response_id: None,
        })?;
    }
    let mut edits = ContextEdits::new();
    let mut state = CompactionState::new();
    let original_count = store.len();
    let original = serde_json::to_value(store.events())?;
    let provider = MockProvider::new(responses);
    let failure = match attempt(&store, &mut edits, &mut state, &provider).await {
        Err(SessionError::CompactionSummaryFailed(failure)) => failure,
        other => {
            return Err(std::io::Error::other(format!(
                "expected failed compaction, got {other:?}"
            ))
            .into());
        }
    };
    assert_eq!(provider.call_count(), 1);
    assert!(!state.has_fired());
    assert_eq!(serde_json::to_value(store.events())?, original);
    assert_eq!(failure.model, "test-model");
    assert_eq!(failure.usage.is_some(), has_usage);
    assert_eq!(failure.reason.class(), ErrorClass::Terminal);
    if has_usage {
        assert_eq!(
            failure.usage.as_ref().map(|usage| usage.input_tokens),
            Some(23)
        );
        assert_eq!(
            failure.usage.as_ref().map(|usage| usage.output_tokens),
            Some(7)
        );
        assert!(matches!(
            failure.reason,
            CompactionFailureReason::Unusable { .. }
        ));
    } else {
        assert!(matches!(
            failure.reason,
            CompactionFailureReason::Provider(_)
        ));
        assert!(failure.to_string().contains("MockProvider"));
    }
    let retry = MockProvider::new(vec![response("RECOVERED SUMMARY", StopReason::EndTurn)]);
    assert!(matches!(
        attempt(&store, &mut edits, &mut state, &retry).await?,
        AutoCompactDecision::Fired(_)
    ));
    assert!(state.has_fired());
    let events = store.events();
    assert_eq!(serde_json::to_value(&events[..original_count])?, original);
    assert!(events.iter().any(|event| matches!(event, SessionEvent::Compaction { summary, .. } if summary == "RECOVERED SUMMARY")));
    let prompt = retry
        .requests()?
        .into_iter()
        .flat_map(|request| request.messages)
        .filter_map(|message| message.content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains("FIRST USER"));
    assert!(prompt.contains("MIDDLE USER"));
    Ok(())
}

#[tokio::test]
async fn provider_failure_preserves_history_and_allows_retry() -> TestResult {
    check_rejection(Vec::new(), false).await
}

#[tokio::test]
async fn truncated_summary_preserves_history_and_usage() -> TestResult {
    check_rejection(vec![response("cut off", StopReason::MaxTokens)], true).await
}

#[tokio::test]
async fn empty_summary_preserves_history_and_usage() -> TestResult {
    check_rejection(vec![response("  ", StopReason::EndTurn)], true).await
}

#[tokio::test]
async fn provider_failure_receipt_preserves_cause_and_unknown_usage() -> TestResult {
    let store = EventStore::new();
    let failure = CompactionFailure {
        model: "test-model".to_owned(),
        reason: CompactionFailureReason::Provider(Box::new(NornError::Provider(
            ProviderError::StreamError {
                reason: "fixture bad request".to_owned(),
                transient: None,
            },
        ))),
        usage: None,
    };
    super::record_failure(&store, None, &failure).await?;
    let events = store.events();
    let data = events
        .first()
        .and_then(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == "loop.compaction_failed" => Some(data),
            _ => None,
        })
        .ok_or_else(|| std::io::Error::other("missing failure audit"))?;
    assert_eq!(data["schema_version"], 1);
    assert_eq!(data["failure_kind"], "provider_error");
    assert_eq!(data["context_changed"], false);
    assert!(data["usage"].is_null());
    assert!(
        data["error"]
            .as_str()
            .is_some_and(|error| error.contains("fixture bad request"))
    );
    let error = NornError::Session(SessionError::CompactionSummaryFailed(Box::new(failure)));
    assert_eq!(error.class(), ErrorClass::Terminal);
    Ok(())
}
