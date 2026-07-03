//! Schedule kinds, records, and the pure next-fire computation.
//!
//! A [`ScheduleSpec`] is one of four kinds — a relative one-shot
//! ([`ScheduleSpec::In`]), a time-of-day one-shot ([`ScheduleSpec::At`]), a
//! looping interval ([`ScheduleSpec::Every`]), or a full cron expression
//! ([`ScheduleSpec::Cron`]) — and [`ScheduleSpec::next_fire_after`] is a pure
//! function of `(spec, after)` with no hidden clock reads, so both the live
//! executor and the resume-rebuild path compute fire times identically.
//!
//! The module imposes **no** cap on schedule count and **no** minimum or
//! maximum interval: an `Every("1s")` and an `Every("365d")` both construct.
//! A zero-length duration is the one rejected degenerate input — it names no
//! future instant (an interval of zero would re-arm onto its own fire time),
//! exactly as a negative duration is rejected; this is not an interval bound
//! on positive values.

use std::time::Duration;

use chrono::{DateTime, Local, LocalResult, NaiveDate, TimeZone, Utc};
use croner::Cron;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A parse or next-fire computation failure for a schedule.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ScheduleError {
    /// A relative-duration string did not match the `<integer><unit>`
    /// grammar (unit one of `s`/`m`/`h`/`d`, integer strictly positive).
    #[error(
        "invalid duration '{input}': expected a positive integer followed by a unit \
         s (seconds), m (minutes), h (hours), or d (days) — e.g. \"90s\", \"15m\", \"2h\", \"3d\""
    )]
    InvalidDuration {
        /// The rejected input.
        input: String,
    },

    /// A time-of-day string was not `HH:MM` in 24-hour form.
    #[error("invalid time-of-day '{input}': expected HH:MM in 24-hour form (00:00 through 23:59)")]
    InvalidTimeOfDay {
        /// The rejected input.
        input: String,
    },

    /// A cron expression failed to parse (carries croner's own message).
    #[error("invalid cron expression '{expr}': {reason}")]
    InvalidCron {
        /// The rejected expression.
        expr: String,
        /// The croner parse failure.
        reason: String,
    },

    /// A parsed cron expression could not yield a next occurrence.
    #[error("failed to compute next fire for cron expression '{expr}': {reason}")]
    CronComputation {
        /// The expression whose next occurrence could not be computed.
        expr: String,
        /// The croner failure.
        reason: String,
    },

    /// A time-of-day could not be resolved to any concrete local instant
    /// within the search window (a pathological zone with no valid local
    /// time for the requested hour/minute on any nearby day).
    #[error("time-of-day {hour:02}:{minute:02} has no representable local instant near {after}")]
    UnresolvableTimeOfDay {
        /// Requested hour (0–23).
        hour: u32,
        /// Requested minute (0–59).
        minute: u32,
        /// The instant the search started after.
        after: DateTime<Utc>,
    },

    /// A duration matched the grammar but exceeds what the scheduling
    /// arithmetic can represent (chrono's time-delta range).
    #[error("duration of {seconds}s is outside the schedulable range: {reason}")]
    DurationOutOfRange {
        /// The requested duration, in seconds.
        seconds: u64,
        /// Why the arithmetic rejected it.
        reason: String,
    },

    /// A programmatically-constructed `In`/`Every` carried a zero-length
    /// duration. A zero interval names no future instant — and for `Every`
    /// would re-arm onto its own fire time and busy-spin. The tool grammar
    /// rejects this earlier in [`parse_duration`]; this is the type-boundary
    /// guard for direct construction that bypasses the grammar.
    #[error(
        "a '{kind}' schedule requires a positive duration; a zero-length interval names \
         no future instant"
    )]
    ZeroDuration {
        /// The schedule kind that carried the zero duration (`in` or `every`).
        kind: &'static str,
    },
}

