//! `action_log` — queryable memory of the agent's own tool calls.
//!
//! The tool is a thin read-only view over the session
//! [`ActionLog`](crate::session::action_log::ActionLog), published on the
//! shared [`ToolContext`] as an [`Arc<ActionLog>`] extension by the agent
//! builder. It never holds the log itself, mirroring `tool_search`'s use of
//! a context-published catalogue.
//!
//! Five query modes are supported:
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

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::ToolError;
use crate::session::action_log::{ActionLog, ActionLogEntry};
use crate::session::mutation_ledger::MutationLedgerEntry;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

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
    /// One of `list`, `detail`, `context`, `mutations`, `follow_ups`.
    query: String,
    /// Optional scoping filter for `list`, `mutations`, and `follow_ups` queries.
    #[serde(default)]
    filter: Option<ActionLogFilter>,
    /// Tool-call id required by `detail` and `context`; ignored by `list`.
    #[serde(default)]
    call_id: Option<String>,
}

/// Optional list-query scoping filter. All fields are optional and combine
/// with AND semantics; an absent filter returns every entry.
#[derive(Debug, Default, Deserialize)]
struct ActionLogFilter {
    /// Keep only entries whose `tool_name` equals this value.
    #[serde(default)]
    tool: Option<String>,
    /// Keep only entries whose coarse outcome tag equals this value
    /// (`success`, `error`, or `blocked`).
    #[serde(default)]
    outcome: Option<String>,
    /// For `list`, keep only entries whose `summary_line` contains this
    /// substring. For `mutations`, keep only the ledger entry whose
    /// `file_path` equals this path exactly.
    #[serde(default)]
    file: Option<String>,
    /// Keep only entries recorded strictly after the entry with this
    /// `tool_call_id`. When the id is not present in the log, no entries
    /// match (there is nothing known to be after an absent marker).
    #[serde(default)]
    since: Option<String>,
    /// After all other filters, keep only the most recent `last` entries.
    #[serde(default)]
    last: Option<u32>,
}

impl ActionLogFilter {
    /// Apply the filter to `entries` (chronological order in, chronological
    /// order out).
    fn apply(&self, entries: Vec<ActionLogEntry>) -> Vec<ActionLogEntry> {
        let mut filtered: Vec<ActionLogEntry> = match &self.since {
            Some(since) => match entries.iter().position(|e| e.tool_call_id == *since) {
                Some(idx) => entries.into_iter().skip(idx + 1).collect(),
                None => Vec::new(),
            },
            None => entries,
        };

        if let Some(tool) = &self.tool {
            filtered.retain(|e| e.tool_name == *tool);
        }
        if let Some(outcome) = &self.outcome {
            filtered.retain(|e| e.outcome.tag() == outcome.as_str());
        }
        if let Some(file) = &self.file {
            filtered.retain(|e| e.summary_line.contains(file.as_str()));
        }
        if let Some(last) = self.last {
            let last = usize::try_from(last).unwrap_or(usize::MAX);
            if filtered.len() > last {
                filtered.drain(0..filtered.len() - last);
            }
        }
        filtered
    }
}

/// Build the structured error [`ToolOutput`] for a missing `call_id`.
fn missing_call_id(query: &str, started: Instant) -> ToolOutput {
    ToolOutput {
        content: serde_json::json!({
            "error": format!("call_id required for {query} query"),
        }),
        is_error: true,
        duration: started.elapsed(),
    }
}

