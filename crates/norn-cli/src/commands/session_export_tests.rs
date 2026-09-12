//! Markdown export reads canonical items while preserving legacy exports.

use norn::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use norn::session::events::{EventBase, EventUsage, SessionEvent};

use super::write_assistant;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn assistant(content: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: content.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    }
}

#[test]
fn canonical_markdown_uses_authoritative_text_and_tool_calls() -> TestResult {
    let mut event = assistant("stale duplicate text");
    if let SessionEvent::AssistantMessage { response_items, .. } = &mut event {
        for raw in [
            serde_json::json!({"type":"message", "id":"msg_export", "status":"completed", "role":"assistant", "content":[{"type":"output_text", "text":"Canonical text.", "annotations":[], "logprobs":[]}]}),
            serde_json::json!({"type":"function_call", "id":"fc_export", "status":"completed", "call_id":"call_export", "name":"read_file", "arguments":"{\"path\":\"README.md\"}"}),
        ] {
            response_items.push(ResponseTranscriptItem {
                item: ResponseItem::from_value(raw)?,
                provenance: ResponseStreamProvenance::default(),
            });
        }
    }
    let mut output = Vec::new();
    write_assistant(&mut output, &event)?;
    let text = String::from_utf8(output)?;
    assert_eq!(
        text,
        "## Assistant\n\nCanonical text.\n\n### Tool Call: read_file\n\n```json\n{\"path\":\"README.md\"}\n```\n\n"
    );
    Ok(())
}

#[test]
fn legacy_markdown_retains_flat_text() -> TestResult {
    let mut output = Vec::new();
    write_assistant(&mut output, &assistant("Legacy text."))?;
    assert_eq!(
        String::from_utf8(output)?,
        "## Assistant\n\nLegacy text.\n\n"
    );
    Ok(())
}

#[test]
fn markdown_writer_propagates_output_failure() {
    let mut output = &mut [0_u8; 1][..];
    let result = write_assistant(&mut output, &assistant("Cannot fit."));
    assert!(result.is_err_and(|error| error.kind() == std::io::ErrorKind::WriteZero));
}