/// Parse a relative-duration string of the form `<integer><unit>`.
///
/// The unit is a single trailing character: `s` (seconds), `m` (minutes),
/// `h` (hours), or `d` (days). The integer is one or more ASCII digits and
/// must be strictly positive. Everything else — a bare number (`"15"`), a
/// spaced or spelled-out unit (`"2 hours"`), a sign (`"-5m"`), an unknown
/// unit (`"0x"`), or a zero magnitude (`"0s"`) — is rejected with a
/// [`ScheduleError::InvalidDuration`] naming the expected grammar.
///
/// # Errors
///
/// Returns [`ScheduleError::InvalidDuration`] when `input` does not match the
/// grammar or the magnitude overflows.
pub fn parse_duration(input: &str) -> Result<Duration, ScheduleError> {
    let err = || ScheduleError::InvalidDuration {
        input: input.to_string(),
    };
    let unit = input.chars().last().ok_or_else(err)?;
    let multiplier: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        _ => return Err(err()),
    };
    let value_str = &input[..input.len() - unit.len_utf8()];
    if value_str.is_empty() || !value_str.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err());
    }
    // The digits-only guard above leaves overflow as the only parse
    // failure; the grammar error's message already names the accepted shape.
    let Ok(value) = value_str.parse::<u64>() else {
        return Err(err());
    };
    if value == 0 {
        // A zero-length relative wake-up or interval names no future
        // instant; reject it like a negative duration rather than accept a
        // spec that would fire onto its own creation time (and, for
        // Every, spin). This is not a minimum-interval bound: every
        // positive magnitude, from "1s" up, is accepted.
        return Err(err());
    }
    let secs = value.checked_mul(multiplier).ok_or_else(err)?;
    Ok(Duration::from_secs(secs))
}

/// Parse an `HH:MM` 24-hour time-of-day into `(hour, minute)`.
///
/// # Errors
///
/// Returns [`ScheduleError::InvalidTimeOfDay`] when `input` is not exactly
/// two colon-separated fields with hour in `0..=23` and minute in `0..=59`.
pub fn parse_time_of_day(input: &str) -> Result<(u32, u32), ScheduleError> {
    let err = || ScheduleError::InvalidTimeOfDay {
        input: input.to_string(),
    };
    let (hour_str, minute_str) = input.split_once(':').ok_or_else(err)?;
    if hour_str.is_empty()
        || minute_str.is_empty()
        || !hour_str.bytes().all(|b| b.is_ascii_digit())
        || !minute_str.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(err());
    }
    // The digits-only guard above leaves overflow as the only parse
    // failure; the grammar error's message already names the accepted shape.
    let (Ok(hour), Ok(minute)) = (hour_str.parse::<u32>(), minute_str.parse::<u32>()) else {
        return Err(err());
    };
    if hour > 23 || minute > 59 {
        return Err(err());
    }
    Ok((hour, minute))
}

/// The concrete target of a time-of-day one-shot ([`ScheduleSpec::At`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at_kind", rename_all = "snake_case")]
pub enum AtSpec {
    /// A wall-clock time-of-day resolved to its next occurrence in the
    /// host's local timezone.
    TimeOfDay {
        /// Hour in `0..=23`.
        hour: u32,
        /// Minute in `0..=59`.
        minute: u32,
    },
    /// An explicit absolute instant (an RFC 3339 timestamp at construction).
    Instant {
        /// The fixed target instant, in UTC.
        instant: DateTime<Utc>,
    },
}

impl AtSpec {
    /// The next fire at or after `after` for this time-of-day target.
    fn next_fire_after(&self, after: DateTime<Utc>) -> Result<DateTime<Utc>, ScheduleError> {
        match self {
            Self::Instant { instant } => Ok(*instant),
            Self::TimeOfDay { hour, minute } => next_time_of_day(*hour, *minute, after, &Local),
        }
    }
}

