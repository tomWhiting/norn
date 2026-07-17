use std::error::Error;
use std::io;
use std::sync::Arc;

use norn::provider::openai::response_contract::{
    CODEX_OVERLAY, CodexOverlayKind, PUBLIC_STREAM_EVENTS, StreamEventStage,
};
use norn::provider::openai::response_stream_event::{
    ResponseStreamEvent, ResponseStreamEventManifest,
};
use norn::provider::response_audio::ResponseAudioEvent;
use norn::provider::{AgentEvent, AgentEventKind};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{agent_event_method, agent_event_to_ndjson, agent_event_to_value};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn raw_agent_event(raw: &Value) -> TestResult<AgentEvent> {
    let event = ResponseStreamEvent::from_raw(raw.clone())?;
    Ok(AgentEvent {
        agent_id: Uuid::nil(),
        agent_role: Arc::from("root"),
        event: AgentEventKind::Provider(
            norn::provider::events::ProviderEvent::ResponseStreamEvent {
                event: Box::new(event),
            },
        ),
    })
}

fn missing_wire_value(context: &'static str) -> io::Error {
    io::Error::other(context)
}

fn assert_exact_raw_output(agent_event: &AgentEvent, raw: &Value, partial: bool) -> TestResult {
    assert_eq!(agent_event_method(agent_event), "event/raw");
    let value = agent_event_to_value(agent_event, partial)
        .ok_or_else(|| missing_wire_value("raw stream event was unexpectedly filtered"))?;
    assert_eq!(&value, raw);

    let ndjson = agent_event_to_ndjson(agent_event, partial)
        .ok_or_else(|| missing_wire_value("raw stream event had no NDJSON representation"))?;
    assert_eq!(ndjson, raw.to_string());
    let decoded: Value = serde_json::from_str(&ndjson)?;
    assert_eq!(&decoded, raw);
    Ok(())
}

#[test]
fn all_public_stream_events_are_exact_raw_events_with_stage_aware_filtering() -> TestResult {
    assert_eq!(PUBLIC_STREAM_EVENTS.len(), 53);
    let mut observed_stages = [false; 4];

    for (ordinal, entry) in PUBLIC_STREAM_EVENTS.iter().enumerate() {
        let sequence_number = u64::try_from(ordinal)?;
        let raw = json!({
            "type": entry.name(),
            "sequence_number": sequence_number,
            "contract_probe": {
                "ordinal": sequence_number,
                "event_type": entry.name()
            }
        });
        let agent_event = raw_agent_event(&raw)?;

        assert_exact_raw_output(&agent_event, &raw, true)?;
        match entry.stage() {
            StreamEventStage::Lifecycle => {
                observed_stages[0] = true;
                assert_exact_raw_output(&agent_event, &raw, false)?;
            }
            StreamEventStage::Incremental => {
                observed_stages[1] = true;
                assert!(agent_event_to_value(&agent_event, false).is_none());
                assert!(agent_event_to_ndjson(&agent_event, false).is_none());
            }
            StreamEventStage::Completed => {
                observed_stages[2] = true;
                assert_exact_raw_output(&agent_event, &raw, false)?;
            }
            StreamEventStage::Terminal => {
                observed_stages[3] = true;
                assert_exact_raw_output(&agent_event, &raw, false)?;
            }
        }
    }

    assert!(
        observed_stages.into_iter().all(std::convert::identity),
        "the pinned public registry must exercise every filtering stage"
    );
    Ok(())
}

#[test]
fn codex_overlays_and_future_events_remain_exact_raw_events_without_partial_mode() -> TestResult {
    let mut overlay_count = 0;
    for entry in CODEX_OVERLAY
        .iter()
        .filter(|entry| entry.kind() == CodexOverlayKind::StreamEvent)
    {
        let raw = json!({
            "type": entry.name(),
            "codex_probe": {"event_type": entry.name()}
        });
        let agent_event = raw_agent_event(&raw)?;
        assert_exact_raw_output(&agent_event, &raw, false)?;
        assert_exact_raw_output(&agent_event, &raw, true)?;
        overlay_count += 1;
    }
    assert_eq!(overlay_count, 2, "both Codex stream overlays are covered");

    let unknown_raw = json!({
        "type": "response.future.contract_probe",
        "sequence_number": 9_999,
        "future_payload": {"retained": true, "values": [1, 2, 3]}
    });
    let unknown_event = ResponseStreamEvent::from_raw(unknown_raw.clone())?;
    assert_eq!(
        unknown_event.manifest(),
        ResponseStreamEventManifest::Unknown
    );
    let agent_event = raw_agent_event(&unknown_raw)?;
    assert_exact_raw_output(&agent_event, &unknown_raw, false)?;
    assert_exact_raw_output(&agent_event, &unknown_raw, true)?;
    Ok(())
}

#[test]
fn actionable_audio_projection_does_not_duplicate_the_raw_wire_event() -> TestResult {
    let raw = json!({
        "type": "response.audio.delta",
        "sequence_number": 7,
        "delta": "YXVkaW8="
    });
    let stream_event = ResponseStreamEvent::from_raw(raw.clone())?;
    let event = ResponseAudioEvent::from_stream_event(&stream_event)?
        .ok_or_else(|| missing_wire_value("audio event had no typed projection"))?;
    let actionable = AgentEvent {
        agent_id: Uuid::nil(),
        agent_role: Arc::from("root"),
        event: AgentEventKind::Provider(
            norn::provider::events::ProviderEvent::ResponseAudioFrame {
                stream_event: Box::new(stream_event),
                event,
            },
        ),
    };

    assert_eq!(agent_event_method(&actionable), "event/raw");
    assert!(agent_event_to_value(&actionable, true).is_none());
    assert!(agent_event_to_ndjson(&actionable, true).is_none());
    assert_exact_raw_output(&raw_agent_event(&raw)?, &raw, true)?;
    Ok(())
}
