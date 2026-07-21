use super::ProviderFilteredForkBoundary;
use super::events::{EventBase, EventUsage, ProviderEpochBoundaryReason, SessionEvent};

#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum FrozenPreD3SessionEvent {
    ProviderEpochBoundary {
        base: EventBase,
        reason: FrozenPreD3BoundaryReason,
    },
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum FrozenPreD3BoundaryReason {
    MigratedLegacy,
    ProviderIdentityAdoption,
}

#[test]
fn provider_epoch_boundary_serde_is_exact_and_typed() -> Result<(), serde_json::Error> {
    let event = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: ProviderEpochBoundaryReason::MigratedLegacy,
    };

    let value = serde_json::to_value(&event)?;
    assert_eq!(value["type"], "ProviderEpochBoundary");
    assert_eq!(value["reason"], "migrated_legacy");
    let decoded: SessionEvent = serde_json::from_value(value)?;
    assert!(matches!(
        decoded,
        SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::MigratedLegacy,
            ..
        }
    ));
    Ok(())
}

#[test]
fn provider_identity_adoption_boundary_has_a_distinct_durable_reason()
-> Result<(), serde_json::Error> {
    let event = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
    };

    let value = serde_json::to_value(&event)?;
    assert_eq!(value["reason"], "provider_identity_adoption");
    let decoded: SessionEvent = serde_json::from_value(value)?;
    assert!(matches!(
        decoded,
        SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
            ..
        }
    ));
    Ok(())
}

#[test]
fn filtered_fork_boundary_has_a_distinct_first_class_reason() -> Result<(), serde_json::Error> {
    let event = ProviderFilteredForkBoundary::into_event(EventBase::new(None));

    let value = serde_json::to_value(&event)?;
    assert_eq!(value["type"], "ProviderEpochBoundary");
    assert_eq!(value["reason"], "filtered_fork");
    let decoded: SessionEvent = serde_json::from_value(value)?;
    assert_eq!(
        ProviderFilteredForkBoundary::from_event(&decoded),
        Some(ProviderFilteredForkBoundary)
    );
    Ok(())
}

#[test]
fn pre_d3_reader_fails_closed_on_new_boundary_reasons() -> Result<(), Box<dyn std::error::Error>> {
    let legacy = serde_json::to_value(SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: ProviderEpochBoundaryReason::MigratedLegacy,
    })?;
    let FrozenPreD3SessionEvent::ProviderEpochBoundary { base, reason } =
        serde_json::from_value(legacy)?;
    assert!(!base.id.as_str().is_empty());
    assert!(matches!(reason, FrozenPreD3BoundaryReason::MigratedLegacy));

    for reason in [
        ProviderEpochBoundaryReason::ResponseStatePublication,
        ProviderEpochBoundaryReason::FilteredFork,
    ] {
        let event = SessionEvent::ProviderEpochBoundary {
            base: EventBase::new(None),
            reason,
        };
        let encoded = serde_json::to_value(event)?;
        assert!(serde_json::from_value::<FrozenPreD3SessionEvent>(encoded).is_err());
    }

    let fixture = super::response_publication_fixture(None, true)?;
    let assistant = SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: fixture.assistant_base,
        content: "committed response".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_committed".to_owned()),
    };
    let committed =
        super::committed_response_publication(fixture.boundary, fixture.provenance, assistant)?;
    let committed_boundary = committed
        .first()
        .ok_or_else(|| std::io::Error::other("committed fixture omitted its boundary"))?;
    assert!(
        serde_json::from_value::<FrozenPreD3SessionEvent>(serde_json::to_value(
            committed_boundary,
        )?)
        .is_err()
    );
    Ok(())
}