/// One of the four schedule kinds ruled by the owner (DECISIONS §0.6(e)).
///
/// The tagged JSON shape (`{"kind": "in", "duration": {...}}`, `{"kind":
/// "at", "at_kind": "time_of_day", ...}`, `{"kind": "every", ...}`,
/// `{"kind": "cron", "expr": "..."}`) is the persisted `schedule.created`
/// payload's spec field, so a future daemon can read it back verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleSpec {
    /// Relative one-shot: fire once, `duration` after the schedule was
    /// created ("in N").
    In {
        /// The relative delay from creation.
        duration: Duration,
    },
    /// Time-of-day one-shot: fire once at the next occurrence of the
    /// [`AtSpec`] target.
    At {
        /// The time-of-day target.
        at: AtSpec,
    },
    /// Looping interval: fire every `duration`, re-arming on each fire.
    Every {
        /// The interval between fires.
        duration: Duration,
    },
    /// Full cron expression, evaluated in UTC exactly as the deleted
    /// `compute_next_run` did (croner), re-arming on each fire.
    Cron {
        /// The cron expression.
        expr: String,
    },
}

impl ScheduleSpec {
    /// Compute the next fire strictly after `after`.
    ///
    /// A pure function of `(self, after)` — no clock reads, no interior
    /// state. `In`/`Every` add their interval; `At` resolves its target;
    /// `Cron` asks croner for the next non-inclusive occurrence.
    ///
    /// The one documented exception to "strictly after `after`" is
    /// [`AtSpec::Instant`]: an explicit absolute target returns its fixed
    /// instant verbatim, even when that instant is at or before `after` — so
    /// an `At` scheduled for an already-past instant fires once, immediately,
    /// by design (mirroring a resume-restored past-due one-shot). Every other
    /// kind yields an instant strictly after `after`.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError`] when a cron expression fails to parse or
    /// compute, when a duration is unrepresentable as a chrono delta, or
    /// when a time-of-day cannot be resolved locally.
    pub fn next_fire_after(&self, after: DateTime<Utc>) -> Result<DateTime<Utc>, ScheduleError> {
        match self {
            Self::In { duration } | Self::Every { duration } => add_duration(after, *duration),
            Self::At { at } => at.next_fire_after(after),
            Self::Cron { expr } => cron_next(expr, after),
        }
    }

    /// Whether this kind re-arms on fire (`Every`, `Cron`) rather than
    /// completing after a single fire (`In`, `At`).
    #[must_use]
    pub const fn is_recurring(&self) -> bool {
        matches!(self, Self::Every { .. } | Self::Cron { .. })
    }

    /// Stable label used in tool responses, event payloads, and the
    /// injected message frame.
    #[must_use]
    pub const fn kind_label(&self) -> &'static str {
        match self {
            Self::In { .. } => "in",
            Self::At { .. } => "at",
            Self::Every { .. } => "every",
            Self::Cron { .. } => "cron",
        }
    }
}

/// Add a [`std::time::Duration`] to a UTC instant, surfacing an overflow as
/// a typed error rather than panicking.
fn add_duration(after: DateTime<Utc>, duration: Duration) -> Result<DateTime<Utc>, ScheduleError> {
    let delta = chrono::TimeDelta::from_std(duration).map_err(|error| {
        ScheduleError::DurationOutOfRange {
            seconds: duration.as_secs(),
            reason: error.to_string(),
        }
    })?;
    after
        .checked_add_signed(delta)
        .ok_or_else(|| ScheduleError::DurationOutOfRange {
            seconds: duration.as_secs(),
            reason: "the resulting instant overflows the representable time range".to_string(),
        })
}

/// Croner next-occurrence, matching the deleted `compute_next_run`'s error
/// fidelity exactly (croner's own parse and compute messages).
fn cron_next(expr: &str, after: DateTime<Utc>) -> Result<DateTime<Utc>, ScheduleError> {
    let cron = Cron::new(expr)
        .parse()
        .map_err(|e| ScheduleError::InvalidCron {
            expr: expr.to_string(),
            reason: e.to_string(),
        })?;
    cron.find_next_occurrence(&after, false)
        .map_err(|e| ScheduleError::CronComputation {
            expr: expr.to_string(),
            reason: e.to_string(),
        })
}

