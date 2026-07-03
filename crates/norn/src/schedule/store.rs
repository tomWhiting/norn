//! In-memory schedule store with change notification and event rebuild.
//!
//! One [`ScheduleStore`] belongs to one agent. It holds that agent's pending
//! schedule records behind a mutex and a [`Notify`] the tool operations
//! signal so the live executor re-evaluates the earliest fire time the moment
//! the set changes. The store imposes **no** cap on record count.
//!
//! [`ScheduleStore::from_events`] rebuilds live state from an event slice:
//! created minus cancelled minus fired-out one-shots equals pending. On
//! resume it also re-arms the survivors relative to the resume instant — a
//! past-due one-shot keeps its elapsed fire time (so it fires immediately)
//! and is marked `late`, while a recurring schedule re-arms to its next
//! natural fire after the resume instant with **no** backfill of missed
//! occurrences.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::sync::futures::Notified;
use uuid::Uuid;

use crate::session::events::SessionEvent;

use super::entry::{ScheduleRecord, ScheduleSpec};
use super::events::{
    SCHEDULE_CANCELLED_EVENT_TYPE, SCHEDULE_CREATED_EVENT_TYPE, SCHEDULE_FIRED_EVENT_TYPE,
    ScheduleLifecycle,
};

/// The agent-owned store of pending schedule records.
///
/// Only pending records live here: a one-shot that fires and a schedule that
/// is cancelled are removed. The [`Notify`] wakes the executor whenever the
/// pending set changes.
pub struct ScheduleStore {
    inner: Mutex<HashMap<Uuid, ScheduleRecord>>,
    notify: Notify,
}

