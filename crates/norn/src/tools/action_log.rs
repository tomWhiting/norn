//! `action_log` — queryable memory of the agent's own tool calls.
//!
//! The tool is a thin read-only view over the session
//! [`ActionLog`](crate::session::action_log::ActionLog), published on the
//! shared [`ToolContext`] as an [`Arc<ActionLog>`] extension by the agent
//! builder. It never holds the log itself, mirroring `tool_search`'s use of
//! a context-published catalogue.
//!
//! Six query modes are supported:
//!
//! * `list` — Level 1 compact summaries (via
//!   [`ActionLogEntry::compact_json`](crate::session::action_log::ActionLogEntry::compact_json)),
//!   optionally scoped by a [`ActionLogFilter`]. Cheap to call frequently.
//! * `detail` — Level 2 data (full output, arguments, duration, follow-ups)
//!   for one `call_id`, via [`ActionLog::get_detail`].
//! * `context` — Level 3 data (Level 2 plus before-content and any recorded
//!   post-validate outcome) for one `call_id`, via [`ActionLog::get_context`].
//! * `mutations` — the session mutation ledger: every file the agent changed,
//!   each with a lazily-evaluated revert status, via
//!   [`ActionLog::mutation_entries`]. Optionally scoped to a single path with
//!   the filter's `file` field.
//! * `follow_ups` — unexpired follow-up actions across the session, with
//!   expiry checked at query time, via [`ActionLog::unexpired_follow_ups`].
//!   `filter.tool` scopes to the registering tool and `filter.outcome` to the
//!   original call's outcome.
//! * `events` — the session's Custom audit events, via
//!   [`ActionLog::custom_events`]: typed subagent lifecycle records
//!   (`subagent.started` / `subagent.completed`), the Wave 3 inter-agent
//!   message audit trail (`agent_message.sent` / `agent_message.delivered`
//!   / `agent_message.queued` / `agent_message.dequeued`),
//!   and any embedder-defined event types — payloads verbatim (they are the
//!   serde-stable audit contract). `filter.event_type` narrows to one type
//!   and `filter.last` bounds the result; the other filter fields describe
//!   tool calls and are rejected with a typed failure, never ignored.
//!
//! # Scope (federated queries over the agent subtree)
//!
//! Every agent — root, fork, spawn — has its own [`ActionLog`], registered
//! in the session-wide
//! [`ActionLogTree`](crate::session::action_log_tree::ActionLogTree). The
//! optional `scope` argument widens `list` / `detail` / `context` /
//! `mutations` / `events` from the caller's own log (the default — exactly the
//! pre-scope behaviour) to `children` (self + direct children), `all` (the
//! whole subtree), or one specific descendant by registry path or UUID.
//! Federated `list` results interleave by timestamp and label every entry
//! with its agent; labels are registry ground truth — the agent's path,
//! `"root"` for the root agent, or the bare UUID when no registry record
//! exists. The boundary is strict: scope never reaches upward, so a child
//! can query its own subtree but never its parent or a sibling. Finished
//! and reclaimed children stay queryable for the session — the tree holds
//! their logs independently of registry reclamation, consistent with
//! registry tombstones.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::ToolError;
use crate::session::action_log::ActionLog;
use crate::session::action_log_scope::{
    ActionLogFilter, ScopedLog, collect_labeled_entries, collect_labeled_events, collect_mutations,
    find_context, find_detail,
};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::action_log_scope_resolve::{
    Scope, agents_legend, parse_scope, resolve_scoped_logs,
};

/// Queryable view over the session action log.
pub struct ActionLogTool;

impl ActionLogTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ActionLogTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Model-supplied arguments for an `action_log` call.
#[derive(Debug, Deserialize)]
struct ActionLogArgs {
    /// One of `list`, `detail`, `context`, `mutations`, `follow_ups`,
    /// `events`.
    query: String,
    /// Optional scoping filter for `list`, `mutations`, `follow_ups`,
    /// and `events` queries.
    #[serde(default)]
    filter: Option<ActionLogFilter>,
    /// Tool-call id required by `detail` and `context`; ignored by `list`.
    #[serde(default)]
    call_id: Option<String>,
    /// Which agent's logs to query: absent / `self`, `children`, `all`,
    /// or a specific agent path / UUID within the caller's subtree.
    #[serde(default)]
    scope: Option<String>,
}

/// Build the structured error [`ToolOutput`] for a missing `call_id`.
fn missing_call_id(query: &str) -> ToolOutput {
    ToolOutput::failure(
        ToolErrorPayload::new(
            ToolErrorKind::InvalidArguments,
            format!("call_id required for {query} query"),
        )
        .with_detail(serde_json::json!({ "query": query })),
    )
}

/// Build the structured error [`ToolOutput`] for a filter field supplied
/// to a query it does not apply to. Inapplicable filters fail loudly —
/// silently ignoring one would report results the caller believes are
/// narrowed when they are not.
fn inapplicable_filter(query: &str, field: &str, hint: &str) -> ToolOutput {
    ToolOutput::failure(
        ToolErrorPayload::new(
            ToolErrorKind::InvalidArguments,
            format!("filter.{field} does not apply to the {query} query; {hint}"),
        )
        .with_detail(serde_json::json!({ "query": query, "field": field })),
    )
}

/// Build the structured error [`ToolOutput`] for an unknown `call_id`.
fn unknown_call_id(call_id: &str) -> ToolOutput {
    ToolOutput::failure_with_content(
        serde_json::json!({ "call_id": call_id }),
        ToolErrorPayload::new(ToolErrorKind::NotFound, "tool_call_id not found")
            .with_detail(serde_json::json!({ "call_id": call_id })),
    )
}

/// Build the `follow_ups` query response: every unexpired follow-up action,
/// optionally scoped by `filter.tool` (the registering tool's name) and
/// `filter.outcome` (the original call's outcome).
///
/// `current_turn_id` is `None`: the tool envelope/context do not yet carry a
/// turn id, so [`ExpiryCondition::TurnScoped`](crate::tool::follow_up::ExpiryCondition::TurnScoped)
/// follow-ups are treated as expired until the runtime threads turn state
/// through (out of scope for NTA-004). File- and never-scoped follow-ups are
/// unaffected. Paths are resolved against the agent working directory so
/// expiry hashing matches every other file-touching tool.
fn query_follow_ups(
    action_log: &ActionLog,
    filter: Option<&ActionLogFilter>,
    ctx: &ToolContext,
) -> ToolOutput {
    let resolve = |path: &std::path::Path| ctx.resolve_path(path);
    let mut follow_ups = action_log.unexpired_follow_ups(resolve, None);

    if let Some(filter) = filter {
        if let Some(tool) = &filter.tool {
            follow_ups.retain(|f| f.registering_tool == *tool);
        }
        if let Some(outcome) = &filter.outcome {
            follow_ups.retain(|f| f.outcome.tag() == outcome.as_str());
        }
    }

    let actions: Vec<serde_json::Value> = follow_ups
        .iter()
        .map(|f| {
            serde_json::json!({
                "tool_call_id": f.tool_call_id,
                "action": f.action.action,
                "description": f.action.description,
                "tool": f.action.tool,
                "expires": f.action.expires.model_facing(),
            })
        })
        .collect();

    ToolOutput::success(serde_json::json!({
        "query": "follow_ups",
        "count": actions.len(),
        "actions": actions,
    }))
}