/// The next concrete UTC instant at which local `hour:minute` occurs strictly
/// after `after`, in timezone `tz`.
///
/// DST-safe: a fall-back fold ([`LocalResult::Ambiguous`], the local time
/// occurring twice) resolves to the **earliest** of the two instants; a
/// spring-forward gap ([`LocalResult::None`], the local time not existing on
/// that day) is skipped to the following day. Searches a bounded window of
/// days so a pathological zone cannot loop forever.
fn next_time_of_day<Tz: TimeZone>(
    hour: u32,
    minute: u32,
    after: DateTime<Utc>,
    tz: &Tz,
) -> Result<DateTime<Utc>, ScheduleError> {
    // Start on the local calendar day of `after` and walk forward. A window
    // of a few days is far more than any real zone needs (a valid local
    // hour:minute recurs daily); the bound only guards against a broken
    // zone with no valid instant at all.
    let mut date = after.with_timezone(tz).date_naive();
    for _ in 0..DAY_SEARCH_WINDOW {
        if let Some(candidate) = resolve_local_time(date, hour, minute, tz)
            && candidate > after
        {
            return Ok(candidate);
        }
        date = date
            .succ_opt()
            .ok_or(ScheduleError::UnresolvableTimeOfDay {
                hour,
                minute,
                after,
            })?;
    }
    Err(ScheduleError::UnresolvableTimeOfDay {
        hour,
        minute,
        after,
    })
}

/// How many local days [`next_time_of_day`] scans before giving up.
const DAY_SEARCH_WINDOW: u32 = 8;

/// Resolve `date` at `hour:minute` in `tz` to a concrete UTC instant,
/// applying the DST fold/gap policy of [`pick_from_local_result`].
fn resolve_local_time<Tz: TimeZone>(
    date: NaiveDate,
    hour: u32,
    minute: u32,
    tz: &Tz,
) -> Option<DateTime<Utc>> {
    let naive = date.and_hms_opt(hour, minute, 0)?;
    pick_from_local_result(tz.from_local_datetime(&naive))
}

/// The DST fold/gap policy: a single valid local time is taken as-is, a
/// fall-back fold takes the earliest of the two instants, and a
/// spring-forward gap yields `None` (the caller advances a day).
fn pick_from_local_result<Tz: TimeZone>(
    result: LocalResult<DateTime<Tz>>,
) -> Option<DateTime<Utc>> {
    match result {
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(earliest, _latest) => Some(earliest.with_timezone(&Utc)),
        LocalResult::None => None,
    }
}

/// A terminal disposition for a schedule that will never fire again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleDisposition {
    /// A one-shot that has fired and completed.
    FiredOut,
    /// Cancelled by the owning agent before completing.
    Cancelled,
}

/// A live schedule record held in the store and persisted as
/// `schedule.created`.
///
/// Carries a stable id, the [`ScheduleSpec`], the agent-supplied message
/// injected on fire, the owning agent id, the creation instant, the computed
/// `next_fire`, an optional terminal `disposition`, and the `late` flag — set
/// only when a resume rebuild finds a one-shot whose fire time already
/// passed, so its immediate catch-up fire is marked late.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRecord {
    /// Stable identifier, shared with every `schedule.*` event for this
    /// schedule.
    pub id: Uuid,
    /// The schedule kind and its parameters.
    pub spec: ScheduleSpec,
    /// The agent-supplied text injected when the schedule fires.
    pub message: String,
    /// The agent that owns (and is woken by) this schedule.
    pub owning_agent_id: Uuid,
    /// Wall-clock creation instant.
    pub created_at: DateTime<Utc>,
    /// The next instant this schedule fires.
    pub next_fire: DateTime<Utc>,
    /// Terminal disposition once the schedule will never fire again;
    /// `None` while pending.
    pub disposition: Option<ScheduleDisposition>,
    /// Whether the next fire is a past-due catch-up (a resume-restored
    /// one-shot whose fire time elapsed during downtime).
    pub late: bool,
}

