//! Provider-epoch boundary for non-identity forks.

use crate::session::events::{EventBase, ProviderEpochBoundaryReason, SessionEvent};

/// Marker that prevents provider-owned state from crossing a non-identity fork.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderFilteredForkBoundary;

impl ProviderFilteredForkBoundary {
    /// Wrap the marker in its first-class provider-epoch event.
    #[must_use]
    pub(crate) fn into_event(base: EventBase) -> SessionEvent {
        SessionEvent::ProviderEpochBoundary {
            base,
            reason: ProviderEpochBoundaryReason::FilteredFork,
        }
    }

    /// Whether this event is a filtered-fork provider boundary.
    #[must_use]
    pub fn is_family(event: &SessionEvent) -> bool {
        matches!(
            event,
            SessionEvent::ProviderEpochBoundary {
                reason: ProviderEpochBoundaryReason::FilteredFork,
                ..
            }
        )
    }

    /// Parse this exact boundary family and ignore every other event.
    #[must_use]
    pub fn from_event(event: &SessionEvent) -> Option<Self> {
        Self::is_family(event).then_some(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_as_a_first_class_epoch_boundary() -> Result<(), serde_json::Error> {
        let event = ProviderFilteredForkBoundary::into_event(EventBase::new(None));
        let encoded = serde_json::to_value(&event)?;
        assert_eq!(encoded["type"], "ProviderEpochBoundary");
        assert_eq!(encoded["reason"], "filtered_fork");
        assert_eq!(
            ProviderFilteredForkBoundary::from_event(&event),
            Some(ProviderFilteredForkBoundary)
        );
        Ok(())
    }

    #[test]
    fn ignores_application_custom_events_with_the_legacy_discriminator() {
        let event = SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: "provider.epoch.filtered_fork".to_owned(),
            data: serde_json::json!({"version": 1}),
        };
        assert_eq!(ProviderFilteredForkBoundary::from_event(&event), None);
    }
}