/// Run a query over the resolved scope.
///
/// `scope_echo` is `None` for the default / `"self"` scope, producing
/// exactly the pre-scope output shapes (no `scope` echo, no `agents`
/// legend, no per-entry labels — zero breaking change). With an explicit
/// non-self scope it is the supplied scope string, and the same shapes
/// gain `scope`, a per-agent `agents` legend on `list`/`mutations`, and an
/// `agent` label on every entry / resolved call.
///
/// `scoped` is never empty: the caller's own log is always first; for a
/// specific-agent scope it is that agent's log alone.
fn run_query(
    args: &ActionLogArgs,
    scope_echo: Option<&str>,
    scoped: &[ScopedLog],
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let finish = |mut output: serde_json::Value| {
        if let Some(echo) = scope_echo {
            output["scope"] = serde_json::json!(echo);
        }
        Ok(ToolOutput::success(output))
    };
    // `filter.event_type` describes Custom session events; on any
    // recognized tool-call query it is a typed failure, never a silent
    // no-op (unknown queries keep their own unknown-query error below).
    if args.query != "events"
        && matches!(
            args.query.as_str(),
            "list" | "detail" | "context" | "mutations" | "follow_ups"
        )
        && args.filter.as_ref().is_some_and(|f| f.event_type.is_some())
    {
        return Ok(inapplicable_filter(
            &args.query,
            "event_type",
            "it narrows the events query only",
        ));
    }
    match args.query.as_str() {
        "list" => {
            // A single scoped log (self, or one specific agent) keeps
            // its insertion order — the legacy output exactly; only a
            // multi-log scope merges by timestamp (see
            // `collect_labeled_entries` for why the distinction matters
            // on a non-monotonic clock).
            let merged = collect_labeled_entries(scoped);
            let filtered = match &args.filter {
                Some(filter) => filter.apply_labeled(merged),
                None => merged,
            };
            let entries: Vec<serde_json::Value> = filtered
                .iter()
                .map(|le| {
                    let mut value = le.entry.compact_json();
                    if scope_echo.is_some() {
                        value["agent"] = serde_json::json!(scoped[le.agent_idx].label);
                    }
                    value
                })
                .collect();
            let mut output = serde_json::json!({
                "query": "list",
                "count": entries.len(),
                "entries": entries,
            });
            if scope_echo.is_some() {
                output["agents"] = serde_json::Value::Array(agents_legend(scoped));
            }
            finish(output)
        }
        "detail" => {
            let Some(call_id) = args.call_id.as_deref() else {
                return Ok(missing_call_id("detail"));
            };
            match find_detail(scoped, call_id) {
                Some((idx, detail)) => {
                    let mut output = serde_json::json!({
                        "query": "detail",
                        "entry": detail.entry,
                        "output": detail.output,
                        "args": detail.args,
                        "duration_ms": detail.duration_ms,
                        "follow_ups": detail.follow_ups,
                    });
                    if scope_echo.is_some() {
                        output["agent"] = serde_json::json!(scoped[idx].label);
                    }
                    finish(output)
                }
                None => Ok(unknown_call_id(call_id)),
            }
        }
        "context" => {
            let Some(call_id) = args.call_id.as_deref() else {
                return Ok(missing_call_id("context"));
            };
            match find_context(scoped, call_id) {
                Some((idx, context)) => {
                    let mut output = serde_json::json!({
                        "query": "context",
                        "entry": context.detail.entry,
                        "output": context.detail.output,
                        "args": context.detail.args,
                        "duration_ms": context.detail.duration_ms,
                        "follow_ups": context.detail.follow_ups,
                        "before_content": context.before_content,
                        "post_validate_outcome": context.post_validate_outcome,
                    });
                    if scope_echo.is_some() {
                        output["agent"] = serde_json::json!(scoped[idx].label);
                    }
                    finish(output)
                }
                None => Ok(unknown_call_id(call_id)),
            }
        }
        "mutations" => {
            let file_filter = args.filter.as_ref().and_then(|f| f.file.as_ref());
            let mut entries = Vec::new();
            for (idx, entry) in collect_mutations(scoped) {
                if let Some(file) = file_filter {
                    // Ledger entries store paths resolved against the
                    // agent working directory, so resolve a relative
                    // filter the same way (raw match kept for absolute /
                    // already-resolved input). With a cross-agent scope
                    // that resolution uses the CALLER's directory only —
                    // the schema directs cross-agent filters to absolute
                    // paths for exactly this reason.
                    let raw = PathBuf::from(file);
                    let resolved = ctx.resolve_path(&raw);
                    if entry.file_path != raw && entry.file_path != resolved {
                        continue;
                    }
                }
                let mut value =
                    serde_json::to_value(&entry).map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("serialising mutation entry: {e}"),
                    })?;
                if scope_echo.is_some() {
                    value["agent"] = serde_json::json!(scoped[idx].label);
                }
                entries.push(value);
            }
            let mut output = serde_json::json!({
                "query": "mutations",
                "count": entries.len(),
                "entries": entries,
            });
            if scope_echo.is_some() {
                output["agents"] = serde_json::Value::Array(agents_legend(scoped));
            }
            finish(output)
        }
        "events" => {
            if let Some(filter) = &args.filter {
                let inapplicable = [
                    ("tool", filter.tool.is_some()),
                    ("outcome", filter.outcome.is_some()),
                    ("file", filter.file.is_some()),
                    ("since", filter.since.is_some()),
                ];
                for (field, present) in inapplicable {
                    if present {
                        return Ok(inapplicable_filter(
                            "events",
                            field,
                            "session events are not tool calls; use filter.event_type \
                             and filter.last",
                        ));
                    }
                }
            }
            let merged = collect_labeled_events(scoped);
            let filtered = match &args.filter {
                Some(filter) => filter.apply_events(merged),
                None => merged,
            };
            let events: Vec<serde_json::Value> = filtered
                .iter()
                .map(|le| {
                    let mut value = serde_json::json!({
                        "type": le.record.event_type,
                        "ts": le.record.timestamp.to_rfc3339(),
                        "data": le.record.data,
                    });
                    if scope_echo.is_some() {
                        value["agent"] = serde_json::json!(scoped[le.agent_idx].label);
                    }
                    value
                })
                .collect();
            let mut output = serde_json::json!({
                "query": "events",
                "count": events.len(),
                "events": events,
            });
            if scope_echo.is_some() {
                output["agents"] = serde_json::Value::Array(agents_legend(scoped));
            }
            finish(output)
        }
        "follow_ups" => match scope_echo {
            Some(echo) => Ok(ToolOutput::failure(
                ToolErrorPayload::new(
                    ToolErrorKind::InvalidArguments,
                    "scope is not supported for the follow_ups query; follow-up actions are \
                     executable only by the agent that registered them",
                )
                .with_detail(serde_json::json!({ "scope": echo })),
            )),
            None => Ok(query_follow_ups(&scoped[0].log, args.filter.as_ref(), ctx)),
        },
        other => Err(ToolError::ExecutionFailed {
            reason: format!(
                "unknown query '{other}': expected list, detail, context, mutations, \
                 follow_ups, or events"
            ),
        }),
    }
}