impl ScheduleRecord {
    /// Construct a fresh pending record, computing `next_fire` from `spec`
    /// relative to `created_at`.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError::ZeroDuration`] when `spec` is an `In` or
    /// `Every` with a zero-length duration (the type-boundary guard for
    /// direct construction that bypasses the tool grammar's rejection), and
    /// propagates any [`ScheduleError`] from [`ScheduleSpec::next_fire_after`].
    pub fn new(
        id: Uuid,
        spec: ScheduleSpec,
        message: String,
        owning_agent_id: Uuid,
        created_at: DateTime<Utc>,
    ) -> Result<Self, ScheduleError> {
        // Reject a zero-length interval at the type boundary: a zero `In`/
        // `Every` is constructible programmatically (public fields) and would
        // name its own creation instant — and, for `Every`, busy-spin re-arming
        // onto that instant. The tool grammar rejects it earlier; this closes
        // the direct-construction bypass.
        match &spec {
            ScheduleSpec::In { duration } if duration.is_zero() => {
                return Err(ScheduleError::ZeroDuration { kind: "in" });
            }
            ScheduleSpec::Every { duration } if duration.is_zero() => {
                return Err(ScheduleError::ZeroDuration { kind: "every" });
            }
            _ => {}
        }
        let next_fire = spec.next_fire_after(created_at)?;
        Ok(Self {
            id,
            spec,
            message,
            owning_agent_id,
            created_at,
            next_fire,
            disposition: None,
            late: false,
        })
    }

    /// Whether this record is still pending (no terminal disposition).
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.disposition.is_none()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use chrono::{Datelike, FixedOffset, TimeZone};

    // ----- duration parsing ------------------------------------------------

