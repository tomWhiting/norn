//! Repeated compaction preserves prior summaries without resurrecting hidden history.

use crate::r#loop::compaction::{
    AutoCompactArgs, AutoCompactDecision, CompactionState, maybe_auto_compact,
};
use crate::r#loop::retry::RetryPolicy;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::provider::usage::Usage;
use crate::session::context_edit::ContextEdits;
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use crate::session::store::EventStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn user(content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: content.to_owned(),
    }
}

fn assistant(content: &str, tool_calls: Vec<ToolCallEvent>) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: content.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls,
        usage: EventUsage::default(),
        stop_reason: String::new(),
        response_id: None,
    }
}

async fn compact(
    store: &EventStore,
    edits: &mut ContextEdits,
) -> Result<String, Box<dyn std::error::Error>> {
    let provider = MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "NEXT SUMMARY".to_owned(),
        },
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        },
    ]]);
    let mut state = CompactionState::new();
    let before = serde_json::to_value(store.events())?;
    let count = store.len();
    let decision = maybe_auto_compact(AutoCompactArgs {
        state: &mut state,
        edits: Some(edits),
        store,
        provider: &provider,
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
    .await?;
    assert!(matches!(decision, AutoCompactDecision::Fired(_)));
    assert_eq!(store.len(), count + 1);
    assert_eq!(serde_json::to_value(&store.events()[..count])?, before);
    let requests = provider.requests()?;
    assert_eq!(requests.len(), 1);
    let request = requests
        .first()
        .ok_or_else(|| std::io::Error::other("missing compaction request"))?;
    assert!(request.tools.is_empty());
    assert!(!request.store);
    assert!(request.previous_response_id.is_none());
    Ok(request
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n"))
}

async fn repeated_compaction(restored: bool) -> TestResult {
    let store = EventStore::new();
    let mut edits = ContextEdits::new();
    let original_user = store.append(user("SUPERSEDED ORIGINAL USER"))?;
    let original_answer = store.append(assistant("SUPERSEDED ORIGINAL ANSWER", Vec::new()))?;
    edits.summarize(
        &store,
        vec![original_user, original_answer],
        "PRIOR SUMMARY".to_owned(),
    )?;
    let hidden = store.append(user("SUPPRESSED MESSAGE"))?;
    edits.suppress(&store, hidden)?;
    store.append(user("MIDDLE USER"))?;
    store.append(assistant("MIDDLE ANSWER", Vec::new()))?;
    store.append(user("LATEST USER"))?;
    store.append(assistant("LATEST ANSWER", Vec::new()))?;
    if restored {
        edits = ContextEdits::new();
        edits.apply_persisted_marks(&store);
    }
    let input = compact(&store, &mut edits).await?;
    assert!(
        !input.contains("SUPERSEDED ORIGINAL"),
        "old originals must not re-enter the summary request"
    );
    assert!(!input.contains("SUPPRESSED MESSAGE"));
    assert_eq!(input.matches("PRIOR SUMMARY").count(), 1);
    assert!(input.contains("MIDDLE USER"));
    assert!(input.contains("MIDDLE ANSWER"));
    assert!(!input.contains("LATEST USER"));
    assert!(!input.contains("LATEST ANSWER"));
    store.append(user("BRIDGE USER"))?;
    store.append(assistant("BRIDGE ANSWER", Vec::new()))?;
    store.append(user("NEWEST USER"))?;
    store.append(assistant("NEWEST ANSWER", Vec::new()))?;
    let following = compact(&store, &mut edits).await?;
    assert!(!following.contains("SUPERSEDED ORIGINAL"));
    assert!(!following.contains("PRIOR SUMMARY"));
    assert!(!following.contains("MIDDLE ANSWER"));
    assert_eq!(following.matches("NEXT SUMMARY").count(), 1);
    assert!(following.contains("LATEST ANSWER"));
    assert!(!following.contains("NEWEST ANSWER"));
    Ok(())
}

#[tokio::test]
async fn repeated_compaction_uses_current_prompt_instead_of_raw_history() -> TestResult {
    repeated_compaction(false).await
}

#[tokio::test]
async fn restored_marks_keep_hidden_history_out_of_summary_request() -> TestResult {
    repeated_compaction(true).await
}

#[tokio::test]
async fn suppressed_result_does_not_resurrect_its_tool_call_in_summary() -> TestResult {
    let store = EventStore::new();
    let mut edits = ContextEdits::new();
    store.append(user("TASK"))?;
    store.append(assistant(
        "INSPECTING",
        vec![ToolCallEvent {
            call_id: "call-hidden".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({"path":"HIDDEN_TOOL_PATH"}),
            kind: ToolCallKind::Function,
            caller: ToolCallCaller::Absent,
        }],
    ))?;
    let result = store.append(SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: "call-hidden".to_owned(),
        tool_name: "read".to_owned(),
        output: serde_json::json!({"content":"HIDDEN_TOOL_RESULT"}),
        spool_ref: None,
        duration_ms: 1,
    })?;
    edits.suppress(&store, result)?;
    store.append(user("RETAINED USER"))?;
    store.append(assistant("RETAINED ANSWER", Vec::new()))?;
    let input = compact(&store, &mut edits).await?;
    assert!(input.contains("INSPECTING"));
    assert!(!input.contains("HIDDEN_TOOL_PATH"));
    assert!(!input.contains("HIDDEN_TOOL_RESULT"));
    Ok(())
}