/// Build the structured error [`ToolOutput`] for an unknown `call_id`.
fn unknown_call_id(call_id: &str, started: Instant) -> ToolOutput {
    ToolOutput {
        content: serde_json::json!({
            "error": "tool_call_id not found",
            "call_id": call_id,
        }),
        is_error: true,
        duration: started.elapsed(),
    }
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
    started: Instant,
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

    ToolOutput {
        content: serde_json::json!({
            "query": "follow_ups",
            "count": actions.len(),
            "actions": actions,
        }),
        is_error: false,
        duration: started.elapsed(),
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
         `filter.tool`/`filter.outcome` scoping."
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
                    "enum": ["list", "detail", "context", "mutations", "follow_ups"],
                    "description": "list: Level 1 summaries with optional filter. detail: Level 2 data for one call_id. context: Level 3 data for one call_id. mutations: file-change ledger with live revert status, optionally scoped by filter.file. follow_ups: unexpired follow-up actions, optionally scoped by filter.tool (registering tool) and filter.outcome."
                },
                "filter": {
                    "type": "object",
                    "description": "Optional scoping for list, mutations, and follow_ups queries. Fields combine with AND. For follow_ups, only tool (the registering tool's name) and outcome (the original call's outcome) apply.",
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
                            "description": "list: entries whose summary mentions this file path. mutations: the ledger entry whose file_path equals this path exactly."
                        },
                        "since": {
                            "type": "string",
                            "description": "Return only entries recorded after this tool_call_id."
                        },
                        "last": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Return only the most recent N entries."
                        }
                    },
                    "additionalProperties": false
                },
                "call_id": {
                    "type": "string",
                    "description": "Tool-call id, required for detail and context queries."
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
        let started = Instant::now();
        let args: ActionLogArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;

        let action_log: Arc<ActionLog> =
            ctx.get_extension::<ActionLog>()
                .ok_or_else(|| ToolError::ExecutionFailed {
                    reason: "action log not configured in tool context".to_string(),
                })?;

        match args.query.as_str() {
            "list" => {
                let entries = action_log.entries();
                let filtered = match &args.filter {
                    Some(filter) => filter.apply(entries),
                    None => entries,
                };
                let summaries: Vec<serde_json::Value> =
                    filtered.iter().map(ActionLogEntry::compact_json).collect();
                Ok(ToolOutput {
                    content: serde_json::json!({
                        "query": "list",
                        "count": summaries.len(),
                        "entries": summaries,
                    }),
                    is_error: false,
                    duration: started.elapsed(),
                })
            }
            "detail" => {
                let Some(call_id) = args.call_id.as_deref() else {
                    return Ok(missing_call_id("detail", started));
                };
                match action_log.get_detail(call_id) {
                    Some(detail) => Ok(ToolOutput {
                        content: serde_json::json!({
                            "query": "detail",
                            "entry": detail.entry,
                            "output": detail.output,
                            "args": detail.args,
                            "duration_ms": detail.duration_ms,
                            "follow_ups": detail.follow_ups,
                        }),
                        is_error: false,
                        duration: started.elapsed(),
                    }),
                    None => Ok(unknown_call_id(call_id, started)),
                }
            }
            "context" => {
                let Some(call_id) = args.call_id.as_deref() else {
                    return Ok(missing_call_id("context", started));
                };
                match action_log.get_context(call_id) {
                    Some(context) => Ok(ToolOutput {
                        content: serde_json::json!({
                            "query": "context",
                            "entry": context.detail.entry,
                            "output": context.detail.output,
                            "args": context.detail.args,
                            "duration_ms": context.detail.duration_ms,
                            "follow_ups": context.detail.follow_ups,
                            "before_content": context.before_content,
                            "post_validate_outcome": context.post_validate_outcome,
                        }),
                        is_error: false,
                        duration: started.elapsed(),
                    }),
                    None => Ok(unknown_call_id(call_id, started)),
                }
            }
            "mutations" => {
                let entries = action_log.mutation_entries();
                let filtered: Vec<MutationLedgerEntry> =
                    match args.filter.as_ref().and_then(|f| f.file.as_ref()) {
                        Some(file) => {
                            let wanted = PathBuf::from(file);
                            entries
                                .into_iter()
                                .filter(|e| e.file_path == wanted)
                                .collect()
                        }
                        None => entries,
                    };
                Ok(ToolOutput {
                    content: serde_json::json!({
                        "query": "mutations",
                        "count": filtered.len(),
                        "entries": filtered,
                    }),
                    is_error: false,
                    duration: started.elapsed(),
                })
            }
            "follow_ups" => Ok(query_follow_ups(
                &action_log,
                args.filter.as_ref(),
                ctx,
                started,
            )),
            other => Err(ToolError::ExecutionFailed {
                reason: format!(
                    "unknown query '{other}': expected list, detail, context, mutations, or follow_ups"
                ),
            }),
        }
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
        assert!(!out.is_error);
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
        assert!(!out.is_error);
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
        assert!(!out.is_error);
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
        assert!(out.is_error);
        assert_eq!(out.content["error"], "call_id required for detail query");
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
        assert!(out.is_error);
        assert_eq!(out.content["error"], "tool_call_id not found");
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
        assert!(!out.is_error);
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
        assert!(!out.is_error);
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
        assert!(out.is_error);
        assert_eq!(out.content["error"], "call_id required for context query");
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
        assert!(!out.is_error);
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
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
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
        assert!(!out.is_error);

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
                json!("follow_ups")
            ]
        );
        assert_eq!(schema["required"], json!(["query"]));
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
        assert!(!out.is_error);
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
}