    #[test]
    fn parse_duration_accepts_each_unit() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("15m").unwrap(), Duration::from_mins(15));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_hours(2));
        assert_eq!(parse_duration("3d").unwrap(), Duration::from_hours(3 * 24));
    }

    #[test]
    fn parse_duration_rejects_malformed_inputs_with_grammar_message() {
        for bad in ["15", "2 hours", "-5m", "0x", "", "s", "m", "1.5h", "0s"] {
            let err = parse_duration(bad).expect_err(bad);
            let message = err.to_string();
            assert!(
                message.contains("expected a positive integer followed by a unit"),
                "error for {bad:?} must name the grammar: {message}",
            );
        }
    }

    #[test]
    fn no_interval_bounds_anywhere() {
        // A one-second interval and a one-year interval both construct — no
        // minimum floor, no maximum ceiling.
        assert_eq!(parse_duration("1s").unwrap(), Duration::from_secs(1));
        assert_eq!(
            parse_duration("365d").unwrap(),
            Duration::from_hours(365 * 24)
        );
    }

    // ----- Every -----------------------------------------------------------

    #[test]
    fn every_next_fire_is_strictly_after_by_exactly_the_interval() {
        let after = Utc.with_ymd_and_hms(2026, 7, 3, 12, 0, 0).unwrap();
        let spec = ScheduleSpec::Every {
            duration: Duration::from_hours(36),
        };
        let next = spec.next_fire_after(after).unwrap();
        assert_eq!(next, after + chrono::TimeDelta::seconds(36 * 3_600));
        assert!(next > after);
    }

    #[test]
    fn every_one_second_and_every_year_both_construct() {
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let one_s = ScheduleSpec::Every {
            duration: Duration::from_secs(1),
        };
        let one_y = ScheduleSpec::Every {
            duration: Duration::from_hours(365 * 24),
        };
        assert_eq!(
            one_s.next_fire_after(after).unwrap(),
            after + chrono::TimeDelta::seconds(1)
        );
        assert_eq!(
            one_y.next_fire_after(after).unwrap(),
            after + chrono::TimeDelta::days(365)
        );
    }

    #[test]
    fn in_next_fire_adds_the_delay() {
        let after = Utc.with_ymd_and_hms(2026, 7, 3, 9, 0, 0).unwrap();
        let spec = ScheduleSpec::In {
            duration: Duration::from_secs(90),
        };
        assert_eq!(
            spec.next_fire_after(after).unwrap(),
            after + chrono::TimeDelta::seconds(90)
        );
    }

    // ----- Cron ------------------------------------------------------------

    #[test]
    fn cron_accepts_valid_and_computes_next() {
        let after = Utc.with_ymd_and_hms(2026, 7, 3, 12, 2, 0).unwrap();
        let spec = ScheduleSpec::Cron {
            expr: "*/5 * * * *".to_string(),
        };
        let next = spec.next_fire_after(after).unwrap();
        // Next multiple-of-5 minute strictly after 12:02 is 12:05.
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 7, 3, 12, 5, 0).unwrap());
    }

    #[test]
    fn cron_rejects_garbage_with_croner_message() {
        let spec = ScheduleSpec::Cron {
            expr: "not a cron expression".to_string(),
        };
        let err = spec.next_fire_after(Utc::now()).expect_err("garbage cron");
        match err {
            ScheduleError::InvalidCron { expr, reason } => {
                assert_eq!(expr, "not a cron expression");
                assert!(!reason.is_empty(), "must carry croner's parse failure");
            }
            other => panic!("expected InvalidCron, got {other:?}"),
        }
    }

    #[test]
    fn cron_weekday_nine_am_is_utc_computed() {
        // A Saturday 00:00 UTC → next weekday-09:00 is Monday 09:00 UTC.
        let sat = Utc.with_ymd_and_hms(2026, 7, 4, 0, 0, 0).unwrap();
        let spec = ScheduleSpec::Cron {
            expr: "0 9 * * 1-5".to_string(),
        };
        let next = spec.next_fire_after(sat).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 7, 6, 9, 0, 0).unwrap());
        assert_eq!(next.weekday(), chrono::Weekday::Mon);
    }

    // ----- At (time-of-day) ------------------------------------------------

    #[test]
    fn parse_time_of_day_accepts_and_rejects() {
        assert_eq!(parse_time_of_day("09:00").unwrap(), (9, 0));
        assert_eq!(parse_time_of_day("23:59").unwrap(), (23, 59));
        for bad in ["9", "09", "24:00", "09:60", "0900", "09:0a", ":00", "09:"] {
            assert!(parse_time_of_day(bad).is_err(), "must reject {bad:?}");
        }
    }

    /// Pin a fixed +05:00 offset (no DST) so the acceptance is deterministic
    /// regardless of the test machine's zone: at 08:59 local the next 09:00
    /// is within the minute; at 09:01 local it rolls to tomorrow 09:00.
    #[test]
    fn at_time_of_day_next_occurrence_in_fixed_offset() {
        let offset = FixedOffset::east_opt(5 * 3_600).unwrap();

        let at_0859 = offset
            .with_ymd_and_hms(2026, 7, 3, 8, 59, 0)
            .unwrap()
            .with_timezone(&Utc);
        let next = next_time_of_day(9, 0, at_0859, &offset).unwrap();
        assert_eq!(
            next,
            offset
                .with_ymd_and_hms(2026, 7, 3, 9, 0, 0)
                .unwrap()
                .with_timezone(&Utc),
            "08:59 local → today 09:00 local (within the minute)",
        );
        assert!(next - at_0859 <= chrono::TimeDelta::minutes(1));

        let at_0901 = offset
            .with_ymd_and_hms(2026, 7, 3, 9, 1, 0)
            .unwrap()
            .with_timezone(&Utc);
        let next = next_time_of_day(9, 0, at_0901, &offset).unwrap();
        assert_eq!(
            next,
            offset
                .with_ymd_and_hms(2026, 7, 4, 9, 0, 0)
                .unwrap()
                .with_timezone(&Utc),
            "09:01 local → tomorrow 09:00 local",
        );
    }

    /// DST boundary — the fall-back fold: a local time that occurs twice
    /// resolves to the earliest of the two instants, never the later one and
    /// never a panic.
    #[test]
    fn dst_fold_resolves_to_earliest_instant() {
        let east = FixedOffset::east_opt(2 * 3_600).unwrap();
        let west = FixedOffset::east_opt(3_600).unwrap();
        let earliest = east.with_ymd_and_hms(2026, 10, 25, 2, 30, 0).unwrap();
        let latest = west.with_ymd_and_hms(2026, 10, 25, 2, 30, 0).unwrap();
        let picked = pick_from_local_result(LocalResult::Ambiguous(earliest, latest))
            .expect("ambiguous resolves");
        assert_eq!(picked, earliest.with_timezone(&Utc));
        assert!(picked < latest.with_timezone(&Utc));
    }

    /// DST boundary — the spring-forward gap: a local time that does not
    /// exist yields no instant for that day, so the day-walk advances rather
    /// than fabricating one.
    #[test]
    fn dst_gap_yields_none_and_search_advances_a_day() {
        assert!(pick_from_local_result::<Utc>(LocalResult::None).is_none());

        // A zone that has no valid local time at 02:30 on 2026-03-08 (a
        // spring-forward gap) must resolve the *next* day's 02:30 instead of
        // erroring — modelled here with a stub zone whose 02:30 is a gap on
        // that one date.
        let after = Utc.with_ymd_and_hms(2026, 3, 8, 0, 0, 0).unwrap();
        let zone = GapZone;
        let next = next_time_of_day(2, 30, after, &zone).unwrap();
        assert_eq!(
            next.date_naive(),
            NaiveDate::from_ymd_opt(2026, 3, 9).unwrap(),
            "the gap day is skipped to the next valid 02:30",
        );
    }

    /// A minimal timezone whose 02:30 local time does not exist on
    /// 2026-03-08 (a spring-forward gap), used to prove the day-walk in
    /// [`next_time_of_day`] advances past a gap. Every other local time
    /// resolves at a fixed +00:00 offset.
    #[derive(Clone, Copy)]
    struct GapZone;

    impl TimeZone for GapZone {
        type Offset = FixedOffset;

        fn from_offset(_offset: &Self::Offset) -> Self {
            Self
        }

        fn offset_from_local_date(&self, _local: &NaiveDate) -> LocalResult<Self::Offset> {
            LocalResult::Single(FixedOffset::east_opt(0).unwrap())
        }

        fn offset_from_local_datetime(
            &self,
            local: &chrono::NaiveDateTime,
        ) -> LocalResult<Self::Offset> {
            use chrono::Timelike;
            let is_gap = local.date() == NaiveDate::from_ymd_opt(2026, 3, 8).unwrap()
                && local.hour() == 2
                && local.minute() == 30;
            if is_gap {
                LocalResult::None
            } else {
                LocalResult::Single(FixedOffset::east_opt(0).unwrap())
            }
        }

        fn offset_from_utc_date(&self, _utc: &NaiveDate) -> Self::Offset {
            FixedOffset::east_opt(0).unwrap()
        }

        fn offset_from_utc_datetime(&self, _utc: &chrono::NaiveDateTime) -> Self::Offset {
            FixedOffset::east_opt(0).unwrap()
        }
    }

    // ----- records ---------------------------------------------------------

    #[test]
    fn record_new_computes_next_fire_and_is_pending() {
        let created = Utc.with_ymd_and_hms(2026, 7, 3, 9, 0, 0).unwrap();
        let record = ScheduleRecord::new(
            Uuid::new_v4(),
            ScheduleSpec::In {
                duration: Duration::from_mins(15),
            },
            "check the build".to_string(),
            Uuid::new_v4(),
            created,
        )
        .unwrap();
        assert_eq!(record.next_fire, created + chrono::TimeDelta::seconds(900));
        assert!(record.is_pending());
        assert!(!record.late);
    }

    /// Finding 5: a zero-length `In`/`Every` is constructible programmatically
    /// (public fields bypass the tool grammar), so `ScheduleRecord::new`
    /// rejects it at the type boundary rather than admitting a spec that names
    /// its own creation instant (and, for `Every`, busy-spins).
    #[test]
    fn record_new_rejects_zero_in_and_every_at_the_type_boundary() {
        let created = Utc.with_ymd_and_hms(2026, 7, 3, 9, 0, 0).unwrap();
        let agent = Uuid::new_v4();
        for (spec, expected_kind) in [
            (
                ScheduleSpec::In {
                    duration: Duration::ZERO,
                },
                "in",
            ),
            (
                ScheduleSpec::Every {
                    duration: Duration::ZERO,
                },
                "every",
            ),
        ] {
            let err = ScheduleRecord::new(Uuid::new_v4(), spec, "x".to_string(), agent, created)
                .expect_err("a zero duration must be rejected");
            match err {
                ScheduleError::ZeroDuration { kind } => assert_eq!(kind, expected_kind),
                other => panic!("expected ZeroDuration, got {other:?}"),
            }
        }
    }

    /// Finding 5 (doc exception): `AtSpec::Instant` returns its fixed instant
    /// even when that instant is at or before `after` — an explicit absolute
    /// target fires once, immediately, when already past (the documented
    /// exception to "strictly after `after`").
    #[test]
    fn at_instant_returns_its_target_even_when_already_past() {
        let past = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 7, 3, 12, 0, 0).unwrap();
        let spec = ScheduleSpec::At {
            at: AtSpec::Instant { instant: past },
        };
        assert_eq!(
            spec.next_fire_after(after).unwrap(),
            past,
            "an explicit past Instant returns verbatim (fires immediately), not a future \
             instant",
        );
    }

    #[test]
    fn kind_label_and_recurring_flags() {
        let in_spec = ScheduleSpec::In {
            duration: Duration::from_secs(1),
        };
        let at_spec = ScheduleSpec::At {
            at: AtSpec::TimeOfDay { hour: 9, minute: 0 },
        };
        let every = ScheduleSpec::Every {
            duration: Duration::from_secs(1),
        };
        let cron = ScheduleSpec::Cron {
            expr: "* * * * *".to_string(),
        };
        assert_eq!(in_spec.kind_label(), "in");
        assert_eq!(at_spec.kind_label(), "at");
        assert_eq!(every.kind_label(), "every");
        assert_eq!(cron.kind_label(), "cron");
        assert!(!in_spec.is_recurring());
        assert!(!at_spec.is_recurring());
        assert!(every.is_recurring());
        assert!(cron.is_recurring());
    }

    #[test]
    fn spec_serde_roundtrips_every_kind() {
        let specs = [
            ScheduleSpec::In {
                duration: Duration::from_secs(90),
            },
            ScheduleSpec::At {
                at: AtSpec::TimeOfDay {
                    hour: 9,
                    minute: 30,
                },
            },
            ScheduleSpec::At {
                at: AtSpec::Instant {
                    instant: Utc.with_ymd_and_hms(2026, 7, 3, 9, 0, 0).unwrap(),
                },
            },
            ScheduleSpec::Every {
                duration: Duration::from_hours(1),
            },
            ScheduleSpec::Cron {
                expr: "0 9 * * 1-5".to_string(),
            },
        ];
        for spec in specs {
            let json = serde_json::to_string(&spec).unwrap();
            let back: ScheduleSpec = serde_json::from_str(&json).unwrap();
            assert_eq!(back, spec, "spec must round-trip: {json}");
        }
    }
}
