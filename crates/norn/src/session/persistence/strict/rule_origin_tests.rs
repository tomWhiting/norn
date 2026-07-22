use std::io::Cursor;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::rules::source::RuleOrigin;
use crate::rules::types::{DeliveryMode, TriggerTiming};
use crate::session::events::{EventBase, SessionEvent};

use super::{StrictFormatHeader, StrictStoreError, read_strict_event_file};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rule_event(origin: Option<RuleOrigin>) -> SessionEvent {
    SessionEvent::RuleInjection {
        base: EventBase::new(None),
        rule_id: "authority".to_owned(),
        origin,
        delivery: DeliveryMode::SystemContextAppend,
        timing: TriggerTiming::After,
        content: "body".to_owned(),
    }
}

fn strict_file<T: Serialize>(row: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(&StrictFormatHeader::current())?;
    bytes.push(b'\n');
    serde_json::to_writer(&mut bytes, row)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[test]
fn strict_reader_accepts_new_and_legacy_rule_origins() -> TestResult {
    for expected in [
        Some(RuleOrigin::Operator),
        Some(RuleOrigin::Workspace),
        None,
    ] {
        let bytes = strict_file(&rule_event(expected))?;
        let timeline = read_strict_event_file(Cursor::new(bytes), Path::new("rule-origin.jsonl"))?;
        let [SessionEvent::RuleInjection { origin, .. }] = timeline.events.as_slice() else {
            return Err(std::io::Error::other("missing rule injection").into());
        };
        assert_eq!(*origin, expected);
    }
    Ok(())
}

#[test]
fn strict_reader_rejects_unknown_rule_origin() -> TestResult {
    let mut value = serde_json::to_value(rule_event(Some(RuleOrigin::Operator)))?;
    let object = value
        .as_object_mut()
        .ok_or("serialized rule injection was not an object")?;
    object.insert("origin".to_owned(), Value::String("repository".to_owned()));
    let result = read_strict_event_file(
        Cursor::new(strict_file(&value)?),
        Path::new("unknown-rule-origin.jsonl"),
    );
    assert!(matches!(
        result,
        Err(StrictStoreError::InvalidJson { line: 2, .. })
    ));
    Ok(())
}

#[test]
fn strict_reader_rejects_unknown_rule_field() -> TestResult {
    let mut value = serde_json::to_value(rule_event(Some(RuleOrigin::Workspace)))?;
    let object = value
        .as_object_mut()
        .ok_or("serialized rule injection was not an object")?;
    object.insert("authority".to_owned(), Value::String("system".to_owned()));
    let result = read_strict_event_file(
        Cursor::new(strict_file(&value)?),
        Path::new("unknown-rule-field.jsonl"),
    );
    assert!(matches!(
        result,
        Err(StrictStoreError::UnknownField { line: 2, .. })
    ));
    Ok(())
}
