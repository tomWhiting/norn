use crate::agent::{ContextFilter, verify_no_orphan_tool_calls};
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::{
    ProviderFilteredForkBoundary, ResponseAudioArtifactLink, ResponseAudioArtifactRef,
    response_audio_artifact_links,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn filtered_payload(events: &[SessionEvent]) -> TestResult<&[SessionEvent]> {
    let Some((boundary, payload)) = events.split_last() else {
        return Err("filtered fork omitted its provider epoch boundary".into());
    };
    if !ProviderFilteredForkBoundary::is_family(boundary) {
        return Err("filtered fork ended without the expected boundary".into());
    }
    Ok(payload)
}

fn canonical_item(
    raw: serde_json::Value,
    sequence_number: u64,
) -> Result<ResponseTranscriptItem, crate::provider::ResponseItemError> {
    Ok(ResponseTranscriptItem {
        item: ResponseItem::from_value(raw)?,
        provenance: ResponseStreamProvenance {
            sequence_number: Some(sequence_number),
            ..ResponseStreamProvenance::default()
        },
    })
}

fn canonical_assistant(
    base: EventBase,
    raw_items: Vec<serde_json::Value>,
    compatibility_content: &str,
) -> Result<SessionEvent, crate::provider::ResponseItemError> {
    let response_items = raw_items
        .into_iter()
        .map(|raw| canonical_item(raw, 1))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SessionEvent::AssistantMessage {
        response_items,
        base,
        content: compatibility_content.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    })
}

fn function_call(call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": "read",
        "arguments": "{}",
        "status": "completed"
    })
}

fn function_output(call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "function_call_output",
        "id": format!("fco_{call_id}"),
        "call_id": call_id,
        "output": "complete",
        "status": "completed"
    })
}

#[test]
fn nonidentity_filter_removes_a_call_whose_output_is_compacted() -> TestResult {
    let call = canonical_assistant(
        EventBase::new(None),
        vec![function_call("reverse_split")],
        "",
    )?;
    let output = canonical_assistant(
        EventBase::new(None),
        vec![function_output("reverse_split")],
        "",
    )?;
    let summary = SessionEvent::Compaction {
        base: EventBase::new(None),
        summary: "completed tool interaction".to_owned(),
        replaced_event_ids: vec![output.base().id.clone()],
    };

    let filtered = ContextFilter {
        include_system: true,
        include_recent_n: Some(16),
        exclude_tool_calls: false,
    }
    .apply(&[call, output, summary])?;
    let payload = filtered_payload(&filtered)?;

    assert_eq!(payload.len(), 1);
    assert!(matches!(payload[0], SessionEvent::Compaction { .. }));
    assert!(verify_no_orphan_tool_calls(payload, "unrelated_fork_call").is_empty());
    Ok(())
}

#[test]
fn excluding_the_final_canonical_item_does_not_reactivate_flat_content() -> TestResult {
    const STALE_CONTENT: &str = "STALE-COMPATIBILITY-CONTENT";
    let event = canonical_assistant(
        EventBase::new(None),
        vec![function_call("canonical_only")],
        STALE_CONTENT,
    )?;

    let filtered = ContextFilter {
        include_system: true,
        include_recent_n: None,
        exclude_tool_calls: true,
    }
    .apply(&[event])?;
    let payload = filtered_payload(&filtered)?;

    assert!(payload.is_empty());
    assert!(!serde_json::to_string(&filtered)?.contains(STALE_CONTENT));
    Ok(())
}

#[test]
fn filtering_an_audio_linked_final_tool_item_removes_both_artifact_halves() -> TestResult {
    const STALE_CONTENT: &str = "STALE-AUDIO-COMPATIBILITY-CONTENT";
    let reference: ResponseAudioArtifactRef =
        serde_json::from_value(serde_json::json!("123e4567-e89b-42d3-a456-426614174000"))?;
    let link_base = EventBase::new(None);
    let assistant_base = EventBase::new(Some(link_base.id.clone()));
    let link = ResponseAudioArtifactLink::new(
        assistant_base.id.clone(),
        reference,
        Some("resp_audio_tool_only".to_owned()),
    )
    .into_custom_event(link_base)?;
    let assistant = canonical_assistant(
        assistant_base,
        vec![function_call("audio_tool_only")],
        STALE_CONTENT,
    )?;

    let filtered = ContextFilter {
        include_system: true,
        include_recent_n: None,
        exclude_tool_calls: true,
    }
    .apply(&[link, assistant])?;
    let payload = filtered_payload(&filtered)?;

    assert!(payload.is_empty());
    assert!(response_audio_artifact_links(&filtered)?.is_empty());
    assert!(!serde_json::to_string(&filtered)?.contains(STALE_CONTENT));
    Ok(())
}
