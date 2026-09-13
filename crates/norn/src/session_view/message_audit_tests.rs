//! Transport audit rows remain inspectable metadata without creating transcript clutter.

use super::body::resolve_committed_body;
use super::contract_tests::{TestResult, cursor, source};
use super::{DisplayField, ViewItemKind, project_committed};
use crate::session::events::{EventBase, SessionEvent};
use serde_json::{Value, json};
use uuid::Uuid;

fn audits() -> Vec<(&'static str, Value)> {
    let message = Uuid::new_v4();
    let from = Uuid::new_v4();
    let to = Uuid::new_v4();
    let now = chrono::Utc::now();
    vec![
        (
            "agent_message.queued",
            json!({"phase":"queued", "message_id":message, "from_id":from,"from":"worker", "role":null,"to_id":to,"to":"parent","kind":"update","seq":7,"content":"actual message","queued_at":now}),
        ),
        (
            "agent_message.dequeued",
            json!({"phase":"dequeued","message_id":message,"to_id":to,"dequeued_at":now}),
        ),
        (
            "agent_message.sent",
            json!({"phase":"sent","message_id":message,"from_id":from,"from":"worker","to_id":to,"to":"parent","kind":"update","seq":7,"content":"actual message","sent_at":now}),
        ),
        (
            "agent_message.delivered",
            json!({"phase":"delivered","message_id":message,"from_id":from,"from":"worker","to_id":to,"seq":7,"delivered_at":now}),
        ),
    ]
}

#[test]
fn copied_audits_are_metadata_with_typed_original_details() -> TestResult {
    let owner = source();
    for (ordinal, (name, data)) in audits().into_iter().enumerate() {
        let event = SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: name.to_owned(),
            data: data.clone(),
        };
        let before = serde_json::to_value(&event)?;
        let record = project_committed(&cursor(&owner, ordinal, &event), &event)?;
        let [item] = record.items() else {
            return Err("expected one audit row".into());
        };
        assert!(matches!(item.kind, ViewItemKind::Metadata), "{name}");
        assert_eq!(item.bodies.len(), 1);
        let body = resolve_committed_body(&event, &DisplayField::CustomLifecycle)?;
        let decoded: Value = serde_json::from_str(body.as_ref())?;
        for (key, expected) in data.as_object().ok_or("fixture is not object")? {
            assert_eq!(&decoded[key], expected, "{name}.{key}");
        }
        assert_eq!(serde_json::to_value(&event)?, before);
    }
    Ok(())
}

#[test]
fn malformed_or_wrong_phase_message_audits_remain_visible_failures() -> TestResult {
    let owner = source();
    for (name, mut data) in audits() {
        data["phase"] = json!("invalid-private-marker");
        let event = SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: name.to_owned(),
            data,
        };
        let record = project_committed(&cursor(&owner, 0, &event), &event)?;
        assert!(matches!(record.items()[0].kind, ViewItemKind::Unavailable));
        let error = resolve_committed_body(&event, &DisplayField::CustomLifecycle)
            .err()
            .ok_or("malformed audit accepted")?;
        assert!(!error.to_string().contains("invalid-private-marker"));
    }
    Ok(())
}
