//! Replayed tool completions retain exact event time and event order independently of wall time.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::json;

use super::rebuild_action_log;
use crate::session::EventStore;
use crate::session::action_log::ActionLog;
use crate::session::events::{EventBase, SessionEvent};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn replay_preserves_nanoseconds_equal_and_backward_timestamps_in_event_order() -> TestResult {
    let later =
        DateTime::parse_from_rfc3339("2026-09-01T12:34:56.123456789+10:00")?.with_timezone(&Utc);
    let earlier =
        DateTime::parse_from_rfc3339("2026-08-30T01:02:03.987654321Z")?.with_timezone(&Utc);
    let expected = [
        ("read-first", later),
        ("read-tied", later),
        ("read-backward", earlier),
    ];
    let events = expected
        .iter()
        .map(|(id, timestamp)| SessionEvent::ToolResult {
            base: EventBase {
                timestamp: *timestamp,
                ..EventBase::new(None)
            },
            tool_call_id: (*id).to_owned(),
            tool_name: "read".to_owned(),
            output: json!({"content": format!("result for {id}")}),
            spool_ref: None,
            duration_ms: 23,
        })
        .collect::<Vec<_>>();
    let persisted = serde_json::to_vec(&events)?;
    let reloaded: Vec<SessionEvent> = serde_json::from_slice(&persisted)?;
    let store = Arc::new(EventStore::new());
    for event in &reloaded {
        store.append(event.clone())?;
    }
    let log = ActionLog::new(Arc::clone(&store));
    for _ in 0..2 {
        rebuild_action_log(&log, &reloaded);
        let entries = log.entries();
        assert_eq!(entries.len(), expected.len());
        for (entry, (id, timestamp)) in entries.iter().zip(&expected) {
            assert_eq!(&entry.tool_call_id, id);
            assert_eq!(entry.timestamp, *timestamp);
            let detail = log.get_detail(id).ok_or("restored detail missing")?;
            assert_eq!(detail.entry.timestamp, *timestamp);
            assert_eq!(detail.duration_ms, 23);
            assert_eq!(detail.output, json!({"content":format!("result for {id}")}));
        }
    }
    let second = ActionLog::new(Arc::clone(&store));
    rebuild_action_log(&second, &reloaded);
    assert_eq!(
        serde_json::to_value(second.entries())?,
        serde_json::to_value(log.entries())?
    );
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        persisted,
        "rebuild cannot write to the event store"
    );
    Ok(())
}
