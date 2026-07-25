//! Classification tests for crash-torn publication tails.

use super::{PUBLICATION_TAIL_RECOVERY_EVENT_TYPE, PublicationTailRecovery, torn_publication_tail};
use crate::session::events::{EventBase, EventId, EventUsage, SessionEvent};
use crate::session::provider_state_provenance::ProviderStateProvenance;
use crate::session::provider_state_validation::seal_response_publication_group;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type EventsResult = Result<Vec<SessionEvent>, Box<dyn std::error::Error>>;

fn sealed_group(parent: Option<EventId>, label: &str) -> EventsResult {
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(parent),
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let provenance_base = EventBase::new(Some(boundary.base().id.clone()));
    let assistant_base = EventBase::new(Some(provenance_base.id.clone()));
    let provenance = ProviderStateProvenance::new(assistant_base.id.clone(), true)
        .into_custom_event(provenance_base)?;
    let assistant = SessionEvent::AssistantMessage {
        base: assistant_base,
        response_items: Vec::new(),
        content: label.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(format!("resp-{label}")),
    };
    let mut group = vec![boundary, provenance, assistant];
    seal_response_publication_group(&mut group)?;
    Ok(group)
}

/// Build one sealed four-row group carrying a response-audio link.
fn sealed_audio_group(parent: Option<EventId>, label: &str) -> EventsResult {
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(parent),
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let provenance_base = EventBase::new(Some(boundary.base().id.clone()));
    let link_base = EventBase::new(Some(provenance_base.id.clone()));
    let assistant_base = EventBase::new(Some(link_base.id.clone()));
    let provenance = ProviderStateProvenance::new(assistant_base.id.clone(), true)
        .into_custom_event(provenance_base)?;
    let reference = serde_json::from_value(serde_json::json!(
        uuid::Uuid::new_v4().hyphenated().to_string()
    ))?;
    let link = crate::session::ResponseAudioArtifactLink::new(
        assistant_base.id.clone(),
        reference,
        Some(format!("resp-{label}")),
    )
    .into_custom_event(link_base)?;
    let assistant = SessionEvent::AssistantMessage {
        base: assistant_base,
        response_items: Vec::new(),
        content: label.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(format!("resp-{label}")),
    };
    let mut group = vec![boundary, provenance, link, assistant];
    seal_response_publication_group(&mut group)?;
    Ok(group)
}

fn user(parent: Option<EventId>) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(parent),
        content: "hello".to_owned(),
    }
}

/// `[user, complete group]` followed by `rows` of a second group.
fn torn_timeline(rows: usize) -> EventsResult {
    let first = user(None);
    let mut events = vec![first.clone()];
    let group = sealed_group(Some(first.base().id.clone()), "healthy")?;
    let last = group.last().ok_or("short group")?.base().id.clone();
    events.extend(group);
    let torn = sealed_group(Some(last), "torn")?;
    events.extend(torn.get(..rows).ok_or("short group")?.iter().cloned());
    Ok(events)
}

#[test]
fn a_healthy_timeline_is_never_classified_as_torn() -> TestResult {
    let events = torn_timeline(3)?;
    assert_eq!(torn_publication_tail(&events), None);
    Ok(())
}

#[test]
fn an_empty_timeline_is_never_classified_as_torn() {
    assert_eq!(torn_publication_tail(&[]), None);
}

#[test]
fn every_strict_group_prefix_is_classified_as_torn() -> TestResult {
    for rows in 1..=2_usize {
        let events = torn_timeline(rows)?;
        let torn = torn_publication_tail(&events).ok_or("expected a torn tail")?;
        assert_eq!(torn.boundary_index, events.len() - rows);
        assert_eq!(torn.quarantined_event_ids.len(), rows);
        assert_eq!(torn.orphaned_assistant_event_id.is_some(), rows >= 2);
    }
    Ok(())
}

#[test]
fn a_tail_longer_than_a_group_is_not_torn() -> TestResult {
    let mut events = torn_timeline(2)?;
    let last = events.last().ok_or("empty")?.base().id.clone();
    events.push(user(Some(last.clone())));
    events.push(user(Some(last.clone())));
    events.push(user(Some(last)));
    assert_eq!(torn_publication_tail(&events), None);
    Ok(())
}

