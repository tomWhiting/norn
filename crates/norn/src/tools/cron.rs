//! `CronTool` — model-facing in-session scheduling: schedule, list, cancel.
//!
//! Resolves the agent's [`ScheduleHandle`] (installed by
//! [`arm_schedule_executor`](crate::schedule::arm_schedule_executor) at
//! assembly) and drives the [`ScheduleStore`](crate::schedule::ScheduleStore)
//! the live executor watches. Persistence ordering is created-before-armed
//! and cancelled-before-removed: the lifecycle event is appended to the
//! owning agent's event store before the in-memory store changes, so a crash
//! can never lose an accepted schedule or resurrect a cancelled one.
//!
//! Owner ruling DECISIONS §0.6(e): no cap on schedule count, no bounds on
//! intervals. Timezone split (RULED-AS-FLAGGED): `at: "HH:MM"` resolves in
//! the host's local timezone; `cron` expressions evaluate in UTC.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::ToolError;
use crate::schedule::{
    AtSpec, ScheduleHandle, ScheduleLifecycle, ScheduleRecord, ScheduleSpec, append_schedule_event,
    parse_duration, parse_time_of_day,
};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Stable tool name for in-session scheduling.
pub const CRON_TOOL_NAME: &str = "cron";

/// In-session schedule management: schedule, list, cancel.
pub struct CronTool;