impl ScheduleStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            notify: Notify::new(),
        }
    }

    /// Rebuild pending state from a session-event slice and re-arm the
    /// survivors relative to `now` (the resume instant).
    ///
    /// Created records that were neither cancelled nor fired-out survive.
    /// Recurring survivors re-arm to their next natural fire after `now`
    /// (no backfill); one-shot survivors whose fire time already passed keep
    /// that elapsed time (firing immediately on the executor's first pass)
    /// and are marked `late`. A recurring schedule whose expression no
    /// longer computes a next occurrence is dropped with an error log rather
    /// than left un-armable.
    #[must_use]
    pub fn from_events(events: &[SessionEvent], now: DateTime<Utc>) -> Self {
        let mut records: HashMap<Uuid, ScheduleRecord> = HashMap::new();
        for event in events {
            let SessionEvent::Custom {
                event_type, data, ..
            } = event
            else {
                continue;
            };
            match event_type.as_str() {
                SCHEDULE_CREATED_EVENT_TYPE => {
                    match serde_json::from_value::<ScheduleLifecycle>(data.clone()) {
                        Ok(ScheduleLifecycle::Created { record }) => {
                            records.insert(record.id, record);
                        }
                        // A schedule.created-typed event that deserializes to
                        // a non-Created lifecycle phase is a corrupt/mislabeled
                        // payload: it arms nothing, but the mismatch is a data
                        // fault, not a silent skip — warn.
                        Ok(other) => tracing::warn!(
                            phase = other.session_event_type(),
                            "schedule store: schedule.created event carried a \
                             non-Created lifecycle payload; skipping",
                        ),
                        Err(error) => tracing::warn!(
                            %error,
                            "schedule store: invalid schedule.created payload; skipping",
                        ),
                    }
                }
                SCHEDULE_FIRED_EVENT_TYPE => {
                    if let Some(id) = schedule_id_from(data) {
                        // A fired one-shot is done; a fired recurring
                        // schedule stays and re-arms from `now` below.
                        if records
                            .get(&id)
                            .is_some_and(|record| !record.spec.is_recurring())
                        {
                            records.remove(&id);
                        }
                    } else {
                        // A corrupt schedule.fired payload carries no usable
                        // id, so the matching one-shot cannot be retired: it
                        // survives the rebuild and may fire again on resume.
                        // Inherent to the corruption — the warn is the honest
                        // signal, not a silent skip.
                        tracing::warn!(
                            "schedule store: unparseable schedule.fired payload (no valid \
                             id); cannot retire the matching one-shot, which may re-fire \
                             on resume",
                        );
                    }
                }
                SCHEDULE_CANCELLED_EVENT_TYPE => {
                    if let Some(id) = schedule_id_from(data) {
                        records.remove(&id);
                    } else {
                        // A corrupt schedule.cancelled payload carries no
                        // usable id, so the intended cancellation cannot be
                        // applied: the target schedule silently resurrects on
                        // resume. Inherent to the corruption (no recovery is
                        // invented) — the warn is the honest signal.
                        tracing::warn!(
                            "schedule store: unparseable schedule.cancelled payload (no \
                             valid id); the intended cancellation is NOT applied and the \
                             target schedule resurrects on resume",
                        );
                    }
                }
                _ => {}
            }
        }

        records.retain(|_, record| rearm_on_resume(record, now));
        Self {
            inner: Mutex::new(records),
            notify: Notify::new(),
        }
    }

    /// Insert (or replace) a pending record and wake the executor.
    pub fn insert(&self, record: ScheduleRecord) {
        self.inner.lock().insert(record.id, record);
        self.notify.notify_one();
    }

    /// Remove a pending record by id, waking the executor. Returns `true`
    /// when a pending record was actually removed — `false` for an unknown
    /// or already-terminal id (the caller reports `NotFound`).
    pub fn cancel(&self, id: Uuid) -> bool {
        let removed = self.inner.lock().remove(&id).is_some();
        if removed {
            self.notify.notify_one();
        }
        removed
    }

    /// Every pending record, cloned, in no particular order.
    #[must_use]
    pub fn list_pending(&self) -> Vec<ScheduleRecord> {
        self.inner.lock().values().cloned().collect()
    }

    /// Fetch a pending record by id.
    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<ScheduleRecord> {
        self.inner.lock().get(&id).cloned()
    }

    /// The earliest `next_fire` across all pending records, if any.
    #[must_use]
    pub fn next_fire(&self) -> Option<DateTime<Utc>> {
        self.inner
            .lock()
            .values()
            .map(|record| record.next_fire)
            .min()
    }

    /// A snapshot (clones) of every record whose `next_fire` is at or before
    /// `at`, in ascending fire order. Does not mutate the store — the
    /// executor delivers each first, then calls [`Self::complete_fire`].
    #[must_use]
    pub fn due_at(&self, at: DateTime<Utc>) -> Vec<ScheduleRecord> {
        let mut due: Vec<ScheduleRecord> = self
            .inner
            .lock()
            .values()
            .filter(|record| record.next_fire <= at)
            .cloned()
            .collect();
        due.sort_by_key(|record| record.next_fire);
        due
    }

    /// Complete a fire: re-arm a recurring schedule from `fire_time` (next
    /// occurrence strictly after the logical fire instant — no drift, no
    /// backfill) or remove a one-shot. Clears the `late` flag either way, so
    /// a re-armed schedule's subsequent fires are on-time. A recurring
    /// schedule whose expression no longer computes is removed with an error
    /// log. Wakes the executor to recompute the earliest fire.
    ///
    /// **Live catch-up after a suspend.** Anchoring the re-arm to `fire_time`
    /// is correct for eliminating drift, but when the process was suspended
    /// (timers frozen while wall-clock jumped forward — laptop sleep, a paused
    /// container) the drift-anchored `next` can already be in the past. Left
    /// alone the executor would replay every missed occurrence one-by-one
    /// (an 8h sleep against `every:"1m"` ≈ 480 back-to-back steers). When the
    /// re-armed `next` is at or before `now`, this collapses the backlog into
    /// a single next natural fire computed from `now`, logging the skip. This
    /// **extends the owner's no-backfill resume ruling (DECISIONS §0.6(e)) to
    /// the live path after a suspend — flagged for owner confirmation.**
    ///
    /// `now` is caller-supplied — this module performs no hidden clock reads,
    /// so the live executor (which passes `Utc::now()`) and every test (which
    /// passes fixture-consistent instants) compute fire times identically and
    /// deterministically.
    pub fn complete_fire(&self, id: Uuid, fire_time: DateTime<Utc>, now: DateTime<Utc>) {
        let mut inner = self.inner.lock();
        let Some(record) = inner.get_mut(&id) else {
            return;
        };
        if record.spec.is_recurring() {
            match record.spec.next_fire_after(fire_time) {
                Ok(next) => {
                    if next <= now {
                        // A suspend froze timers past several occurrences.
                        // Collapse the backlog to a single fire from `now`
                        // (no backfill) rather than bursting the agent.
                        match catch_up_next_fire(id, record, next, now) {
                            Some(caught_up) => {
                                record.next_fire = caught_up;
                                record.late = false;
                            }
                            None => {
                                inner.remove(&id);
                            }
                        }
                    } else {
                        record.next_fire = next;
                        record.late = false;
                    }
                }
                Err(error) => {
                    tracing::error!(
                        schedule_id = %id,
                        %error,
                        "schedule store: recurring schedule no longer computes a next \
                         fire; removing it rather than leaving it un-armable",
                    );
                    inner.remove(&id);
                }
            }
        } else {
            inner.remove(&id);
        }
        drop(inner);
        self.notify.notify_one();
    }

    /// A future that resolves the next time the pending set changes.
    ///
    /// Created before the executor reads [`Self::next_fire`] each loop so a
    /// change racing the read is never lost (the [`Notify`] holds one
    /// permit).
    pub fn notified(&self) -> Notified<'_> {
        self.notify.notified()
    }

    /// Number of pending records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Whether the store holds no pending records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

