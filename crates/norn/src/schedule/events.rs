//! Custom session-event payloads for schedule lifecycle, and the append
//! helper that persists them.
//!
//! Schedule lifecycle is durable exactly like pending inter-agent messages
//! (`crate::agent::pending_messages`): stable `event_type` constants, a typed
//! payload enum, and a `from_events` rebuild in [`super::store`]. Three
//! phases persist — `schedule.created` (the full [`ScheduleRecord`]),
//! `schedule.fired` (`{ id, fired_at, late }`), and `schedule.cancelled`
//! (`{ id, cancelled_at }`). Ordering is content-first: a `schedule.created`
//! is appended before the executor arms the entry, and a `schedule.fired` is
//! appended only after the injected delivery (or durable queue) succeeded, so
//! a crash can at worst replay a fire notice, never lose an accepted
//! schedule.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::SessionError;
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

use super::entry::ScheduleRecord;

/// `event_type` for a newly created, armed schedule.
pub const SCHEDULE_CREATED_EVENT_TYPE: &str = "schedule.created";

/// `event_type` for a schedule that fired and delivered its message.
pub const SCHEDULE_FIRED_EVENT_TYPE: &str = "schedule.fired";

/// `event_type` for a schedule the owning agent cancelled.
pub const SCHEDULE_CANCELLED_EVENT_TYPE: &str = "schedule.cancelled";

/// The durable lifecycle of one schedule, persisted as
/// [`SessionEvent::Custom`] events on the owning agent's event store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ScheduleLifecycle {
    /// A schedule was created and armed. Carries the full record so a
    /// resume rebuild reconstructs the spec, message, and original
    /// `next_fire`.
    Created {
        /// The created schedule record.
        record: ScheduleRecord,
    },
    /// A schedule fired and its message was delivered (live or durably
    /// queued).
    Fired {
        /// The schedule that fired.
        id: Uuid,
        /// Wall-clock instant of the fire.
        fired_at: DateTime<Utc>,
        /// Whether this fire was a past-due catch-up (a resume-restored
        /// one-shot).
        late: bool,
    },
    /// A schedule was cancelled by the owning agent.
    Cancelled {
        /// The cancelled schedule.
        id: Uuid,
        /// Wall-clock instant of the cancellation.
        cancelled_at: DateTime<Utc>,
    },
}

impl ScheduleLifecycle {
    /// The schedule id this event concerns.
    #[must_use]
    pub fn schedule_id(&self) -> Uuid {
        match self {
            Self::Created { record } => record.id,
            Self::Fired { id, .. } | Self::Cancelled { id, .. } => *id,
        }
    }

    /// The session-store `event_type` for this phase.
    #[must_use]
    pub const fn session_event_type(&self) -> &'static str {
        match self {
            Self::Created { .. } => SCHEDULE_CREATED_EVENT_TYPE,
            Self::Fired { .. } => SCHEDULE_FIRED_EVENT_TYPE,
            Self::Cancelled { .. } => SCHEDULE_CANCELLED_EVENT_TYPE,
        }
    }
}

/// Append a schedule lifecycle event to `store`.
///
/// The append rides [`crate::r#loop::append_off_executor`] so the sink I/O
/// stays off the executor thread, exactly like the pending-message audit
/// path it mirrors.
///
/// # Errors
///
/// Returns [`SessionError::EventAppendFailed`] if the payload cannot be
/// serialized, or any [`SessionError`] propagated by [`EventStore::append`].
pub fn append_schedule_event(
    store: &EventStore,
    event: &ScheduleLifecycle,
) -> Result<EventId, SessionError> {
    let event_type = event.session_event_type();
    let data = serde_json::to_value(event).map_err(|error| SessionError::EventAppendFailed {
        reason: format!(
            "failed to serialize {event_type} audit for schedule {}: {error}",
            event.schedule_id()
        ),
    })?;
    crate::r#loop::append_off_executor(
        store,
        SessionEvent::Custom {
            base: EventBase::new(store.last_event_id()),
            event_type: event_type.to_owned(),
            data,
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::schedule::entry::ScheduleSpec;

    fn sample_record() -> ScheduleRecord {
        ScheduleRecord::new(
            Uuid::new_v4(),
            ScheduleSpec::In {
                duration: Duration::from_mins(1),
            },
            "ping".to_string(),
            Uuid::new_v4(),
            Utc::now(),
        )
        .unwrap()
    }

    #[test]
    fn event_types_match_phase() {
        let record = sample_record();
        let created = ScheduleLifecycle::Created {
            record: record.clone(),
        };
        let fired = ScheduleLifecycle::Fired {
            id: record.id,
            fired_at: Utc::now(),
            late: true,
        };
        let cancelled = ScheduleLifecycle::Cancelled {
            id: record.id,
            cancelled_at: Utc::now(),
        };
        assert_eq!(created.session_event_type(), SCHEDULE_CREATED_EVENT_TYPE);
        assert_eq!(fired.session_event_type(), SCHEDULE_FIRED_EVENT_TYPE);
        assert_eq!(
            cancelled.session_event_type(),
            SCHEDULE_CANCELLED_EVENT_TYPE
        );
        assert_eq!(created.schedule_id(), record.id);
        assert_eq!(fired.schedule_id(), record.id);
        assert_eq!(cancelled.schedule_id(), record.id);
    }

    #[test]
    fn append_persists_custom_event_with_stable_type() {
        let store = EventStore::new();
        let record = sample_record();
        append_schedule_event(
            &store,
            &ScheduleLifecycle::Created {
                record: record.clone(),
            },
        )
        .unwrap();
        let events = store.events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            SessionEvent::Custom {
                event_type, data, ..
            } => {
                assert_eq!(event_type, SCHEDULE_CREATED_EVENT_TYPE);
                let parsed: ScheduleLifecycle = serde_json::from_value(data.clone()).unwrap();
                assert_eq!(parsed.schedule_id(), record.id);
            }
            other => panic!("expected Custom event, got {other:?}"),
        }
    }

    #[test]
    fn lifecycle_serde_roundtrips() {
        let record = sample_record();
        for event in [
            ScheduleLifecycle::Created {
                record: record.clone(),
            },
            ScheduleLifecycle::Fired {
                id: record.id,
                fired_at: Utc::now(),
                late: false,
            },
            ScheduleLifecycle::Cancelled {
                id: record.id,
                cancelled_at: Utc::now(),
            },
        ] {
            let json = serde_json::to_string(&event).unwrap();
            let back: ScheduleLifecycle = serde_json::from_str(&json).unwrap();
            assert_eq!(back, event);
        }
    }
}
