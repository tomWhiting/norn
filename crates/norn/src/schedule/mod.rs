//! In-session scheduling: a `cron`-tool-driven schedule store, a live tokio
//! executor that fires due schedules as durable injected messages, and
//! session-event persistence with resume restore.
//!
//! Owner ruling DECISIONS §0.6(e): relative wake-ups ("in N"), time-of-day,
//! looping intervals, and full cron expressions; fired schedules deliver as
//! injected messages; schedules persist as session events so resume restores
//! them; no caps on count or interval; in-session first, the daemon phase
//! later. Timers die with the process — only the session-event record
//! survives, for resume.
//!
//! - [`entry`] — the schedule kinds, records, and pure next-fire computation.
//! - [`store`] — the in-memory store with change notification and event rebuild.
//! - [`executor`] — the live per-agent timer task and its delivery paths.
//! - [`events`] — the custom session-event payloads and their append helper.

pub mod entry;
pub mod events;
pub mod executor;
pub mod store;

pub use entry::{
    AtSpec, ScheduleDisposition, ScheduleError, ScheduleRecord, ScheduleSpec, parse_duration,
    parse_time_of_day,
};
pub use events::{
    SCHEDULE_CANCELLED_EVENT_TYPE, SCHEDULE_CREATED_EVENT_TYPE, SCHEDULE_FIRED_EVENT_TYPE,
    ScheduleLifecycle, append_schedule_event,
};
pub use executor::{
    CRON_SENDER_LABEL, ScheduleDelivery, ScheduleExecutorGuard, ScheduleHandle,
    arm_schedule_executor,
};
pub use store::ScheduleStore;