#[async_trait]
impl Tool for ActionLogTool {
    fn name(&self) -> &'static str {
        "action_log"
    }

    fn description(&self) -> &'static str {
        "Query the agent's own action log — a session-lifetime record of every \
         tool call that survives context compaction. `list` returns compact \
         Level 1 summaries (optionally filtered); `detail` returns the full \
         output, arguments, duration, and follow-ups for one call; `context` \
         adds before-content for mutations and any recorded post-validate \
         outcome; `mutations` returns the file-change ledger (one entry per \
         file you changed) with each file's revert status checked live against \
         disk, optionally scoped to one path via filter.file; `follow_ups` \
         returns the still-valid deferred actions (e.g. undo) registered by \
         earlier calls, with expiry checked at query time and optional \
         `filter.tool`/`filter.outcome` scoping; `events` returns the \
         session's typed audit events — subagent lifecycle \
         (subagent.started/completed) and inter-agent messaging \
         (agent_message.sent/delivered/queued/dequeued) — with optional \
         `filter.event_type` and `filter.last` narrowing. The optional \
         `scope` widens list/detail/context/mutations/events beyond your \
         own log: `children` (you plus your direct sub-agents), `all` \
         (your whole sub-agent subtree), or one specific sub-agent by \
         path or UUID. Federated results are labeled per agent and \
         interleaved by time; you can never query a parent's or \
         sibling's log."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Discovery
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "enum": ["list", "detail", "context", "mutations", "follow_ups", "events"],
                    "description": "list: Level 1 summaries with optional filter. detail: Level 2 data for one call_id. context: Level 3 data for one call_id. mutations: file-change ledger with live revert status, optionally scoped by filter.file. follow_ups: unexpired follow-up actions, optionally scoped by filter.tool (registering tool) and filter.outcome. events: the session's typed Custom audit events (subagent.started/completed, agent_message.sent/delivered/queued/dequeued, embedder-defined types) with payloads verbatim, optionally scoped by filter.event_type and filter.last."
                },
                "filter": {
                    "type": "object",
                    "description": "Optional scoping for list, mutations, follow_ups, and events queries. Fields combine with AND. For follow_ups, only tool (the registering tool's name) and outcome (the original call's outcome) apply. For events, only event_type and last apply — the other fields describe tool calls and are rejected.",
                    "properties": {
                        "tool": {
                            "type": "string",
                            "description": "Filter to entries from this tool name."
                        },
                        "outcome": {
                            "type": "string",
                            "enum": ["success", "error", "blocked"],
                            "description": "Filter to entries with this outcome."
                        },
                        "file": {
                            "type": "string",
                            "description": "list: entries whose summary mentions this file path. mutations: the ledger entry whose file_path equals this path exactly. Each agent's ledger records paths resolved against that agent's own working directory, so when querying mutations with a cross-agent scope use an absolute path — a relative path resolves against YOUR working directory and silently misses entries from agents working elsewhere."
                        },
                        "since": {
                            "type": "string",
                            "description": "Return only entries recorded after this tool_call_id (after it in the merged timeline when a scope spans several agents)."
                        },
                        "last": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Return only the most recent N entries."
                        },
                        "event_type": {
                            "type": "string",
                            "description": "events query only: return only Custom session events with exactly this event_type, e.g. \"agent_message.sent\", \"agent_message.delivered\", \"agent_message.queued\", \"agent_message.dequeued\", \"subagent.started\", \"subagent.completed\". Rejected on every other query."
                        }
                    },
                    "additionalProperties": false
                },
                "call_id": {
                    "type": "string",
                    "description": "Tool-call id, required for detail and context queries. With a non-self scope it is resolved across every log in the scope."
                },
                "scope": {
                    "type": "string",
                    "description": "Which agent's logs to query. Omit or \"self\" (default): your own log only — identical output shape to omitting scope. \"children\": you plus your direct sub-agents. \"all\": your entire sub-agent subtree. Or a specific sub-agent's registry path (e.g. \"/smoke/child\") or UUID, returning that agent's own log. Applies to list, detail, context, mutations, and events; not supported for follow_ups. Federated list entries interleave by timestamp and carry an \"agent\" label (registry path, \"root\" for the root agent, or bare UUID), with a per-agent \"agents\" legend (role included only when set). Scope never reaches upward: a parent's or sibling's log is not queryable. Finished/reclaimed sub-agents remain queryable for the whole session. For cross-agent mutations queries, pass filter.file as an absolute path (each agent's ledger records paths against its own working directory)."
                }
            },
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ActionLogArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;

        let action_log: Arc<ActionLog> = ctx.require_extension::<ActionLog>()?;

        let scope = parse_scope(args.scope.as_deref());
        // The default / "self" scope keeps the pre-scope output shapes:
        // no echo, no legend, no labels.
        let scope_echo = match scope {
            Scope::SelfOnly => None,
            // `parse_scope` only yields a non-self scope for `Some` raw
            // input, so the echo is always the supplied string.
            _ => args.scope.as_deref(),
        };
        let scoped = match resolve_scoped_logs(&scope, action_log, ctx) {
            Ok(scoped) => scoped,
            Err(failure) => return Ok(*failure),
        };
        run_query(&args, scope_echo, &scoped, ctx)
    }
}
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants,
    clippy::too_many_lines
)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;
    use crate::session::action_log::{CompletionRecord, Outcome};
    use crate::session::store::EventStore;
    use crate::tool::envelope::RuntimeInputs;
    use crate::tool::follow_up::{
        BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction,
    };

    fn seed(log: &ActionLog, tool: &str, id: &str, outcome: Outcome, summary_output: Value) {
        log.record_completion(CompletionRecord {
            tool_name: tool,
            tool_call_id: id,
            tool_use_description: "",
            outcome,
            output: &summary_output,
            args: json!({ "id": id }),
            duration_ms: 5,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });
    }

    fn ctx_with(log: Arc<ActionLog>) -> ToolContext {
        let ctx = ToolContext::empty();
        ctx.insert_extension(log);
        ctx
    }

    fn envelope(args: Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "self-call".to_string(),
            tool_name: "action_log".to_string(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: Value::Null,
        }
    }

    fn populated_log() -> Arc<ActionLog> {
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        seed(
            &log,
            "edit",
            "tc-1",
            Outcome::Success,
            json!({ "path": "src/a.rs", "added": 1, "removed": 0 }),
        );
        seed(
            &log,
            "read",
            "tc-2",
            Outcome::Success,
            json!({ "path": "src/b.rs", "lines": 10 }),
        );
        seed(
            &log,
            "edit",
            "tc-3",
            Outcome::Error {
                message: "boom".to_owned(),
            },
            json!({ "error": "boom" }),
        );
        log
    }

    #[tokio::test]
    async fn list_no_filter_returns_all_entries() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "list" })), &ctx)
            .await
            .unwrap();
        assert!(!out.is_error());
        assert_eq!(out.content["query"], "list");
        assert_eq!(out.content["count"], 3);
        let entries = out.content["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        // Chronological order preserved.
        assert_eq!(entries[0]["id"], "tc-1");
        assert_eq!(entries[1]["id"], "tc-2");
        assert_eq!(entries[2]["id"], "tc-3");
        // Compact short keys.
        assert!(entries[0].get("tool").is_some());
        assert!(entries[0].get("summary").is_some());
    }

    #[tokio::test]
    async fn list_tool_filter_returns_matching() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "list", "filter": { "tool": "edit" } })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["count"], 2);
        let entries = out.content["entries"].as_array().unwrap();
        assert!(entries.iter().all(|e| e["tool"] == "edit"));
    }

    #[tokio::test]
    async fn list_outcome_filter_returns_matching() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "list", "filter": { "outcome": "error" } })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["count"], 1);
        assert_eq!(out.content["entries"][0]["id"], "tc-3");
        assert_eq!(out.content["entries"][0]["outcome"], "error");
    }

    #[tokio::test]
    async fn list_file_filter_matches_summary_substring() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "list", "filter": { "file": "src/b.rs" } })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["count"], 1);
        assert_eq!(out.content["entries"][0]["id"], "tc-2");
    }

    #[tokio::test]
    async fn list_last_limits_results() {
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        for i in 0..10 {
            seed(
                &log,
                "edit",
                &format!("tc-{i}"),
                Outcome::Success,
                json!({ "path": format!("f{i}.rs"), "added": 1, "removed": 0 }),
            );
        }
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "list", "filter": { "last": 5 } })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["count"], 5);
        let entries = out.content["entries"].as_array().unwrap();
        // The most recent 5, still chronological.
        assert_eq!(entries[0]["id"], "tc-5");
        assert_eq!(entries[4]["id"], "tc-9");
    }

    #[tokio::test]
    async fn list_since_returns_entries_after_marker() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "list", "filter": { "since": "tc-1" } })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["count"], 2);
        assert_eq!(out.content["entries"][0]["id"], "tc-2");
        assert_eq!(out.content["entries"][1]["id"], "tc-3");
    }

    #[tokio::test]
    async fn list_combines_filters_with_and() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(
                    json!({ "query": "list", "filter": { "tool": "edit", "outcome": "success" } }),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["count"], 1);
        assert_eq!(out.content["entries"][0]["id"], "tc-1");
    }

    #[tokio::test]
    async fn list_empty_result_has_zero_count() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "list", "filter": { "tool": "bash" } })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error());
        assert_eq!(out.content["count"], 0);
        assert_eq!(out.content["entries"], json!([]));
    }

    #[tokio::test]
    async fn detail_success_returns_level_2() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "detail", "call_id": "tc-1" })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error());
        assert_eq!(out.content["query"], "detail");
        assert_eq!(out.content["entry"]["tool_call_id"], "tc-1");
        assert_eq!(out.content["output"]["path"], "src/a.rs");
        assert_eq!(out.content["args"]["id"], "tc-1");
        assert_eq!(out.content["duration_ms"], 5);
        assert!(out.content["follow_ups"].is_array());
    }

    #[tokio::test]
    async fn detail_missing_call_id_returns_error() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "detail" })), &ctx)
            .await
            .unwrap();
        assert!(out.is_error());
        assert_eq!(out.content["error"]["kind"], "invalid_arguments");
        assert_eq!(
            out.content["error"]["message"],
            "call_id required for detail query"
        );
    }

    #[tokio::test]
    async fn detail_unknown_call_id_returns_error() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "detail", "call_id": "nope" })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.is_error());
        assert_eq!(out.content["error"]["kind"], "not_found");
        assert_eq!(out.content["error"]["message"], "tool_call_id not found");
        assert_eq!(out.content["call_id"], "nope");
    }

    #[tokio::test]
    async fn context_mutation_includes_before_content() {
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        let mut files = HashMap::new();
        files.insert(PathBuf::from("src/a.rs"), "old".to_owned());
        let follow_up = FollowUpAction {
            action: "undo".to_owned(),
            description: "Revert".to_owned(),
            tool: "apply_patch".to_owned(),
            args: json!({}),
            args_mode: crate::tool::follow_up::FollowUpArgsMode::MergeOriginal,
            expires: ExpiryCondition::Never,
            confidence: Confidence::High,
            before_content: BeforeContentSource::StoredContent { files },
        };
        log.record_completion(CompletionRecord {
            tool_name: "edit",
            tool_call_id: "tc-m",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &json!({ "path": "src/a.rs" }),
            args: json!({}),
            duration_ms: 1,
            follow_ups: vec![follow_up],
            post_validate_outcome: None,
            level_1_only: false,
        });

        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "context", "call_id": "tc-m" })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error());
        assert_eq!(out.content["query"], "context");
        assert_eq!(out.content["before_content"]["src/a.rs"], "old");
        assert!(out.content["post_validate_outcome"].is_null());
    }

    #[tokio::test]
    async fn context_non_mutation_has_null_before_content() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "context", "call_id": "tc-2" })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error());
        assert!(out.content["before_content"].is_null());
    }

    #[tokio::test]
    async fn context_missing_call_id_returns_error() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "context" })), &ctx)
            .await
            .unwrap();
        assert!(out.is_error());
        assert_eq!(out.content["error"]["kind"], "invalid_arguments");
        assert_eq!(
            out.content["error"]["message"],
            "call_id required for context query"
        );
    }

    #[tokio::test]
    async fn self_entry_recorded_level_1_only_yields_minimal_detail() {
        // Mirrors the dispatch path: an `action_log` call is recorded with
        // `level_1_only = true`, so a later detail lookup must not expose
        // the stored query output/args.
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        log.record_completion(CompletionRecord {
            tool_name: "action_log",
            tool_call_id: "tc-self",
            tool_use_description: "list",
            outcome: Outcome::Success,
            output: &json!({ "query": "list", "entries": [1, 2, 3] }),
            args: json!({ "query": "list" }),
            duration_ms: 9,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: true,
        });

        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "detail", "call_id": "tc-self" })),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error());
        assert_eq!(out.content["entry"]["tool_name"], "action_log");
        assert!(out.content["output"].is_null());
        assert!(out.content["args"].is_null());
        assert_eq!(out.content["duration_ms"], 0);
    }

    #[tokio::test]
    async fn missing_action_log_extension_errors() {
        let ctx = ToolContext::empty();
        let err = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "list" })), &ctx)
            .await
            .expect_err("no action log");
        match err {
            ToolError::MissingExtension { extension } => {
                assert!(extension.contains("ActionLog"), "{extension}");
            }
            other => panic!("expected MissingExtension, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_query_errors() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let err = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "bogus" })), &ctx)
            .await
            .expect_err("unknown query");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    fn record_edit_mutation(log: &ActionLog, id: &str, path: &std::path::Path) {
        log.record_completion(CompletionRecord {
            tool_name: "edit",
            tool_call_id: id,
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &json!({
                "path": path.to_string_lossy(),
                "blast_radius": { "lines_added": 2, "lines_removed": 1 },
            }),
            args: json!({}),
            duration_ms: 1,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });
    }

    #[tokio::test]
    async fn mutations_query_returns_entries_with_exact_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();

        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        record_edit_mutation(&log, "tc-1", &path);

        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "mutations" })), &ctx)
            .await
            .unwrap();
        assert!(!out.is_error());

        // Top-level envelope keys are exactly query/count/entries.
        let obj = out.content.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["count", "entries", "query"]);

        assert_eq!(out.content["query"], "mutations");
        assert_eq!(out.content["count"], 1);
        let entry = &out.content["entries"][0];
        assert_eq!(entry["file_path"].as_str(), path.to_str());
        assert_eq!(entry["operation"], "Modified");
        assert_eq!(entry["first_tool_call_id"], "tc-1");
        assert_eq!(entry["last_tool_call_id"], "tc-1");
        assert_eq!(entry["revert_status"], "Active");
        assert_eq!(entry["diff_stats"]["lines_added"], 2);
        assert_eq!(entry["diff_stats"]["lines_removed"], 1);
    }

    #[tokio::test]
    async fn mutations_query_filter_file_narrows_to_one() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, "a\n").unwrap();
        std::fs::write(&b, "b\n").unwrap();

        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        record_edit_mutation(&log, "tc-1", &a);
        record_edit_mutation(&log, "tc-2", &b);

        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(
                    json!({ "query": "mutations", "filter": { "file": a.to_string_lossy() } }),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["count"], 1);
        assert_eq!(out.content["entries"][0]["file_path"].as_str(), a.to_str());
    }

    #[tokio::test]
    async fn mutations_query_is_session_scoped_per_instance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "x\n").unwrap();

        let seeded = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        record_edit_mutation(&seeded, "tc-1", &path);

        // A different ActionLog instance has its own ledger and must not see
        // the first instance's mutations.
        let fresh = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        let ctx = ctx_with(Arc::clone(&fresh));
        let out = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "mutations" })), &ctx)
            .await
            .unwrap();
        assert_eq!(out.content["count"], 0);
        assert_eq!(out.content["entries"], json!([]));
    }

    #[test]
    fn metadata_is_correct() {
        let tool = ActionLogTool::new();
        assert_eq!(tool.name(), "action_log");
        assert_eq!(tool.category(), ToolCategory::Discovery);
        assert_eq!(tool.effect(), ToolEffect::ReadOnly);
        let schema = tool.input_schema();
        let enum_values = schema["properties"]["query"]["enum"].as_array().unwrap();
        assert_eq!(
            enum_values,
            &vec![
                json!("list"),
                json!("detail"),
                json!("context"),
                json!("mutations"),
                json!("follow_ups"),
                json!("events")
            ]
        );
        assert_eq!(schema["required"], json!(["query"]));
        assert!(
            schema["properties"]["filter"]["properties"]["event_type"]["description"]
                .as_str()
                .is_some_and(|d| d.contains("agent_message.sent")),
            "filter.event_type documents the message audit types"
        );
    }

    fn seed_follow_up(
        log: &ActionLog,
        tool: &str,
        id: &str,
        outcome: Outcome,
        action: &str,
        target_tool: &str,
        expires: ExpiryCondition,
    ) {
        let follow_up = FollowUpAction {
            action: action.to_owned(),
            description: format!("{action} via {target_tool}"),
            tool: target_tool.to_owned(),
            args: json!({}),
            args_mode: crate::tool::follow_up::FollowUpArgsMode::MergeOriginal,
            expires,
            confidence: Confidence::High,
            before_content: BeforeContentSource::Unavailable,
        };
        log.record_completion(CompletionRecord {
            tool_name: tool,
            tool_call_id: id,
            tool_use_description: "",
            outcome,
            output: &json!({}),
            args: json!({ "id": id }),
            duration_ms: 1,
            follow_ups: vec![follow_up],
            post_validate_outcome: None,
            level_1_only: false,
        });
    }

    #[tokio::test]
    async fn follow_ups_excludes_expired_file_modified_action() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"original").unwrap();

        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        seed_follow_up(
            &log,
            "edit",
            "tc-fm",
            Outcome::Success,
            "reapply",
            "apply_patch",
            ExpiryCondition::FileModified {
                path: path.clone(),
                content_hash: crate::session::action_log::hash_content(b"original"),
            },
        );
        seed_follow_up(
            &log,
            "edit",
            "tc-never",
            Outcome::Success,
            "undo",
            "apply_patch",
            ExpiryCondition::Never,
        );

        let ctx = ctx_with(Arc::clone(&log));

        // Before mutation: both surface.
        let out = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "follow_ups" })), &ctx)
            .await
            .unwrap();
        assert!(!out.is_error());
        assert_eq!(out.content["query"], "follow_ups");
        assert_eq!(out.content["count"], 2);

        // Mutate the file: the FileModified action expires.
        std::fs::write(&path, b"changed").unwrap();
        let out = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "follow_ups" })), &ctx)
            .await
            .unwrap();
        assert_eq!(out.content["count"], 1);
        let actions = out.content["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["action"], "undo");
    }

    #[tokio::test]
    async fn follow_ups_action_shape_includes_all_fields() {
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        seed_follow_up(
            &log,
            "edit",
            "tc-1",
            Outcome::Success,
            "undo",
            "apply_patch",
            ExpiryCondition::Never,
        );
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(&envelope(json!({ "query": "follow_ups" })), &ctx)
            .await
            .unwrap();
        let action = &out.content["actions"][0];
        assert_eq!(action["tool_call_id"], "tc-1");
        assert_eq!(action["action"], "undo");
        assert_eq!(action["description"], "undo via apply_patch");
        assert_eq!(action["tool"], "apply_patch");
        assert_eq!(action["expires"], "never");
    }

    #[tokio::test]
    async fn follow_ups_filter_tool_scopes_to_registering_tool() {
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        seed_follow_up(
            &log,
            "edit",
            "tc-edit",
            Outcome::Success,
            "undo",
            "apply_patch",
            ExpiryCondition::Never,
        );
        seed_follow_up(
            &log,
            "write",
            "tc-write",
            Outcome::Success,
            "undo",
            "apply_patch",
            ExpiryCondition::Never,
        );
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "follow_ups", "filter": { "tool": "edit" } })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["count"], 1);
        assert_eq!(out.content["actions"][0]["tool_call_id"], "tc-edit");
    }

    #[tokio::test]
    async fn follow_ups_filter_outcome_scopes_to_original_outcome() {
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        seed_follow_up(
            &log,
            "edit",
            "tc-ok",
            Outcome::Success,
            "undo",
            "apply_patch",
            ExpiryCondition::Never,
        );
        seed_follow_up(
            &log,
            "edit",
            "tc-err",
            Outcome::Error {
                message: "boom".to_owned(),
            },
            "retry",
            "edit",
            ExpiryCondition::Never,
        );
        let ctx = ctx_with(Arc::clone(&log));
        let out = ActionLogTool::new()
            .execute(
                &envelope(json!({ "query": "follow_ups", "filter": { "outcome": "error" } })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["count"], 1);
        assert_eq!(out.content["actions"][0]["tool_call_id"], "tc-err");
        assert_eq!(out.content["actions"][0]["action"], "retry");
    }

    // ----- scope (federated subtree queries) --------------------------------

    use crate::agent::message_router::MessageRouter;
    use crate::agent::registry::AgentRegistry;
    use crate::provider::mock::MockProvider;
    use crate::provider::traits::Provider;
    use crate::session::action_log_tree::ActionLogTree;
    use crate::tools::agent::AgentToolInfra;
    use uuid::Uuid;

    fn infra_for(
        agent_id: Uuid,
        parent_id: Option<Uuid>,
        registry: &Arc<parking_lot::RwLock<AgentRegistry>>,
    ) -> Arc<AgentToolInfra> {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        Arc::new(AgentToolInfra {
            registry: Arc::clone(registry),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id,
            parent_id,
            grant: None,
            tool_registry: None,
        })
    }

    /// Root + one registered child ("/smoke/child", role "researcher"),
    /// each with its own log in a shared [`ActionLogTree`], plus contexts
    /// for both. Entries: root `p-1` (read), child `c-1` (edit), root
    /// `p-2` (read), with strictly increasing timestamps.
    struct Family {
        registry: Arc<parking_lot::RwLock<AgentRegistry>>,
        tree: Arc<ActionLogTree>,
        root_id: Uuid,
        child_id: Uuid,
        child_log: Arc<ActionLog>,
        root_ctx: ToolContext,
        child_ctx: ToolContext,
    }

    fn family() -> Family {
        let registry = AgentRegistry::shared();
        let root_id = Uuid::new_v4();
        let root_policy = crate::tools::agent::coord::test_support::test_root_policy();
        let guard = AgentRegistry::reserve(
            &registry,
            "/smoke/child".to_owned(),
            "researcher".to_owned(),
            "haiku".to_owned(),
            Some(root_id),
            root_policy.grant_for_child(None).expect("grant"),
            Some(&root_policy),
        )
        .expect("reserve child");
        let child_id = guard.id();
        guard.confirm().expect("confirm child");

        let root_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        let child_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        let tree = Arc::new(ActionLogTree::new(root_id));
        tree.register(root_id, None, Arc::clone(&root_log));
        tree.register(child_id, Some(root_id), Arc::clone(&child_log));

        // Strictly increasing timestamps so merged ordering is
        // deterministic even on coarse clocks.
        seed(&root_log, "read", "p-1", Outcome::Success, json!({}));
        std::thread::sleep(std::time::Duration::from_millis(2));
        seed(&child_log, "edit", "c-1", Outcome::Success, json!({}));
        std::thread::sleep(std::time::Duration::from_millis(2));
        seed(&root_log, "read", "p-2", Outcome::Success, json!({}));

        let root_ctx = ToolContext::empty();
        root_ctx.insert_extension(Arc::clone(&root_log));
        root_ctx.insert_extension(infra_for(root_id, None, &registry));
        root_ctx.insert_extension(Arc::clone(&tree));

        let child_ctx = ToolContext::empty();
        child_ctx.insert_extension(Arc::clone(&child_log));
        child_ctx.insert_extension(infra_for(child_id, Some(root_id), &registry));
        child_ctx.insert_extension(Arc::clone(&tree));

        Family {
            registry,
            tree,
            root_id,
            child_id,
            child_log,
            root_ctx,
            child_ctx,
        }
    }

    async fn run(ctx: &ToolContext, args: Value) -> ToolOutput {
        ActionLogTool::new()
            .execute(&envelope(args), ctx)
            .await
            .expect("action_log executes")
    }

    /// Explicit `scope: "self"` is byte-for-byte the default shape — no
    /// `scope` echo, no `agents` legend, no per-entry labels.
    #[tokio::test]
    async fn scope_self_explicit_matches_default_shape() {
        let fam = family();
        let default_out = run(&fam.root_ctx, json!({ "query": "list" })).await;
        let self_out = run(&fam.root_ctx, json!({ "query": "list", "scope": "self" })).await;
        assert_eq!(default_out.content, self_out.content);
        assert!(default_out.content.get("scope").is_none());
        assert!(default_out.content.get("agents").is_none());
        assert_eq!(
            default_out.content["count"], 2,
            "self sees only its own log"
        );
    }

    /// Parent `scope: "all"` interleaves its own and its children's
    /// entries by timestamp, each labeled with registry ground truth.
    #[tokio::test]
    async fn scope_all_interleaves_children_with_labels() {
        let fam = family();
        let out = run(&fam.root_ctx, json!({ "query": "list", "scope": "all" })).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["scope"], "all");
        assert_eq!(out.content["count"], 3);
        let entries = out.content["entries"].as_array().unwrap();
        assert_eq!(entries[0]["id"], "p-1");
        assert_eq!(entries[0]["agent"], "root");
        assert_eq!(entries[1]["id"], "c-1");
        assert_eq!(entries[1]["agent"], "/smoke/child");
        assert_eq!(entries[2]["id"], "p-2");
        assert_eq!(entries[2]["agent"], "root");

        let agents = out.content["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0]["agent"], "root");
        assert!(
            agents[0].get("role").is_none(),
            "root has no registry role; role appears only when set",
        );
        assert_eq!(agents[1]["agent"], "/smoke/child");
        assert_eq!(agents[1]["id"], fam.child_id.to_string());
        assert_eq!(agents[1]["role"], "researcher");
    }

    /// `children` covers direct children only; `all` reaches grandchildren.
    #[tokio::test]
    async fn scope_children_excludes_grandchildren_but_all_includes_them() {
        let fam = family();
        let grandchild_id = Uuid::new_v4();
        let grandchild_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        seed(&grandchild_log, "bash", "g-1", Outcome::Success, json!({}));
        fam.tree
            .register(grandchild_id, Some(fam.child_id), grandchild_log);

        let children = run(
            &fam.root_ctx,
            json!({ "query": "list", "scope": "children" }),
        )
        .await;
        assert_eq!(children.content["count"], 3, "{:?}", children.content);
        assert!(
            !children.content["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["id"] == "g-1"),
            "children scope must not include grandchildren",
        );

        let all = run(&fam.root_ctx, json!({ "query": "list", "scope": "all" })).await;
        assert_eq!(all.content["count"], 4, "{:?}", all.content);
        let g = all.content["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == "g-1")
            .expect("grandchild entry in scope=all");
        assert_eq!(
            g["agent"],
            grandchild_id.to_string(),
            "an unregistered agent labels as its bare UUID",
        );
    }

    /// Boundary: a child can never query its parent or a sibling — by
    /// UUID or by path — and its own `all` scope is its subtree only.
    #[tokio::test]
    async fn scope_blocks_parent_and_sibling_queries() {
        let fam = family();

        // Upward by UUID.
        let out = run(
            &fam.child_ctx,
            json!({ "query": "list", "scope": fam.root_id.to_string() }),
        )
        .await;
        assert!(out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["error"]["kind"], "permission_denied");

        // Sibling by path.
        let root_policy = crate::tools::agent::coord::test_support::test_root_policy();
        let guard = AgentRegistry::reserve(
            &fam.registry,
            "/smoke/sibling".to_owned(),
            "worker".to_owned(),
            "haiku".to_owned(),
            Some(fam.root_id),
            root_policy.grant_for_child(None).expect("grant"),
            Some(&root_policy),
        )
        .expect("reserve sibling");
        let sibling_id = guard.id();
        guard.confirm().expect("confirm sibling");
        let sibling_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        fam.tree
            .register(sibling_id, Some(fam.root_id), sibling_log);
        let out = run(
            &fam.child_ctx,
            json!({ "query": "list", "scope": "/smoke/sibling" }),
        )
        .await;
        assert!(out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["error"]["kind"], "permission_denied");

        // The child's own `all` scope holds only its subtree.
        let out = run(&fam.child_ctx, json!({ "query": "list", "scope": "all" })).await;
        assert_eq!(out.content["count"], 1, "{:?}", out.content);
        assert_eq!(out.content["entries"][0]["id"], "c-1");
        assert_eq!(out.content["entries"][0]["agent"], "/smoke/child");
    }

    /// A specific descendant scope (registry path or UUID) returns that
    /// agent's own log, labeled.
    #[tokio::test]
    async fn scope_specific_agent_resolves_by_path_and_uuid() {
        let fam = family();
        for scope in [json!("/smoke/child"), json!(fam.child_id.to_string())] {
            let out = run(&fam.root_ctx, json!({ "query": "list", "scope": scope })).await;
            assert!(!out.is_error(), "{:?}", out.content);
            assert_eq!(out.content["count"], 1);
            assert_eq!(out.content["entries"][0]["id"], "c-1");
            assert_eq!(out.content["entries"][0]["agent"], "/smoke/child");
        }
    }

    /// An identifier with no record anywhere resolves to not_found.
    #[tokio::test]
    async fn scope_unknown_agent_is_not_found() {
        let fam = family();
        let out = run(
            &fam.root_ctx,
            json!({ "query": "list", "scope": "/nope/missing" }),
        )
        .await;
        assert!(out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["error"]["kind"], "not_found");
    }

    /// detail / context for a call id resolve across the queried scope and
    /// name the owning agent.
    #[tokio::test]
    async fn scope_detail_and_context_resolve_into_child_log() {
        let fam = family();
        let out = run(
            &fam.root_ctx,
            json!({ "query": "detail", "call_id": "c-1", "scope": "all" }),
        )
        .await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["agent"], "/smoke/child");
        assert_eq!(out.content["entry"]["tool_name"], "edit");
        assert_eq!(out.content["scope"], "all");

        let out = run(
            &fam.root_ctx,
            json!({ "query": "context", "call_id": "c-1", "scope": "all" }),
        )
        .await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["agent"], "/smoke/child");

        // Default scope still cannot see the child's call — the parent's
        // own log does not contain it.
        let out = run(
            &fam.root_ctx,
            json!({ "query": "detail", "call_id": "c-1" }),
        )
        .await;
        assert!(out.is_error());
        assert_eq!(out.content["error"]["kind"], "not_found");
    }

    /// mutations federate per-agent ledgers, each entry labeled.
    #[tokio::test]
    async fn scope_mutations_federate_with_labels() {
        let fam = family();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("child.rs");
        std::fs::write(&path, "fn c() {}\n").unwrap();
        fam.child_log.record_completion(CompletionRecord {
            tool_name: "edit",
            tool_call_id: "c-mut",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &json!({
                "path": path.to_string_lossy(),
                "blast_radius": { "lines_added": 1, "lines_removed": 0 },
            }),
            args: json!({}),
            duration_ms: 1,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });

        let out = run(
            &fam.root_ctx,
            json!({ "query": "mutations", "scope": "all" }),
        )
        .await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["count"], 1);
        let entry = &out.content["entries"][0];
        assert_eq!(entry["agent"], "/smoke/child");
        assert_eq!(entry["file_path"].as_str(), path.to_str());

        // The file filter narrows federated results the same way.
        let out = run(
            &fam.root_ctx,
            json!({
                "query": "mutations",
                "scope": "all",
                "filter": { "file": "/definitely/not/this.rs" },
            }),
        )
        .await;
        assert_eq!(out.content["count"], 0);
    }

    /// follow_ups stay self-scoped: a non-self scope is a structured
    /// invalid_arguments failure, never a silent fallback.
    #[tokio::test]
    async fn scope_follow_ups_rejected_with_structured_error() {
        let fam = family();
        let out = run(
            &fam.root_ctx,
            json!({ "query": "follow_ups", "scope": "all" }),
        )
        .await;
        assert!(out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["error"]["kind"], "invalid_arguments");
    }

    /// Reclaimed children stay queryable for the session: the tree holds
    /// the log Arc independently of registry reclamation, and the label
    /// falls back to the tombstone's path (role not retained).
    #[tokio::test]
    async fn scope_reclaimed_child_log_stays_queryable() {
        let fam = family();
        {
            let mut reg = fam.registry.write();
            reg.mark_completing(fam.child_id).expect("completing");
            reg.mark_completed(fam.child_id).expect("completed");
            assert!(reg.remove_terminal(fam.child_id), "reclaim child entry");
        }
        let out = run(&fam.root_ctx, json!({ "query": "list", "scope": "all" })).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["count"], 3);
        let child_entry = out.content["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == "c-1")
            .expect("reclaimed child's entries stay queryable");
        assert_eq!(
            child_entry["agent"], "/smoke/child",
            "label resolves through the registry tombstone",
        );
        let agents = out.content["agents"].as_array().unwrap();
        let child_legend = agents
            .iter()
            .find(|a| a["agent"] == "/smoke/child")
            .expect("legend entry");
        assert!(
            child_legend.get("role").is_none(),
            "tombstones do not retain roles",
        );
    }

    /// A context with no tree and no agent infrastructure has no
    /// descendants: `children` / `all` truthfully resolve to the caller
    /// alone, labeled "root", in the federated shape.
    #[tokio::test]
    async fn scope_without_tree_degrades_to_self_with_root_label() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        let out = run(&ctx, json!({ "query": "list", "scope": "all" })).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["scope"], "all");
        assert_eq!(out.content["count"], 3);
        assert!(
            out.content["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["agent"] == "root"),
            "{:?}",
            out.content,
        );
        let agents = out.content["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["agent"], "root");
        assert!(
            agents[0].get("id").is_none(),
            "identity unknown without infra"
        );

        // A UUID-shaped scope without infrastructure names an agent
        // this context has no record of anywhere: not_found, never a
        // permission_denied that would imply the agent exists.
        let out = run(
            &ctx,
            json!({ "query": "list", "scope": Uuid::new_v4().to_string() }),
        )
        .await;
        assert!(out.is_error());
        assert_eq!(out.content["error"]["kind"], "not_found");
    }

    /// A parseable UUID that no agent ever held resolves to not_found
    /// even with a full tree + registry installed — permission_denied is
    /// reserved for agents that exist outside the caller's subtree, and
    /// its message ("route through the parent") would be a lie here.
    #[tokio::test]
    async fn scope_never_existed_uuid_is_not_found_with_tree() {
        let fam = family();
        let out = run(
            &fam.root_ctx,
            json!({ "query": "list", "scope": Uuid::new_v4().to_string() }),
        )
        .await;
        assert!(out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["error"]["kind"], "not_found");
    }

    /// An agent registered in the tree but never in the registry (e.g. a
    /// grandchild wired tree-only) still resolves by UUID — the tree
    /// existence probe, not just the registry, vouches for it.
    #[tokio::test]
    async fn scope_tree_only_agent_resolves_by_uuid() {
        let fam = family();
        let grandchild_id = Uuid::new_v4();
        let grandchild_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        seed(&grandchild_log, "bash", "g-1", Outcome::Success, json!({}));
        fam.tree
            .register(grandchild_id, Some(fam.child_id), grandchild_log);

        let out = run(
            &fam.root_ctx,
            json!({ "query": "list", "scope": grandchild_id.to_string() }),
        )
        .await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["count"], 1);
        assert_eq!(out.content["entries"][0]["id"], "g-1");
    }

    // ----- events (Custom audit events, W3.7) --------------------------------

    fn append_custom(store: &EventStore, event_type: &str, data: Value) {
        store
            .append(crate::session::events::SessionEvent::Custom {
                base: crate::session::events::EventBase::new(None),
                event_type: event_type.to_owned(),
                data,
            })
            .expect("append custom event");
    }

    /// Events are served verbatim from the backing store in append
    /// order, with type and timestamp.
    #[tokio::test]
    async fn events_query_returns_custom_events_verbatim() {
        let store = Arc::new(EventStore::new());
        let log = Arc::new(ActionLog::new(Arc::clone(&store)));
        append_custom(
            &store,
            "agent_message.sent",
            json!({ "phase": "sent", "seq": 1 }),
        );
        append_custom(&store, "subagent.started", json!({ "phase": "started" }));

        let ctx = ctx_with(Arc::clone(&log));
        let out = run(&ctx, json!({ "query": "events" })).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["query"], "events");
        assert_eq!(out.content["count"], 2);
        let events = out.content["events"].as_array().unwrap();
        assert_eq!(events[0]["type"], "agent_message.sent");
        assert_eq!(events[0]["data"]["seq"], 1);
        assert!(events[0]["ts"].as_str().is_some());
        assert_eq!(events[1]["type"], "subagent.started");
        // Self-scope output carries no agent labels, matching list.
        assert!(events[0].get("agent").is_none());
        assert!(out.content.get("agents").is_none());
    }

    /// `filter.event_type` narrows to one type; `filter.last` bounds.
    #[tokio::test]
    async fn events_query_filters_by_event_type_and_last() {
        let store = Arc::new(EventStore::new());
        let log = Arc::new(ActionLog::new(Arc::clone(&store)));
        for seq in 0..3 {
            append_custom(
                &store,
                "agent_message.sent",
                json!({ "phase": "sent", "seq": seq }),
            );
        }
        append_custom(
            &store,
            "agent_message.delivered",
            json!({ "phase": "delivered" }),
        );

        let ctx = ctx_with(Arc::clone(&log));
        let out = run(
            &ctx,
            json!({ "query": "events", "filter": { "event_type": "agent_message.sent" } }),
        )
        .await;
        assert_eq!(out.content["count"], 3, "{:?}", out.content);

        let out = run(
            &ctx,
            json!({
                "query": "events",
                "filter": { "event_type": "agent_message.sent", "last": 2 },
            }),
        )
        .await;
        assert_eq!(out.content["count"], 2);
        let events = out.content["events"].as_array().unwrap();
        // The most recent two, still chronological.
        assert_eq!(events[0]["data"]["seq"], 1);
        assert_eq!(events[1]["data"]["seq"], 2);
    }

    /// An event type with no matches (or an empty store) yields an
    /// honest zero, not an error.
    #[tokio::test]
    async fn events_query_empty_result_has_zero_count() {
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        let ctx = ctx_with(Arc::clone(&log));
        let out = run(
            &ctx,
            json!({ "query": "events", "filter": { "event_type": "agent_message.sent" } }),
        )
        .await;
        assert!(!out.is_error());
        assert_eq!(out.content["count"], 0);
        assert_eq!(out.content["events"], json!([]));
    }

    /// Tool-call filter fields on the events query fail typed — never a
    /// silent no-op that pretends to have narrowed.
    #[tokio::test]
    async fn events_query_rejects_tool_call_filter_fields() {
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        let ctx = ctx_with(Arc::clone(&log));
        for (field, filter) in [
            ("tool", json!({ "tool": "edit" })),
            ("outcome", json!({ "outcome": "success" })),
            ("file", json!({ "file": "a.rs" })),
            ("since", json!({ "since": "tc-1" })),
        ] {
            let out = run(&ctx, json!({ "query": "events", "filter": filter })).await;
            assert!(out.is_error(), "filter.{field} must be rejected");
            assert_eq!(out.content["error"]["kind"], "invalid_arguments");
            assert_eq!(out.content["error"]["detail"]["field"], field);
        }
    }

    /// `filter.event_type` on a tool-call query fails typed for the same
    /// reason.
    #[tokio::test]
    async fn event_type_filter_rejected_on_tool_call_queries() {
        let log = populated_log();
        let ctx = ctx_with(Arc::clone(&log));
        for query in ["list", "mutations", "follow_ups"] {
            let out = run(
                &ctx,
                json!({ "query": query, "filter": { "event_type": "agent_message.sent" } }),
            )
            .await;
            assert!(out.is_error(), "{query} must reject filter.event_type");
            assert_eq!(out.content["error"]["kind"], "invalid_arguments");
            assert_eq!(out.content["error"]["detail"]["field"], "event_type");
        }
    }

    /// Federated events: a parent's `scope: "all"` interleaves its own
    /// and its child's Custom events by timestamp, labeled per agent —
    /// this is how a parent reads `agent_message.delivered` records that
    /// only exist in the child's store.
    #[tokio::test]
    async fn scope_all_federates_events_with_labels() {
        let fam = family();
        // Root store: a sent audit record (granting-parent copy).
        let root_store = Arc::new(EventStore::new());
        let root_log = Arc::new(ActionLog::new(Arc::clone(&root_store)));
        append_custom(
            &root_store,
            "agent_message.sent",
            json!({ "phase": "sent", "seq": 1 }),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        // Child store: the delivery record.
        let child_store = Arc::new(EventStore::new());
        let child_log = Arc::new(ActionLog::new(Arc::clone(&child_store)));
        append_custom(
            &child_store,
            "agent_message.delivered",
            json!({ "phase": "delivered", "seq": 1 }),
        );
        // Swap freshly-stored logs into the family tree (the family
        // helper's logs have storeless seeds).
        let tree = Arc::new(ActionLogTree::new(fam.root_id));
        tree.register(fam.root_id, None, Arc::clone(&root_log));
        tree.register(fam.child_id, Some(fam.root_id), child_log);
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::clone(&root_log));
        ctx.insert_extension(infra_for(fam.root_id, None, &fam.registry));
        ctx.insert_extension(tree);

        let out = run(&ctx, json!({ "query": "events", "scope": "all" })).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["scope"], "all");
        assert_eq!(out.content["count"], 2);
        let events = out.content["events"].as_array().unwrap();
        assert_eq!(events[0]["type"], "agent_message.sent");
        assert_eq!(events[0]["agent"], "root");
        assert_eq!(events[1]["type"], "agent_message.delivered");
        assert_eq!(events[1]["agent"], "/smoke/child");
        let agents = out.content["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 2);
    }

    /// The events query respects the same subtree boundary as the rest
    /// of the scope surface.
    #[tokio::test]
    async fn scope_events_blocked_for_parent_query() {
        let fam = family();
        let out = run(
            &fam.child_ctx,
            json!({ "query": "events", "scope": fam.root_id.to_string() }),
        )
        .await;
        assert!(out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["error"]["kind"], "permission_denied");
    }

    /// The scope property is documented in the input schema.
    #[test]
    fn input_schema_documents_scope() {
        let schema = ActionLogTool::new().input_schema();
        let scope = &schema["properties"]["scope"];
        assert_eq!(scope["type"], "string");
        let desc = scope["description"].as_str().unwrap();
        for needle in ["self", "children", "all", "subtree", "follow_ups"] {
            assert!(
                desc.contains(needle),
                "scope description must mention {needle}"
            );
        }
    }
}
