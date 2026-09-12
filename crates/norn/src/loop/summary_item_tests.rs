//! Summary requests exclude ciphertext without changing durable items or ordinary replay.

use crate::r#loop::retry::RetryPolicy;
use crate::r#loop::summarization::{
    SummarizationOutcome, SummarizationRetry, request_compaction_summary,
};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::provider::usage::Usage;
use crate::session::conversion::prompt_events_to_messages;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn assistant(
    raw: Vec<Value>,
) -> Result<SessionEvent, crate::provider::response_item::ResponseItemError> {
    let response_items = raw
        .into_iter()
        .map(|value| {
            ResponseItem::from_value(value).map(|item| ResponseTranscriptItem {
                item,
                provenance: ResponseStreamProvenance::default(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items,
        content: "stale compatibility text".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    })
}

#[tokio::test]
async fn actual_summary_request_omits_blobs_but_retains_readable_fields_and_replay() -> TestResult {
    let payload = "opaque-密文-".repeat(8192);
    let reasoning = json!({"type":"reasoning", "id":"rs-retained", "summary":[{"type":"summary_text","text":"Keep the customer's constraints"}], "content":[{"type":"reasoning_text","text":"Visible reasoning content"}], "encrypted_content":payload, "future_metadata":{"reference":"retained-reference"}});
    let compaction =
        json!({"type":"compaction", "id":"compact-retained", "encrypted_content":payload});
    let call = json!({"type":"function_call", "id":"fc-retained", "call_id":"call-retained", "name":"inspect", "arguments":"{\"encrypted_content\":\"application-field-must-stay\"}"});
    let events = vec![assistant(vec![
        reasoning.clone(),
        compaction.clone(),
        call.clone(),
    ])?];
    let original = serde_json::to_value(&events)?;
    let provider = MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "usable summary".to_owned(),
        },
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        },
    ]]);
    let outcome = request_compaction_summary(
        &provider,
        "test-model",
        &events,
        SummarizationRetry {
            policy: &RetryPolicy::default(),
            cancel: None,
            event_tx: None,
        },
    )
    .await;
    assert!(matches!(outcome, SummarizationOutcome::Completed(_)));
    let requests = provider.requests()?;
    assert_eq!(requests.len(), 1);
    let input = requests
        .first()
        .ok_or_else(|| std::io::Error::other("summary request missing"))?
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!input.contains("opaque-密文-"));
    assert!(input.contains("Keep the customer's constraints"));
    assert!(input.contains("Visible reasoning content"));
    assert!(input.contains("retained-reference"));
    assert!(input.contains("rs-retained"));
    assert!(input.contains("compact-retained"));
    assert!(input.contains("application-field-must-stay"));
    assert!(input.contains(&format!("omitted {} bytes", payload.len())));
    assert_eq!(input.matches("original provider item retained").count(), 2);
    assert!(input.len() < payload.len() / 10);
    assert!(!input.contains("stale compatibility text"));
    assert_eq!(serde_json::to_value(&events)?, original);
    let replay = prompt_events_to_messages(&events);
    let replay_items = &replay
        .first()
        .ok_or_else(|| std::io::Error::other("replay message missing"))?
        .response_items;
    assert_eq!(
        replay_items
            .iter()
            .map(|entry| entry.item.raw().clone())
            .collect::<Vec<_>>(),
        vec![reasoning, compaction, call]
    );
    Ok(())
}

#[test]
fn absent_null_and_empty_reasoning_payloads_are_unchanged() -> TestResult {
    for field in [None, Some(Value::Null), Some(json!(""))] {
        let mut raw = json!({"type":"reasoning", "id":"rs-empty", "summary":[]});
        if let Some(value) = field {
            raw["encrypted_content"] = value;
        }
        let item = ResponseItem::from_value(raw.clone())?;
        assert_eq!(super::render(&item), raw.to_string());
    }
    Ok(())
}

#[test]
fn unknown_item_and_application_fields_are_not_recursively_redacted() -> TestResult {
    for raw in [
        json!({"type":"future_item", "id":"future-1", "encrypted_content":"unknown-contract-payload"}),
        json!({"type":"function_call", "call_id":"call-1", "name":"inspect", "arguments":"{\"encrypted_content\":\"application-payload\"}", "encrypted_content":"unknown-extension"}),
        json!({"type":"mcp_call", "id":"mcp-1", "encrypted_content":"unknown-hosted-extension"}),
    ] {
        let item = ResponseItem::from_value(raw.clone())?;
        assert_eq!(super::render(&item), raw.to_string());
    }
    Ok(())
}