impl Default for ScheduleStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Re-arm one surviving record against the resume instant. Returns whether
/// the record should be retained.
fn rearm_on_resume(record: &mut ScheduleRecord, now: DateTime<Utc>) -> bool {
    if record.spec.is_recurring() {
        match record.spec.next_fire_after(now) {
            Ok(next) => {
                record.next_fire = next;
                record.late = false;
                true
            }
            Err(error) => {
                tracing::error!(
                    schedule_id = %record.id,
                    %error,
                    "schedule store: recurring schedule failed to re-arm on resume; dropping it",
                );
                false
            }
        }
    } else {
        // One-shot: a fire time that already passed during downtime fires
        // immediately, marked late; a still-future fire is left untouched.
        if record.next_fire <= now {
            record.late = true;
        }
        true
    }
}

/// Compute the single catch-up fire for a recurring record whose
/// drift-anchored `drifted_next` fell at or before `now` (a suspend froze
/// timers past several occurrences). Returns `Some(next_fire)` — the next
/// natural occurrence strictly after `now`, collapsing the missed backlog
/// with no backfill — or `None` when the expression can no longer compute
/// (the caller removes the record). Logs the skip: a skipped-occurrence
/// count for [`ScheduleSpec::Every`] (cheap fixed-interval arithmetic),
/// otherwise the skipped time range (a per-occurrence count is not cheap for
/// a cron expression).
fn catch_up_next_fire(
    id: Uuid,
    record: &ScheduleRecord,
    drifted_next: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match record.spec.next_fire_after(now) {
        Ok(caught_up) => {
            match &record.spec {
                ScheduleSpec::Every { duration } => {
                    // Occurrences in (fire_time, now] were skipped; with a
                    // fixed interval that is floor(elapsed / interval),
                    // counting the first drifted occurrence itself.
                    let elapsed = u64::try_from((now - drifted_next).num_seconds()).unwrap_or(0);
                    let skipped = elapsed
                        .checked_div(duration.as_secs())
                        .map_or(0, |occurrences| occurrences + 1);
                    tracing::info!(
                        schedule_id = %id,
                        skipped_occurrences = skipped,
                        next_fire = %caught_up,
                        "schedule store: live catch-up after a suspend collapsed missed \
                         occurrences into a single next fire (no backfill); extends the \
                         owner's no-backfill resume ruling (DECISIONS §0.6(e)) to the \
                         live path — flagged for owner confirmation",
                    );
                }
                _ => {
                    tracing::info!(
                        schedule_id = %id,
                        skipped_from = %drifted_next,
                        skipped_until = %now,
                        next_fire = %caught_up,
                        "schedule store: live catch-up after a suspend collapsed the \
                         occurrences in this time range into a single next fire (no \
                         backfill); extends the owner's no-backfill resume ruling \
                         (DECISIONS §0.6(e)) to the live path — flagged for owner \
                         confirmation",
                    );
                }
            }
            Some(caught_up)
        }
        Err(error) => {
            tracing::error!(
                schedule_id = %id,
                %error,
                "schedule store: recurring schedule no longer computes a next fire \
                 during live catch-up; removing it rather than leaving it un-armable",
            );
            None
        }
    }
}