impl CronTool {
    /// Constructs the tool.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for CronTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct CronArgs {
    op: CronOp,
    #[serde(rename = "in")]
    r#in: Option<String>,
    at: Option<String>,
    every: Option<String>,
    cron: Option<String>,
    message: Option<String>,
    id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CronOp {
    Schedule,
    List,
    Cancel,
}

fn invalid_arguments(reason: String, detail: serde_json::Value) -> ToolOutput {
    ToolOutput::failure(
        ToolErrorPayload::new(ToolErrorKind::InvalidArguments, reason).with_detail(detail),
    )
}

/// Resolve the `schedule` op's mutually exclusive kind arguments into a
/// [`ScheduleSpec`], or a model-facing failure.
fn resolve_spec(args: &CronArgs) -> Result<ScheduleSpec, Box<ToolOutput>> {
    let provided: Vec<&str> = [
        ("in", args.r#in.is_some()),
        ("at", args.at.is_some()),
        ("every", args.every.is_some()),
        ("cron", args.cron.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect();
    if provided.len() != 1 {
        return Err(Box::new(invalid_arguments(
            format!(
                "op \"schedule\" requires exactly one of \"in\", \"at\", \"every\", or \
                 \"cron\" — they are mutually exclusive; got {}",
                if provided.is_empty() {
                    "none of them".to_string()
                } else {
                    format!("{provided:?}")
                },
            ),
            serde_json::json!({ "provided": provided }),
        )));
    }

    let spec_error = |error: crate::schedule::ScheduleError| {
        Box::new(invalid_arguments(
            error.to_string(),
            serde_json::json!({ "argument": provided[0] }),
        ))
    };
    if let Some(input) = args.r#in.as_deref() {
        return parse_duration(input)
            .map(|duration| ScheduleSpec::In { duration })
            .map_err(spec_error);
    }
    if let Some(input) = args.every.as_deref() {
        return parse_duration(input)
            .map(|duration| ScheduleSpec::Every { duration })
            .map_err(spec_error);
    }
    if let Some(input) = args.at.as_deref() {
        // An explicit RFC 3339 instant wins; otherwise HH:MM host-local.
        if let Ok(instant) = DateTime::parse_from_rfc3339(input) {
            return Ok(ScheduleSpec::At {
                at: AtSpec::Instant {
                    instant: instant.with_timezone(&Utc),
                },
            });
        }
        return match parse_time_of_day(input) {
            Ok((hour, minute)) => Ok(ScheduleSpec::At {
                at: AtSpec::TimeOfDay { hour, minute },
            }),
            Err(_) => Err(Box::new(invalid_arguments(
                format!(
                    "invalid \"at\" value '{input}': expected HH:MM (24-hour, resolved in \
                     the host's local timezone) or an RFC 3339 timestamp"
                ),
                serde_json::json!({ "argument": "at" }),
            ))),
        };
    }
    if let Some(expr) = args.cron.as_deref() {
        return Ok(ScheduleSpec::Cron {
            expr: expr.to_string(),
        });
    }
    // Unreachable: `provided.len() == 1` guarantees one branch above matched.
    Err(Box::new(invalid_arguments(
        "op \"schedule\" requires exactly one of \"in\", \"at\", \"every\", or \"cron\""
            .to_string(),
        serde_json::json!({ "provided": provided }),
    )))
}

fn op_schedule(args: &CronArgs, handle: &ScheduleHandle) -> Result<ToolOutput, ToolError> {
    let spec = match resolve_spec(args) {
        Ok(spec) => spec,
        Err(failure) => return Ok(*failure),
    };
    let Some(message) = args
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    else {
        return Ok(invalid_arguments(
            "op \"schedule\" requires a non-empty \"message\" — the text delivered to you \
             when the schedule fires"
                .to_string(),
            serde_json::json!({ "argument": "message" }),
        ));
    };

    let record = match ScheduleRecord::new(
        Uuid::new_v4(),
        spec,
        message.to_string(),
        handle.agent_id,
        Utc::now(),
    ) {
        Ok(record) => record,
        Err(error) => {
            // Croner parse failures land here with croner's own message.
            return Ok(invalid_arguments(
                error.to_string(),
                serde_json::json!({ "op": "schedule" }),
            ));
        }
    };

    // Created-before-armed: the durable record lands before the executor can
    // observe the entry, so an accepted schedule survives any crash after
    // this point.
    if let Err(error) = append_schedule_event(
        &handle.event_store,
        &ScheduleLifecycle::Created {
            record: record.clone(),
        },
    ) {
        return Err(ToolError::ExecutionFailed {
            reason: format!(
                "failed to persist schedule.created for schedule {}: {error}",
                record.id
            ),
        });
    }
    let response = serde_json::json!({
        "scheduled": true,
        "id": record.id,
        "kind": record.spec.kind_label(),
        "message": record.message,
        "next_fire": record.next_fire,
    });
    handle.store.insert(record);
    Ok(ToolOutput::success(response))
}

fn op_list(handle: &ScheduleHandle) -> ToolOutput {
    let mut pending = handle.store.list_pending();
    pending.sort_by_key(|record| record.next_fire);
    let schedules: Vec<serde_json::Value> = pending
        .iter()
        .map(|record| {
            serde_json::json!({
                "id": record.id,
                "kind": record.spec.kind_label(),
                "message": record.message,
                "next_fire": record.next_fire,
                "late": record.late,
            })
        })
        .collect();
    ToolOutput::success(serde_json::json!({
        "count": schedules.len(),
        "schedules": schedules,
    }))
}

fn op_cancel(args: &CronArgs, handle: &ScheduleHandle) -> Result<ToolOutput, ToolError> {
    let Some(id_str) = args.id.as_deref() else {
        return Ok(invalid_arguments(
            "op \"cancel\" requires \"id\" — the schedule id returned by op \"schedule\""
                .to_string(),
            serde_json::json!({ "argument": "id" }),
        ));
    };
    let Ok(id) = Uuid::parse_str(id_str) else {
        return Ok(invalid_arguments(
            format!("invalid schedule id '{id_str}': expected a UUID"),
            serde_json::json!({ "argument": "id" }),
        ));
    };
    if handle.store.get(id).is_none() {
        return Ok(ToolOutput::failure(
            ToolErrorPayload::new(
                ToolErrorKind::NotFound,
                format!(
                    "no pending schedule with id {id}: it is unknown, already fired out, \
                     or already cancelled"
                ),
            )
            .with_detail(serde_json::json!({ "id": id })),
        ));
    }
    // Cancelled-before-removed: persist first so a crash between the two
    // steps re-cancels on rebuild instead of resurrecting the schedule.
    if let Err(error) = append_schedule_event(
        &handle.event_store,
        &ScheduleLifecycle::Cancelled {
            id,
            cancelled_at: Utc::now(),
        },
    ) {
        return Err(ToolError::ExecutionFailed {
            reason: format!("failed to persist schedule.cancelled for schedule {id}: {error}"),
        });
    }
    let removed = handle.store.cancel(id);
    if !removed {
        // The executor fired the one-shot between our existence check and
        // the cancel. The event log now carries both fired and cancelled;
        // rebuild treats it as terminal either way. Report honestly.
        return Ok(ToolOutput::failure(
            ToolErrorPayload::new(
                ToolErrorKind::NotFound,
                format!("schedule {id} completed before the cancellation was applied"),
            )
            .with_detail(serde_json::json!({ "id": id })),
        ));
    }
    Ok(ToolOutput::success(serde_json::json!({
        "cancelled": true,
        "id": id,
    })))
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &'static str {
        CRON_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        include_str!("guidance/cron.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("guidance/cron.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["op"],
            "additionalProperties": false,
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["schedule", "list", "cancel"],
                    "description": "The operation: schedule a wake-up, list pending schedules, or cancel one."
                },
                "in": {
                    "type": "string",
                    "description": "Relative one-shot delay: positive integer + unit s/m/h/d, e.g. \"90s\", \"15m\", \"2h\", \"3d\". Mutually exclusive with at/every/cron."
                },
                "at": {
                    "type": "string",
                    "description": "Time-of-day one-shot: \"HH:MM\" (24-hour, host-local timezone) or an RFC 3339 instant. Mutually exclusive with in/every/cron."
                },
                "every": {
                    "type": "string",
                    "description": "Looping interval, same grammar as \"in\"; re-arms after each fire. Mutually exclusive with in/at/cron."
                },
                "cron": {
                    "type": "string",
                    "description": "Full 5-field cron expression (min hour day month dow), evaluated in UTC; re-arms after each fire. Mutually exclusive with in/at/every."
                },
                "message": {
                    "type": "string",
                    "description": "Required for op \"schedule\": the text delivered to you as an injected message when the schedule fires."
                },
                "id": {
                    "type": "string",
                    "description": "Required for op \"cancel\": the schedule id to cancel."
                }
            }
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Process
    }

    fn effect_for_args(&self, args: &serde_json::Value) -> ToolEffect {
        match args.get("op").and_then(serde_json::Value::as_str) {
            Some("list") => ToolEffect::ReadOnly,
            _ => ToolEffect::Process,
        }
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CronArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;
        let handle = ctx.require_extension::<ScheduleHandle>()?;
        match args.op {
            CronOp::Schedule => op_schedule(&args, &handle),
            CronOp::List => Ok(op_list(&handle)),
            CronOp::Cancel => op_cancel(&args, &handle),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use chrono::{Datelike, TimeDelta, Timelike};

    use super::*;
    use crate::schedule::{SCHEDULE_CANCELLED_EVENT_TYPE, SCHEDULE_CREATED_EVENT_TYPE};
    use crate::schedule::{ScheduleDelivery, ScheduleStore, arm_schedule_executor};
    use crate::session::events::SessionEvent;
    use crate::session::store::EventStore;

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: CRON_TOOL_NAME.to_string(),
            model_args: args,
            metadata: serde_json::Value::Null,
        }
    }

    /// Arm a fresh context and return it with its schedule handle parts.
    fn armed_ctx() -> (ToolContext, Arc<EventStore>, Uuid) {
        let ctx = ToolContext::empty();
        let agent_id = Uuid::new_v4();
        let event_store = Arc::new(EventStore::new());
        let guard = arm_schedule_executor(
            &ctx,
            Arc::new(ScheduleStore::new()),
            ScheduleDelivery {
                agent_id,
                inbound: None,
                pending: Some(Arc::new(crate::agent::PendingAgentMessages::new())),
                event_store: Arc::clone(&event_store),
                registry: None,
                wake_registry: None,
            },
        );
        // The guard's executor task is irrelevant to tool tests; keep the
        // extension installed but let the task die with the guard.
        drop(guard);
        (ctx, event_store, agent_id)
    }

    async fn run(ctx: &ToolContext, args: serde_json::Value) -> ToolOutput {
        CronTool::new()
            .execute(&envelope_for(args), ctx)
            .await
            .expect("cron executes")
    }

    #[tokio::test]
    async fn schedule_in_returns_id_and_next_fire() {
        let (ctx, event_store, _) = armed_ctx();
        let before = Utc::now();
        let out = run(
            &ctx,
            serde_json::json!({ "op": "schedule", "in": "15m", "message": "check the build" }),
        )
        .await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert!(out.content["id"].as_str().is_some());
        assert_eq!(out.content["kind"], "in");
        let next_fire: chrono::DateTime<Utc> =
            serde_json::from_value(out.content["next_fire"].clone()).unwrap();
        let expected = before + TimeDelta::minutes(15);
        assert!(
            (next_fire - expected).abs() < TimeDelta::seconds(5),
            "next_fire ≈ now+15m, got {next_fire}",
        );
        // Created-before-armed: the durable record is on the event store.
        assert!(event_store.events().iter().any(|e| matches!(
            e,
            SessionEvent::Custom { event_type, .. } if event_type == SCHEDULE_CREATED_EVENT_TYPE
        )));
    }

    #[tokio::test]
    async fn schedule_with_two_kinds_fails_naming_mutual_exclusion() {
        let (ctx, _, _) = armed_ctx();
        let out = run(
            &ctx,
            serde_json::json!({
                "op": "schedule", "in": "15m", "cron": "* * * * *", "message": "x",
            }),
        )
        .await;
        assert!(out.is_error());
        let error = out.error().expect("error payload");
        assert_eq!(error.kind, ToolErrorKind::InvalidArguments);
        assert!(
            error.message.contains("mutually exclusive"),
            "must name the conflict: {}",
            error.message,
        );
    }

    #[tokio::test]
    async fn schedule_with_no_kind_fails_likewise() {
        let (ctx, _, _) = armed_ctx();
        let out = run(
            &ctx,
            serde_json::json!({ "op": "schedule", "message": "x" }),
        )
        .await;
        assert!(out.is_error());
        let error = out.error().expect("error payload");
        assert_eq!(error.kind, ToolErrorKind::InvalidArguments);
        assert!(error.message.contains("exactly one of"));
    }

    #[tokio::test]
    async fn schedule_requires_message() {
        let (ctx, _, _) = armed_ctx();
        let out = run(&ctx, serde_json::json!({ "op": "schedule", "in": "5m" })).await;
        assert!(out.is_error());
        assert!(
            out.error()
                .expect("error payload")
                .message
                .contains("message"),
        );
    }

    #[tokio::test]
    async fn schedule_cron_weekday_returns_utc_computed_fire() {
        let (ctx, _, _) = armed_ctx();
        let out = run(
            &ctx,
            serde_json::json!({
                "op": "schedule", "cron": "0 9 * * 1-5", "message": "weekday triage",
            }),
        )
        .await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["kind"], "cron");
        let next_fire: chrono::DateTime<Utc> =
            serde_json::from_value(out.content["next_fire"].clone()).unwrap();
        assert!(next_fire > Utc::now());
        assert_eq!(next_fire.hour(), 9, "09:00 UTC from croner");
        assert_eq!(next_fire.minute(), 0);
        let weekday = next_fire.weekday().number_from_monday();
        assert!((1..=5).contains(&weekday), "a weekday, got {weekday}");
    }

