//! Summary evidence retains source identity without changing stored events or prompt replay.

use super::render_transcript;
use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn base(id: &str, parent: Option<&str>) -> Result<EventBase, Box<dyn std::error::Error>> {
    Ok(EventBase {
        id: id.parse()?,
        parent_id: parent.map(str::parse).transpose()?,
        timestamp: chrono::DateTime::parse_from_rfc3339("2026-09-13T01:02:03+00:00")?
            .with_timezone(&chrono::Utc),
    })
}

fn events() -> Result<Vec<SessionEvent>, Box<dyn std::error::Error>> {
    Ok(vec![
        SessionEvent::UserMessage {
            base: base("user\nmarker", None)?,
            content: "Keep the authorised worktree scope".to_owned(),
        },
        SessionEvent::Custom {
            base: base("metadata", Some("user\nmarker"))?,
            event_type: "unprojected".to_owned(),
            data: json!({"not_a_message":true}),
        },
        SessionEvent::AssistantMessage {
            base: base("assistant", Some("metadata"))?,
            response_items: Vec::new(),
            content: "Inspect the assigned file".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: "custom-call\nidentity".to_owned(),
                name: "read".to_owned(),
                arguments: json!("selected/path"),
                kind: ToolCallKind::Custom,
                caller: ToolCallCaller::Absent,
            }],
            usage: EventUsage::default(),
            stop_reason: "tool_use".to_owned(),
            response_id: None,
        },
        SessionEvent::ToolResult {
            base: base("result", Some("assistant"))?,
            tool_call_id: "custom-call\nidentity".to_owned(),
            tool_name: "read".to_owned(),
            output: json!({"content":"recorded file bytes"}),
            spool_ref: None,
            duration_ms: 3,
        },
    ])
}

#[test]
fn summary_blocks_keep_original_event_references_across_skipped_metadata() -> TestResult {
    let events = events()?;
    let original = serde_json::to_value(&events)?;
    let transcript = render_transcript(&events);
    let headers: Vec<Value> = transcript
        .lines()
        .filter_map(|line| {
            line.strip_prefix("[event ")
                .and_then(|line| line.strip_suffix(']'))
        })
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(headers.len(), 3, "{transcript}");
    for (header, event) in headers.iter().zip([&events[0], &events[2], &events[3]]) {
        assert_eq!(header["event_id"], json!(event.base().id));
        assert_eq!(header["parent_event_id"], json!(event.base().parent_id));
        assert_eq!(header["occurred_at"], json!(event.base().timestamp));
        assert_eq!(header["projection"], "prompt_view");
    }
    assert_eq!(headers[2]["tool_call_id"], "custom-call\nidentity");
    assert!(!transcript.contains("not_a_message"));
    assert_eq!(serde_json::to_value(&events)?, original);
    Ok(())
}

#[test]
fn legacy_tool_call_reference_is_escaped_and_retrievable() -> TestResult {
    let transcript = render_transcript(&events()?);
    let identity = transcript
        .lines()
        .find_map(|line| line.strip_prefix("[tool call identity] "))
        .ok_or("summary omitted the tool-call identity")?;
    let identity: Value = serde_json::from_str(identity)?;
    assert_eq!(identity["call_id"], "custom-call\nidentity");
    assert_eq!(identity["kind"], "custom");
    assert!(transcript.contains("[tool call] read(selected/path)"));
    Ok(())
}

#[test]
fn sourced_conversion_preserves_normal_custom_and_function_tool_messages() -> TestResult {
    for kind in [ToolCallKind::Custom, ToolCallKind::Function] {
        let mut events = events()?;
        let SessionEvent::AssistantMessage { tool_calls, .. } = &mut events[2] else {
            return Err("fixture has no assistant call".into());
        };
        tool_calls[0].kind = kind;
        events.insert(
            3,
            SessionEvent::Custom {
                base: base("between-call-and-result", Some("assistant"))?,
                event_type: "unprojected".to_owned(),
                data: json!({}),
            },
        );
        let expected = crate::session::conversion::prompt_events_to_messages(&events);
        let mut messages = Vec::new();
        let mut sources = Vec::new();
        crate::session::conversion::visit_prompt_messages(&events, |event, message| {
            sources.push(event.base().id.clone());
            messages.push(message);
        });
        assert_eq!(
            serde_json::to_value(&messages)?,
            serde_json::to_value(&expected)?
        );
        assert_eq!(
            sources,
            [&events[0], &events[2], &events[4]].map(|event| event.base().id.clone())
        );
        assert_eq!(messages[2].tool_call_kind, Some(kind));
        assert_eq!(
            messages[2].tool_call_id.as_deref(),
            Some("custom-call\nidentity")
        );
    }
    Ok(())
}