/// Extract a schedule id from a `schedule.fired` / `schedule.cancelled`
/// payload's `id` field.
fn schedule_id_from(data: &serde_json::Value) -> Option<Uuid> {
    data.get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use chrono::TimeZone;

    use super::*;
    use crate::schedule::entry::ScheduleSpec;
    use crate::schedule::events::append_schedule_event;
    use crate::session::store::EventStore;

    fn record(spec: ScheduleSpec, created: DateTime<Utc>) -> ScheduleRecord {
        ScheduleRecord::new(
            Uuid::new_v4(),
            spec,
            "ping".to_string(),
            Uuid::new_v4(),
            created,
        )
        .unwrap()
    }

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
            .unwrap()
    }

    #[test]
    fn insert_list_and_next_fire() {
        let store = ScheduleStore::new();
        let now = Utc::now();
        let soon = record(
            ScheduleSpec::In {
                duration: Duration::from_mins(1),
            },
            now,
        );
        let later = record(
            ScheduleSpec::In {
                duration: Duration::from_hours(1),
            },
            now,
        );
        let soon_fire = soon.next_fire;
        store.insert(soon);
        store.insert(later);
        assert_eq!(store.len(), 2);
        assert_eq!(store.list_pending().len(), 2);
        assert_eq!(store.next_fire(), Some(soon_fire));
    }

    #[test]
    fn cancel_removes_and_reports_absence() {
        let store = ScheduleStore::new();
        let rec = record(
            ScheduleSpec::In {
                duration: Duration::from_mins(1),
            },
            Utc::now(),
        );
        let id = rec.id;
        store.insert(rec);
        assert!(store.cancel(id), "first cancel removes the record");
        assert!(!store.cancel(id), "second cancel reports absence");
        assert!(store.is_empty());
    }

    /// Cron re-arm computes croner's next occurrence strictly after the
    /// fire time — successive fires land on exact 5-minute marks with no
    /// drift accumulation.
    #[test]
    fn cron_rearm_uses_next_occurrence_after_fire_time_without_drift() {
        let store = ScheduleStore::new();
        let created = at(2026, 7, 3, 12, 2, 30);
        let rec = record(
            ScheduleSpec::Cron {
                expr: "*/5 * * * *".to_string(),
            },
            created,
        );
        let id = rec.id;
        let first = rec.next_fire;
        assert_eq!(first, at(2026, 7, 3, 12, 5, 0));
        store.insert(rec);

        store.complete_fire(id, first, first);
        let second = store.get(id).expect("recurring survives").next_fire;
        assert_eq!(second, at(2026, 7, 3, 12, 10, 0), "exact next mark");

        store.complete_fire(id, second, second);
        let third = store.get(id).expect("recurring survives").next_fire;
        assert_eq!(
            third,
            at(2026, 7, 3, 12, 15, 0),
            "no drift across two re-arms",
        );
    }

    #[test]
    fn no_count_cap_ten_thousand_records() {
        let store = ScheduleStore::new();
        let now = Utc::now();
        for _ in 0..10_000 {
            store.insert(record(
                ScheduleSpec::Every {
                    duration: Duration::from_hours(1),
                },
                now,
            ));
        }
        assert_eq!(store.len(), 10_000, "no cap on schedule count");
    }

    #[test]
    fn due_at_returns_only_elapsed_records_in_order() {
        let store = ScheduleStore::new();
        let base = at(2026, 7, 3, 12, 0, 0);
        let a = record(
            ScheduleSpec::In {
                duration: Duration::from_mins(1),
            },
            base,
        );
        let b = record(
            ScheduleSpec::In {
                duration: Duration::from_mins(2),
            },
            base,
        );
        let a_fire = a.next_fire;
        store.insert(a);
        store.insert(b);
        let due = store.due_at(a_fire);
        assert_eq!(due.len(), 1, "only the first is due at its fire time");
        assert_eq!(due[0].next_fire, a_fire);
    }

    #[test]
    fn complete_fire_rearms_recurring_and_removes_one_shot() {
        let store = ScheduleStore::new();
        let base = at(2026, 7, 3, 12, 0, 0);
        let every = record(
            ScheduleSpec::Every {
                duration: Duration::from_mins(1),
            },
            base,
        );
        let every_id = every.id;
        let first_fire = every.next_fire;
        store.insert(every);
        store.complete_fire(every_id, first_fire, first_fire);
        let rearmed = store.get(every_id).expect("recurring survives");
        assert_eq!(
            rearmed.next_fire,
            first_fire + chrono::TimeDelta::seconds(60),
            "recurring re-arms from the fire time with no drift",
        );

        let one_shot = record(
            ScheduleSpec::In {
                duration: Duration::from_mins(1),
            },
            base,
        );
        let one_shot_id = one_shot.id;
        let fire = one_shot.next_fire;
        store.insert(one_shot);
        store.complete_fire(one_shot_id, fire, fire);
        assert!(store.get(one_shot_id).is_none(), "one-shot completes out");
    }

    #[test]
    fn from_events_rebuilds_pending_minus_cancelled_and_fired() {
        let event_store = EventStore::new();
        let now = Utc::now();

        let in_shot = record(
            ScheduleSpec::In {
                duration: Duration::from_hours(1),
            },
            now,
        );
        let every = record(
            ScheduleSpec::Every {
                duration: Duration::from_hours(1),
            },
            now,
        );
        let cron = record(
            ScheduleSpec::Cron {
                expr: "0 9 * * *".to_string(),
            },
            now,
        );
        let (in_id, every_id, cron_id) = (in_shot.id, every.id, cron.id);

        for rec in [&in_shot, &every, &cron] {
            append_schedule_event(
                &event_store,
                &ScheduleLifecycle::Created {
                    record: rec.clone(),
                },
            )
            .unwrap();
        }
        // Cancel the cron; fire the in one-shot out.
        append_schedule_event(
            &event_store,
            &ScheduleLifecycle::Cancelled {
                id: cron_id,
                cancelled_at: Utc::now(),
            },
        )
        .unwrap();
        append_schedule_event(
            &event_store,
            &ScheduleLifecycle::Fired {
                id: in_id,
                fired_at: Utc::now(),
                late: false,
            },
        )
        .unwrap();

        let rebuilt =
            ScheduleStore::from_events(&event_store.events(), now + chrono::TimeDelta::seconds(1));
        let pending: Vec<_> = rebuilt.list_pending();
        assert_eq!(pending.len(), 1, "only the recurring `every` remains");
        assert_eq!(pending[0].id, every_id);
    }

    #[test]
    fn resume_past_due_one_shot_is_marked_late_and_due_now() {
        let event_store = EventStore::new();
        let created = at(2026, 7, 3, 12, 0, 0);
        let one_shot = record(
            ScheduleSpec::In {
                duration: Duration::from_secs(1),
            },
            created,
        );
        let id = one_shot.id;
        append_schedule_event(
            &event_store,
            &ScheduleLifecycle::Created { record: one_shot },
        )
        .unwrap();

        // Resume well past the fire time (a dead period).
        let resume = created + chrono::TimeDelta::hours(2);
        let rebuilt = ScheduleStore::from_events(&event_store.events(), resume);
        let pending = rebuilt.get(id).expect("past-due one-shot survives");
        assert!(pending.late, "a past-due one-shot fires late");
        assert!(pending.next_fire <= resume, "and is immediately due");
        assert!(!rebuilt.due_at(resume).is_empty());
    }

    /// Finding 2 (direct): `complete_fire` on a recurring schedule whose
    /// drift-anchored re-arm falls in the past (a suspend froze timers)
    /// collapses to a single next fire strictly after `now` — no per-occurrence
    /// backfill on the live path, mirroring the resume ruling. Fully fixed
    /// dates: the caller-supplied `now` (F8) makes the boundary deterministic
    /// with zero ambient clock reads.
    #[test]
    fn complete_fire_live_catch_up_rearms_from_now_not_fire_time() {
        let store = ScheduleStore::new();
        // Created at 04:00; the process then "sleeps" until noon — the first
        // fire (04:01) is eight hours in the past at completion time.
        let created = at(2026, 7, 3, 4, 0, 0);
        let now = at(2026, 7, 3, 12, 0, 0);
        let every = record(
            ScheduleSpec::Every {
                duration: Duration::from_mins(1),
            },
            created,
        );
        let id = every.id;
        let first_fire = every.next_fire;
        assert_eq!(first_fire, at(2026, 7, 3, 4, 1, 0));
        store.insert(every);

        store.complete_fire(id, first_fire, now);

        let rearmed = store.get(id).expect("recurring survives catch-up");
        assert_eq!(
            rearmed.next_fire,
            at(2026, 7, 3, 12, 1, 0),
            "catch-up re-arms to exactly one natural fire from now — not one \
             drift step past the old fire time, and no backfill of the ~480 \
             missed occurrences",
        );
        assert!(!rearmed.late, "the catch-up fire clears the late flag");
    }

    /// F8 boundary: a re-arm landing exactly ON `now` (`next == now`) takes
    /// the catch-up branch and still yields a strictly-future fire; a re-arm
    /// strictly after `now` is genuinely on-time and is never skipped.
    #[test]
    fn complete_fire_boundary_next_equal_now_catches_up_and_future_is_untouched() {
        // next == now → catch-up: fire completed at 11:59, interval 1m, so
        // the drift re-arm is exactly 12:00 == now → single fire at 12:01.
        let store = ScheduleStore::new();
        let every = record(
            ScheduleSpec::Every {
                duration: Duration::from_mins(1),
            },
            at(2026, 7, 3, 11, 58, 0),
        );
        let id = every.id;
        let first = every.next_fire;
        assert_eq!(first, at(2026, 7, 3, 11, 59, 0));
        store.insert(every);
        store.complete_fire(id, first, at(2026, 7, 3, 12, 0, 0));
        assert_eq!(
            store.get(id).expect("survives").next_fire,
            at(2026, 7, 3, 12, 1, 0),
            "next == now is treated as missed: one strictly-future fire",
        );

        // next > now → on-time: identical shape, but completion is observed
        // a second before the re-arm instant — the drift anchor is preserved.
        let on_time = ScheduleStore::new();
        let every2 = record(
            ScheduleSpec::Every {
                duration: Duration::from_mins(1),
            },
            at(2026, 7, 3, 11, 58, 0),
        );
        let id2 = every2.id;
        let first2 = every2.next_fire;
        on_time.insert(every2);
        on_time.complete_fire(id2, first2, at(2026, 7, 3, 11, 59, 59));
        assert_eq!(
            on_time.get(id2).expect("survives").next_fire,
            at(2026, 7, 3, 12, 0, 0),
            "a genuinely on-time fire keeps the drift-anchored re-arm",
        );
    }

    /// Finding 3 (documented, inherent): a corrupt `schedule.cancelled`
    /// payload (no parseable id) cannot apply its cancellation, so the target
    /// schedule silently resurrects on resume. This is inherent to the
    /// corruption — no recovery is invented; the warn logged at the fold site
    /// is the honest signal. This test pins the behavior so it stays a
    /// conscious contract rather than a surprise.
    #[test]
    fn corrupt_cancelled_payload_resurrects_target_on_resume() {
        use crate::session::events::{EventBase, SessionEvent};

        let event_store = EventStore::new();
        let now = Utc::now();
        let every = record(
            ScheduleSpec::Every {
                duration: Duration::from_hours(1),
            },
            now,
        );
        let id = every.id;
        append_schedule_event(&event_store, &ScheduleLifecycle::Created { record: every }).unwrap();

        // A schedule.cancelled event whose payload carries no valid id — the
        // cancellation cannot be matched to its schedule.
        event_store
            .append(SessionEvent::Custom {
                base: EventBase::new(event_store.last_event_id()),
                event_type: SCHEDULE_CANCELLED_EVENT_TYPE.to_owned(),
                data: serde_json::json!({ "id": "not-a-uuid", "phase": "cancelled" }),
            })
            .unwrap();

        let rebuilt = ScheduleStore::from_events(&event_store.events(), now);
        assert!(
            rebuilt.get(id).is_some(),
            "the corrupt cancellation cannot retire the schedule; it resurrects \
             (inherent — the fold logs a warn)",
        );
    }

    #[test]
    fn resume_recurring_rearms_without_backfill() {
        let event_store = EventStore::new();
        let created = at(2026, 7, 3, 12, 0, 0);
        let every = record(
            ScheduleSpec::Every {
                duration: Duration::from_hours(1),
            },
            created,
        );
        let id = every.id;
        append_schedule_event(&event_store, &ScheduleLifecycle::Created { record: every }).unwrap();

        // Resume 5 hours later — 5 occurrences were missed.
        let resume = created + chrono::TimeDelta::hours(5);
        let rebuilt = ScheduleStore::from_events(&event_store.events(), resume);
        let pending = rebuilt.get(id).expect("recurring survives");
        assert!(!pending.late);
        assert!(
            pending.next_fire > resume && pending.next_fire <= resume + chrono::TimeDelta::hours(1),
            "a single next fire within the hour, no backfill: {}",
            pending.next_fire,
        );
        assert!(
            rebuilt.due_at(resume).is_empty(),
            "no missed occurrence fires immediately",
        );
    }
}