    #[tokio::test]
    async fn schedule_rejects_garbage_cron_with_croner_message() {
        let (ctx, _, _) = armed_ctx();
        let out = run(
            &ctx,
            serde_json::json!({ "op": "schedule", "cron": "not a cron", "message": "x" }),
        )
        .await;
        assert!(out.is_error());
        let error = out.error().expect("error payload");
        assert_eq!(error.kind, ToolErrorKind::InvalidArguments);
        assert!(
            error.message.contains("invalid cron expression"),
            "carries the croner failure: {}",
            error.message,
        );
    }

    #[tokio::test]
    async fn schedule_at_accepts_rfc3339_and_hhmm_and_rejects_garbage() {
        let (ctx, _, _) = armed_ctx();
        let out = run(
            &ctx,
            serde_json::json!({
                "op": "schedule", "at": "2099-01-01T09:00:00Z", "message": "future instant",
            }),
        )
        .await;
        assert!(!out.is_error(), "{:?}", out.content);
        let next_fire: chrono::DateTime<Utc> =
            serde_json::from_value(out.content["next_fire"].clone()).unwrap();
        assert_eq!(next_fire.year(), 2099);

        let out = run(
            &ctx,
            serde_json::json!({ "op": "schedule", "at": "09:00", "message": "morning" }),
        )
        .await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["kind"], "at");

