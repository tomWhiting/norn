use serde_json::json;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn round_trips_legacy_v1_as_readable_but_unbound() -> TestResult {
    for stored in [false, true] {
        let assistant_event_id = EventId::new();
        let event = ProviderStateProvenance::new(assistant_event_id.clone(), stored)
            .into_custom_event(EventBase::new(None))?;

        assert!(matches!(
            &event,
            SessionEvent::Custom { event_type, .. }
                if event_type == PROVIDER_STATE_PROVENANCE_EVENT_TYPE
        ));
        let encoded = serde_json::to_value(&event)?;
        assert_eq!(encoded["data"]["version"], json!(1));
        assert!(encoded["data"].get("prompt_seed_sha256").is_none());
        let Some(decoded) = ProviderStateProvenance::from_event(&event)? else {
            return Err(std::io::Error::other("provenance family was not recognized").into());
        };
        assert_eq!(decoded.assistant_event_id(), &assistant_event_id);
        assert_eq!(decoded.stored(), stored);
        assert!(decoded.prompt_seed_fingerprint().is_none());
    }
    Ok(())
}

#[test]
fn round_trips_v2_with_an_exact_prompt_seed() -> TestResult {
    let assistant_event_id = EventId::new();
    let prompt_seed = PromptSeedFingerprint::empty();
    let event =
        ProviderStateProvenance::with_prompt_seed(assistant_event_id.clone(), true, prompt_seed)
            .into_custom_event(EventBase::new(None))?;

    let encoded = serde_json::to_value(&event)?;
    assert_eq!(encoded["data"]["version"], json!(2));
    let Some(encoded_seed) = encoded["data"]["prompt_seed_sha256"].as_str() else {
        return Err(std::io::Error::other("V2 prompt seed did not serialize as text").into());
    };
    let Some(decoded) = ProviderStateProvenance::from_event(&event)? else {
        return Err(std::io::Error::other("provenance family was not recognized").into());
    };
    assert_eq!(decoded.assistant_event_id(), &assistant_event_id);
    assert_eq!(decoded.prompt_seed_fingerprint(), Some(prompt_seed));
    let debug = format!("{decoded:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains(encoded_seed));
    Ok(())
}

#[test]
fn rejects_version_seed_mismatches_and_extended_payloads() -> TestResult {
    let seed = serde_json::to_value(PromptSeedFingerprint::empty())?;
    for data in [
        json!({
            "version": 2,
            "assistant_event_id": EventId::new(),
            "stored": true,
        }),
        json!({
            "version": 1,
            "assistant_event_id": EventId::new(),
            "stored": true,
            "prompt_seed_sha256": seed,
        }),
        json!({
            "version": 3,
            "assistant_event_id": EventId::new(),
            "stored": true,
            "prompt_seed_sha256": seed,
        }),
        json!({
            "version": 1,
            "assistant_event_id": EventId::new(),
            "stored": true,
            "future": true,
        }),
        json!({"version": 1, "assistant_event_id": EventId::new()}),
    ] {
        let event = SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: PROVIDER_STATE_PROVENANCE_EVENT_TYPE.to_owned(),
            data,
        };
        assert!(matches!(
            ProviderStateProvenance::from_event(&event),
            Err(ProviderStateProvenanceError::InvalidPayload { .. })
        ));
    }
    Ok(())
}

#[test]
fn ignores_unrelated_custom_events() -> TestResult {
    let event = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "application.note".to_owned(),
        data: json!({"version": 1}),
    };
    assert!(ProviderStateProvenance::from_event(&event)?.is_none());
    Ok(())
}
