use super::events::{EventBase, ProviderEpochBoundaryReason, SessionEvent};

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