        let out = run(
            &ctx,
            serde_json::json!({ "op": "schedule", "at": "25:99", "message": "x" }),
        )
        .await;
        assert!(out.is_error());
        assert!(
            out.error().expect("error").message.contains("HH:MM"),
            "names the expected grammar",
        );
    }

    #[tokio::test]
    async fn schedule_rejects_bad_duration_with_grammar() {
        let (ctx, _, _) = armed_ctx();
        for bad in ["15", "2 hours", "-5m", "0x"] {
            let out = run(
                &ctx,
                serde_json::json!({ "op": "schedule", "in": bad, "message": "x" }),
            )
            .await;
            assert!(out.is_error(), "must reject {bad:?}");
            assert!(
                out.error()
                    .expect("error")
                    .message
                    .contains("expected a positive integer followed by a unit"),
                "names the grammar for {bad:?}",
            );
        }
    }

    #[tokio::test]
    async fn list_and_cancel_lifecycle() {
        let (ctx, event_store, _) = armed_ctx();
        let mut ids = Vec::new();
        for (kind, value) in [("in", "10m"), ("every", "1h"), ("cron", "0 9 * * *")] {
            let out = run(
                &ctx,
                serde_json::json!({ "op": "schedule", kind: value, "message": "m" }),
            )
            .await;
            assert!(!out.is_error(), "{:?}", out.content);
            ids.push(out.content["id"].as_str().unwrap().to_string());
        }

        let out = run(&ctx, serde_json::json!({ "op": "list" })).await;
        assert_eq!(out.content["count"], 3, "all three listed");

        let out = run(&ctx, serde_json::json!({ "op": "cancel", "id": ids[0] })).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["cancelled"], true);
        assert!(
            event_store.events().iter().any(|e| matches!(
                e,
                SessionEvent::Custom { event_type, .. }
                    if event_type == SCHEDULE_CANCELLED_EVENT_TYPE
            )),
            "the cancellation persisted",
        );

        let out = run(&ctx, serde_json::json!({ "op": "list" })).await;
        assert_eq!(out.content["count"], 2, "two remain after cancel");

        let out = run(&ctx, serde_json::json!({ "op": "cancel", "id": ids[0] })).await;
        assert!(out.is_error());
        assert_eq!(
            out.error().expect("error payload").kind,
            ToolErrorKind::NotFound,
            "cancelling the same id again reports NotFound",
        );
    }

    #[tokio::test]
    async fn cancel_unknown_id_reports_not_found() {
        let (ctx, _, _) = armed_ctx();
        let out = run(
            &ctx,
            serde_json::json!({ "op": "cancel", "id": Uuid::new_v4() }),
        )
        .await;
        assert!(out.is_error());
        assert_eq!(out.error().expect("error").kind, ToolErrorKind::NotFound);
    }

    #[tokio::test]
    async fn missing_extension_is_a_typed_error() {
        let ctx = ToolContext::empty();
        let err = CronTool::new()
            .execute(&envelope_for(serde_json::json!({ "op": "list" })), &ctx)
            .await
            .expect_err("no ScheduleHandle installed");
        assert!(matches!(err, ToolError::MissingExtension { .. }));
    }

    #[test]
    fn guidance_documents_kinds_grammar_and_delivery() {
        let tool = CronTool::new();
        let description = tool.description();
        for needle in ["in N", "cron", "injected"] {
            assert!(
                description.contains(needle),
                "description must mention {needle:?}",
            );
        }
        let usage = tool.usage_guidance().expect("usage guidance");
        for needle in [
            "\"90s\"",
            "\"15m\"",
            "\"2h\"",
            "\"3d\"",
            "host's local timezone",
            "UTC",
            "norn:cron",
            "backfilled",
        ] {
            assert!(usage.contains(needle), "usage must mention {needle:?}");
        }
    }

    #[test]
    fn list_op_is_read_only_effect() {
        let tool = CronTool::new();
        assert_eq!(
            tool.effect_for_args(&serde_json::json!({ "op": "list" })),
            ToolEffect::ReadOnly,
        );
        assert_eq!(
            tool.effect_for_args(&serde_json::json!({ "op": "schedule" })),
            ToolEffect::Process,
        );
    }
}