/// Rows appended *after* a group prefix are ordinary history, never
/// quarantine fodder: a tail longer than a group can never be a torn group,
/// even when its first rows do look like one.
#[test]
fn rows_beyond_a_group_prefix_are_never_quarantined() -> TestResult {
    let first = user(None);
    let mut events = vec![first.clone()];
    let torn = sealed_audio_group(Some(first.base().id.clone()), "torn")?;
    events.extend(torn.get(..3).ok_or("short group")?.iter().cloned());
    assert!(
        torn_publication_tail(&events).is_some(),
        "the bare three-row prefix is torn",
    );

    let last = events.last().ok_or("empty")?.base().id.clone();
    events.push(user(Some(last)));
    assert_eq!(
        torn_publication_tail(&events),
        None,
        "one more row makes the tail longer than any group, so nothing is torn",
    );
    Ok(())
}

#[test]
fn a_broken_prefix_blocks_tail_recovery() -> TestResult {
    let mut events = torn_timeline(2)?;
    let last = events.last().ok_or("empty")?.base().id.clone();
    let second = sealed_group(Some(last), "second")?;
    events.extend(second.get(..2).ok_or("short group")?.iter().cloned());
    assert_eq!(
        torn_publication_tail(&events),
        None,
        "the older tear is in the interior, so nothing is recoverable",
    );
    Ok(())
}

#[test]
fn a_marker_with_a_foreign_parent_is_not_torn() -> TestResult {
    let mut events = torn_timeline(1)?;
    events.push(
        ProviderStateProvenance::new(EventId::new(), true)
            .into_custom_event(EventBase::new(Some(EventId::new())))?,
    );
    assert_eq!(torn_publication_tail(&events), None);
    Ok(())
}

#[test]
fn a_recovery_record_round_trips_through_its_custom_event() -> TestResult {
    let quarantined = vec![EventId::new(), EventId::new()];
    let orphan = EventId::new();
    let record = PublicationTailRecovery::new(
        "session.jsonl.torn-tail-x.quarantine".to_owned(),
        quarantined.clone(),
        742,
        Some(orphan.clone()),
    )?;
    let event = record.clone().into_custom_event(EventBase::new(None))?;
    let SessionEvent::Custom { event_type, .. } = &event else {
        return Err("expected a custom event".into());
    };
    assert_eq!(event_type, PUBLICATION_TAIL_RECOVERY_EVENT_TYPE);

    let decoded = PublicationTailRecovery::from_event(&event)?.ok_or("expected a record")?;
    assert_eq!(decoded, record);
    assert_eq!(decoded.quarantined_event_ids(), quarantined.as_slice());
    assert_eq!(decoded.quarantined_bytes(), 742);
    assert_eq!(decoded.orphaned_assistant_event_id(), Some(&orphan));

    // The row survives a byte-level round trip, which is what the strict
    // reader demands of every persisted row.
    let bytes = serde_json::to_vec(&event)?;
    let reread: SessionEvent = serde_json::from_slice(&bytes)?;
    assert_eq!(
        serde_json::to_value(&reread)?,
        serde_json::to_value(&event)?
    );
    Ok(())
}

#[test]
fn an_empty_recovery_record_is_refused() {
    assert!(
        PublicationTailRecovery::new("q".to_owned(), Vec::new(), 1, None).is_err(),
        "a record naming no rows is refused",
    );
    assert!(
        PublicationTailRecovery::new(String::new(), vec![EventId::new()], 1, None).is_err(),
        "a record naming no quarantine file is refused",
    );
    assert!(
        PublicationTailRecovery::new("q".to_owned(), vec![EventId::new()], 0, None).is_err(),
        "a record recording no bytes is refused",
    );
}

#[test]
fn an_unknown_recovery_payload_version_is_refused() {
    let event = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: PUBLICATION_TAIL_RECOVERY_EVENT_TYPE.to_owned(),
        data: serde_json::json!({
            "version": 2,
            "quarantine_file": "q",
            "quarantined_event_ids": ["a"],
            "quarantined_bytes": 1,
        }),
    };
    assert!(PublicationTailRecovery::from_event(&event).is_err());
}

#[test]
fn an_unknown_recovery_payload_field_is_refused() {
    let event = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: PUBLICATION_TAIL_RECOVERY_EVENT_TYPE.to_owned(),
        data: serde_json::json!({
            "version": 1,
            "quarantine_file": "q",
            "quarantined_event_ids": ["a"],
            "quarantined_bytes": 1,
            "surprise": true,
        }),
    };
    assert!(PublicationTailRecovery::from_event(&event).is_err());
}

#[test]
fn other_custom_families_are_ignored() -> TestResult {
    let event = ProviderStateProvenance::new(EventId::new(), true)
        .into_custom_event(EventBase::new(None))?;
    assert!(PublicationTailRecovery::from_event(&event)?.is_none());
    assert!(PublicationTailRecovery::from_event(&user(None))?.is_none());
    Ok(())
}
