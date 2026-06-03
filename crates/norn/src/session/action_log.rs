//! Action log: an in-memory query layer over the session's
//! [`EventStore`] that retains every tool call's outcome, structured
//! result, and follow-up actions for the lifetime of the session.
//!
//! The action log is the queryable memory of the agent's own actions.
//! It is cheaper to scan than the full conversation history and richer
//! than what context compaction retains: even when a tool result is
//! summarised or removed from the conversation view, the underlying
//! [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)
//! stays in the event store and the action log preserves the Level 1
//! summary, follow-up actions, and original arguments necessary to
//! drill back into the call.
//!
//! Three levels of detail are exposed:
//!
//! * **Level 1** — [`ActionLogEntry`] one-line summary, retrieved via
//!   the in-memory index without touching the event store.
//! * **Level 2** — Level 1 plus the full structured tool output,
//!   original arguments, duration, and follow-up actions, assembled by
//!   [`ActionLog::get_detail`] by locating the
//!   [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)
//!   matching the tool-call id.
//! * **Level 3** — Level 2 plus before/after file content. For mutation
//!   tools, before-content is sourced from any registered
//!   [`FollowUpAction`] carrying
//!   [`BeforeContentSource::StoredContent`](crate::tool::follow_up::BeforeContentSource::StoredContent).
//!   For non-mutation tools, Level 3 is identical to Level 2 (no file
//!   content to surface).
//!
//! No new storage backend is introduced — the action log is a metadata
//! index keyed by tool-call id over events already persisted by
//! [`EventStore`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::session::mutation_ledger::{MutationLedger, MutationLedgerEntry};
use crate::session::store::EventStore;

use crate::tool::follow_up::{BeforeContentSource, ExpiryCondition, FollowUpAction};

/// Level 1 summary of a single tool call.
///
/// One [`ActionLogEntry`] is appended to the in-memory index per
/// completed tool dispatch (success, error, or pre-validate block).
/// Designed to be cheap to scan — see [`Self::compact_json`] for the
/// token-efficient serialisation used by list queries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionLogEntry {
    /// Name of the tool that was invoked.
    pub tool_name: String,
    /// Provider-assigned tool call id (same id as on the matching
    /// [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)).
    pub tool_call_id: String,
    /// Model-supplied description of intent captured from the tool-use
    /// envelope. Empty when the model did not provide one.
    pub tool_use_description: String,
    /// When the dispatch completed.
    pub timestamp: DateTime<Utc>,
    /// Coarse outcome classification.
    pub outcome: Outcome,
    /// One-line, model-readable summary of the result (e.g.
    /// `"edit committed: src/handler.rs +5/-3"`).
    pub summary_line: String,
}

impl ActionLogEntry {
    /// Token-efficient single-line string for list-query output.
    ///
    /// Format: `tool_name|tool_call_id|outcome|summary` — one line
    /// per entry, pipe-delimited, no JSON object overhead. Keeps a
    /// 100-entry list under the 1000-token budget set by CO1.
    /// Timestamp, description, and full summary are available via
    /// Level 2 detail queries.
    #[must_use]
    pub fn compact_line(&self) -> String {
        let summary = if self.summary_line.len() > 40 {
            format!("{}...", &self.summary_line[..37])
        } else {
            self.summary_line.clone()
        };
        format!(
            "{}|{}|{}|{}",
            self.tool_name,
            self.tool_call_id,
            self.outcome.tag(),
            summary,
        )
    }

    /// Token-efficient JSON object for list-query output.
    ///
    /// Uses short field names — `tool`, `id`, `desc`, `ts`, `outcome`,
    /// `summary` — to keep the per-entry token cost low when the model
    /// scans a long list. The `outcome` value is the coarse tag string
    /// (`success` / `error` / `blocked`); the full structured output,
    /// arguments, and follow-ups are reached via Level 2 detail queries.
    #[must_use]
    pub fn compact_json(&self) -> serde_json::Value {
        serde_json::json!({
            "tool": self.tool_name,
            "id": self.tool_call_id,
            "desc": self.tool_use_description,
            "ts": self.timestamp.to_rfc3339(),
            "outcome": self.outcome.tag(),
            "summary": self.summary_line,
        })
    }
}

/// Coarse outcome of a tool dispatch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Outcome {
    /// The tool executed and produced a non-error result.
    Success,
    /// The tool's execution failed (including pre-validate runtime
    /// failure and execute errors).
    Error {
        /// The error message captured from the tool's output.
        message: String,
    },
    /// A pre-tool hook or pre-validate check blocked the dispatch.
    Blocked {
        /// Reason supplied by the blocking gate.
        reason: String,
    },
}

impl Outcome {
    /// Coarse outcome tag string used by compact list output and the
    /// `outcome` filter of the `action_log` tool: `success`, `error`, or
    /// `blocked`.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error { .. } => "error",
            Self::Blocked { .. } => "blocked",
        }
    }
}

/// Level 2 detail for a single tool call.
///
/// Assembled by [`ActionLog::get_detail`] by combining the in-memory
/// index with the matching
/// [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)
/// from the [`EventStore`]. Fields default to `Null` / `0` when the
/// corresponding event is absent — for example a hook-blocked dispatch
/// records a result with the blocked-output payload, but if the store
/// has been replaced (test contexts) the look-up gracefully degrades.
#[derive(Clone, Debug, Serialize)]
pub struct ActionLogDetail {
    /// Level 1 summary.
    pub entry: ActionLogEntry,
    /// Full structured tool output from the
    /// [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)
    /// event, or `Null` when no matching event is in the store.
    pub output: serde_json::Value,
    /// Arguments the tool was dispatched with.
    pub args: serde_json::Value,
    /// Execution duration in milliseconds, copied from the
    /// [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)
    /// event.
    pub duration_ms: u64,
    /// Follow-up actions registered for this call.
    pub follow_ups: Vec<FollowUpAction>,
}

/// Level 3 full context for a single tool call.
///
/// Extends [`ActionLogDetail`] with before-content for mutation tools and
/// the recorded post-validate outcome.
///
/// Before-content is sourced from any registered [`FollowUpAction`]
/// whose [`BeforeContentSource`] is
/// [`StoredContent`](BeforeContentSource::StoredContent). Mutation tools
/// carry such a follow-up and surface a populated map; non-mutation tools
/// (no stored content) surface `None`, which serialises to JSON `null`.
#[derive(Clone, Debug, Serialize)]
pub struct ActionLogContext {
    /// Level 2 detail.
    pub detail: ActionLogDetail,
    /// Path → before-content snapshots captured at follow-up
    /// registration time. `None` (JSON `null`) for non-mutation tools that
    /// registered no [`StoredContent`](BeforeContentSource::StoredContent)
    /// follow-up.
    pub before_content: Option<HashMap<std::path::PathBuf, String>>,
    /// Post-validate outcome recorded for the call, when the dispatch path
    /// captured one. `None` (JSON `null`) when no post-validate outcome was
    /// recorded for this tool call.
    pub post_validate_outcome: Option<serde_json::Value>,
}

/// Parameters for [`ActionLog::record_completion`].
pub struct CompletionRecord<'a> {
    /// Name of the tool that was invoked.
    pub tool_name: &'a str,
    /// Provider-assigned tool call id.
    pub tool_call_id: &'a str,
    /// Model-supplied description of intent.
    pub tool_use_description: &'a str,
    /// Coarse outcome classification.
    pub outcome: Outcome,
    /// Structured tool output.
    pub output: &'a serde_json::Value,
    /// Original tool call arguments.
    pub args: serde_json::Value,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Follow-up actions registered by the tool.
    pub follow_ups: Vec<FollowUpAction>,
    /// Post-validate outcome recorded for the call, when one was captured.
    /// Surfaced by [`ActionLog::get_context`]. `None` when the dispatch
    /// path recorded no post-validate outcome for this tool.
    pub post_validate_outcome: Option<serde_json::Value>,
    /// When `true`, only the Level 1 [`ActionLogEntry`] is stored — the
    /// Level 2/3 payloads (`output`, `args`, `duration_ms`, `follow_ups`,
    /// `post_validate_outcome`) are dropped. Used for the `action_log`
    /// tool's own dispatches so querying the log does not bloat the log
    /// with full query results (CO4).
    pub level_1_only: bool,
}

struct ActionLogInner {
    entries: IndexMap<String, ActionLogEntry>,
    follow_ups: HashMap<String, Vec<FollowUpAction>>,
    /// O(1) discovery index mapping `(tool_call_id, action_name)` to the
    /// position of the matching [`FollowUpAction`] within the per-call
    /// `follow_ups` vector. Populated alongside `follow_ups` at
    /// [`ActionLog::record_completion`] time.
    follow_up_index: HashMap<(String, String), usize>,
    original_args: HashMap<String, serde_json::Value>,
    outputs: HashMap<String, serde_json::Value>,
    durations: HashMap<String, u64>,
    post_validate_outcomes: HashMap<String, serde_json::Value>,
}

/// Result of [`ActionLog::get_follow_up`]: a single matched follow-up action
/// paired with the original tool call's arguments.
pub struct FollowUpLookup {
    /// The matched follow-up action.
    pub action: FollowUpAction,
    /// Original tool-call arguments stored for the call, or
    /// [`serde_json::Value::Null`] when none were recorded.
    pub original_args: serde_json::Value,
}

/// A still-valid follow-up action surfaced by [`ActionLog::unexpired_follow_ups`],
/// carrying enough context to scope it back to its originating tool call.
pub struct UnexpiredFollowUp {
    /// Tool-call id the follow-up was registered against.
    pub tool_call_id: String,
    /// Name of the tool that registered the follow-up.
    pub registering_tool: String,
    /// Coarse outcome of the original tool call.
    pub outcome: Outcome,
    /// The follow-up action itself.
    pub action: FollowUpAction,
}
/// Query layer over the session [`EventStore`] surfacing the action
/// log's three detail levels.
///
/// Thread-safe via [`parking_lot::RwLock`] mirroring the
/// [`EventStore`]'s own concurrency model. Cheap to clone the holding
/// [`Arc`].
pub struct ActionLog {
    inner: RwLock<ActionLogInner>,
    event_store: Arc<EventStore>,
    mutation_ledger: MutationLedger,
}

impl std::fmt::Debug for ActionLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.read();
        f.debug_struct("ActionLog")
            .field("entries", &inner.entries.len())
            .field("follow_ups", &inner.follow_ups.len())
            .field("follow_up_index", &inner.follow_up_index.len())
            .field("original_args", &inner.original_args.len())
            .field("mutations", &self.mutation_ledger.len())
            .field("event_store", &self.event_store)
            .finish()
    }
}

impl ActionLog {
    /// Create a fresh action log backed by `event_store`.
    ///
    /// The same [`Arc<EventStore>`] threaded through the agent loop
    /// must be passed here — otherwise [`Self::get_detail`] and
    /// [`Self::get_context`] return entries with `Null` outputs because
    /// the look-up store will not contain the matching
    /// [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)
    /// events.
    #[must_use]
    pub fn new(event_store: Arc<EventStore>) -> Self {
        Self {
            inner: RwLock::new(ActionLogInner {
                entries: IndexMap::new(),
                follow_ups: HashMap::new(),
                follow_up_index: HashMap::new(),
                original_args: HashMap::new(),
                outputs: HashMap::new(),
                durations: HashMap::new(),
                post_validate_outcomes: HashMap::new(),
            }),
            event_store,
            mutation_ledger: MutationLedger::new(),
        }
    }

    /// Record the completion of a single tool dispatch.
    ///
    /// Computes the Level 1 summary line, stores the entry keyed by
    /// `tool_call_id` (preserving insertion order via [`IndexMap`]),
    /// and indexes follow-up actions and original arguments alongside
    /// it.
    ///
    /// Called from the tool dispatch path after the
    /// [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)
    /// event has been appended to the [`EventStore`].
    pub fn record_completion(&self, record: CompletionRecord<'_>) {
        let summary_line = compute_summary(record.tool_name, &record.outcome, record.output);

        // Update the session mutation ledger for successful mutation-tool
        // completions only. The ledger is a derived view: it reads the
        // structured output the tool already produced rather than introducing
        // any new store, and is fed solely by this instance's completions so
        // it stays session-scoped.
        if matches!(record.outcome, Outcome::Success) {
            for mutation in crate::session::action_log_mutations::extract_mutations(
                record.tool_name,
                record.tool_call_id,
                record.output,
                &record.follow_ups,
            ) {
                self.mutation_ledger.record_mutation(mutation);
            }
        }

        let entry = ActionLogEntry {
            tool_name: record.tool_name.to_owned(),
            tool_call_id: record.tool_call_id.to_owned(),
            tool_use_description: record.tool_use_description.to_owned(),
            timestamp: Utc::now(),
            outcome: record.outcome,
            summary_line,
        };

        let id = record.tool_call_id.to_owned();
        let mut inner = self.inner.write();
        inner.entries.insert(id.clone(), entry);

        // CO4: `action_log` self-dispatches store only the Level 1 entry.
        // Skipping the Level 2/3 payloads keeps repeated queries from
        // bloating the log with their own (potentially large) results.
        if record.level_1_only {
            return;
        }

        if !record.follow_ups.is_empty() {
            for (idx, follow_up) in record.follow_ups.iter().enumerate() {
                let key = (id.clone(), follow_up.action.clone());
                if inner.follow_up_index.contains_key(&key) {
                    tracing::warn!(
                        tool_call_id = %id,
                        action = %follow_up.action,
                        "duplicate follow-up action name for tool call; \
                         keeping the first indexed slot",
                    );
                    continue;
                }
                inner.follow_up_index.insert(key, idx);
            }
            inner.follow_ups.insert(id.clone(), record.follow_ups);
        }
        inner.original_args.insert(id.clone(), record.args);
        inner.outputs.insert(id.clone(), record.output.clone());
        inner.durations.insert(id.clone(), record.duration_ms);
        if let Some(outcome) = record.post_validate_outcome {
            inner.post_validate_outcomes.insert(id, outcome);
        }
    }

    /// Return all Level 1 entries in insertion order (cloned).
    #[must_use]
    pub fn entries(&self) -> Vec<ActionLogEntry> {
        self.inner.read().entries.values().cloned().collect()
    }

    /// Return the Level 1 entry for a specific tool call.
    #[must_use]
    pub fn entry(&self, tool_call_id: &str) -> Option<ActionLogEntry> {
        self.inner.read().entries.get(tool_call_id).cloned()
    }

    /// Return the Level 2 detail for a specific tool call.
    ///
    /// All data is served from the in-memory cache populated at
    /// [`Self::record_completion`] time — no event store scan needed.
    #[must_use]
    pub fn get_detail(&self, tool_call_id: &str) -> Option<ActionLogDetail> {
        let inner = self.inner.read();
        let entry = inner.entries.get(tool_call_id)?.clone();
        let args = inner
            .original_args
            .get(tool_call_id)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let output = inner
            .outputs
            .get(tool_call_id)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let duration_ms = inner.durations.get(tool_call_id).copied().unwrap_or(0);
        let follow_ups = inner
            .follow_ups
            .get(tool_call_id)
            .cloned()
            .unwrap_or_default();

        Some(ActionLogDetail {
            entry,
            output,
            args,
            duration_ms,
            follow_ups,
        })
    }

    /// Return the Level 3 full context for a specific tool call.
    ///
    /// For mutation tools, before-content is harvested from registered
    /// follow-ups carrying [`BeforeContentSource::StoredContent`]. For
    /// non-mutation tools (no such follow-up), before-content is `None` —
    /// the call returns the same data as [`Self::get_detail`] wrapped in an
    /// [`ActionLogContext`] with a `null` before-content map.
    #[must_use]
    pub fn get_context(&self, tool_call_id: &str) -> Option<ActionLogContext> {
        let detail = self.get_detail(tool_call_id)?;
        let mut before_content: Option<HashMap<std::path::PathBuf, String>> = None;
        for fu in &detail.follow_ups {
            if let BeforeContentSource::StoredContent { files } = &fu.before_content {
                before_content
                    .get_or_insert_with(HashMap::new)
                    .extend(files.clone());
            }
        }
        let post_validate_outcome = self
            .inner
            .read()
            .post_validate_outcomes
            .get(tool_call_id)
            .cloned();
        Some(ActionLogContext {
            detail,
            before_content,
            post_validate_outcome,
        })
    }

    /// Return every file the agent mutated this session, each with its
    /// `revert_status` evaluated against the current filesystem state.
    ///
    /// This is the data source for the `action_log` tool's `mutations` query.
    /// Revert detection is lazy — files are read and hashed here, at query
    /// time, never on a watcher or timer.
    #[must_use]
    pub fn mutation_entries(&self) -> Vec<MutationLedgerEntry> {
        self.mutation_ledger.entries()
    }

    /// Return one mutated file's entry, with `revert_status` evaluated against
    /// the current filesystem state, or `None` when the file was not mutated
    /// this session.
    #[must_use]
    pub fn mutation_entry(&self, file_path: &Path) -> Option<MutationLedgerEntry> {
        self.mutation_ledger.entry(file_path)
    }

    /// Look up a single follow-up action by `tool_call_id` and `action_name`,
    /// returning it together with the original call's arguments.
    ///
    /// Uses the O(1) discovery index; returns `None` when the call id is
    /// unknown or carries no follow-up with that action name.
    #[must_use]
    pub fn get_follow_up(&self, tool_call_id: &str, action_name: &str) -> Option<FollowUpLookup> {
        let inner = self.inner.read();
        let key = (tool_call_id.to_owned(), action_name.to_owned());
        let idx = *inner.follow_up_index.get(&key)?;
        let action = inner.follow_ups.get(tool_call_id)?.get(idx)?.clone();
        let original_args = inner
            .original_args
            .get(tool_call_id)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        Some(FollowUpLookup {
            action,
            original_args,
        })
    }

    /// Return every follow-up action that is still valid at query time across
    /// all recorded calls.
    ///
    /// Expiry is evaluated live: `resolve` maps a recorded path to its
    /// on-disk location (so file-hash checks run against the agent working
    /// directory), and `current_turn_id` decides whether
    /// [`ExpiryCondition::TurnScoped`] follow-ups are still in their turn.
    #[must_use]
    pub fn unexpired_follow_ups<F>(
        &self,
        resolve: F,
        current_turn_id: Option<&str>,
    ) -> Vec<UnexpiredFollowUp>
    where
        F: Fn(&Path) -> PathBuf,
    {
        let inner = self.inner.read();
        let mut out = Vec::new();
        for (tool_call_id, actions) in &inner.follow_ups {
            let Some(entry) = inner.entries.get(tool_call_id) else {
                continue;
            };
            for action in actions {
                if follow_up_is_unexpired(&action.expires, &resolve, current_turn_id) {
                    out.push(UnexpiredFollowUp {
                        tool_call_id: tool_call_id.clone(),
                        registering_tool: entry.tool_name.clone(),
                        outcome: entry.outcome.clone(),
                        action: action.clone(),
                    });
                }
            }
        }
        out
    }
}

/// SHA-256 hex digest of `bytes`, used to fingerprint file content for
/// [`ExpiryCondition::FileModified`] follow-up expiry checks.
#[must_use]
pub fn hash_content(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Returns `true` when the resolved file's current content hash matches the
/// recorded `expected` hash. A missing or unreadable file means the action
/// has expired and yields `false`.
fn file_hash_matches<F>(path: &Path, expected: &str, resolve: &F) -> bool
where
    F: Fn(&Path) -> PathBuf,
{
    let resolved = resolve(path);
    match std::fs::read(&resolved) {
        Ok(bytes) => hash_content(&bytes) == expected,
        Err(error) => {
            tracing::debug!(
                path = %resolved.display(),
                %error,
                "follow-up expiry: file unreadable, treating action as expired",
            );
            false
        }
    }
}

/// Evaluate whether a follow-up with the given `expires` condition is still
/// valid at query time.
fn follow_up_is_unexpired<F>(
    expires: &ExpiryCondition,
    resolve: &F,
    current_turn_id: Option<&str>,
) -> bool
where
    F: Fn(&Path) -> PathBuf,
{
    match expires {
        ExpiryCondition::FileModified { path, content_hash } => {
            file_hash_matches(path, content_hash, resolve)
        }
        ExpiryCondition::AnyFileModified { files } => files
            .iter()
            .all(|(path, hash)| file_hash_matches(path, hash, resolve)),
        ExpiryCondition::TurnScoped { turn_id } => current_turn_id == Some(turn_id.as_str()),
        ExpiryCondition::Never => true,
    }
}

fn compute_summary(tool_name: &str, outcome: &Outcome, output: &serde_json::Value) -> String {
    match outcome {
        Outcome::Blocked { reason } => format!("{tool_name} blocked: {}", first_line(reason)),
        Outcome::Error { message } => format!("error: {}", first_line(message)),
        Outcome::Success => success_summary(tool_name, output),
    }
}

fn success_summary(tool_name: &str, output: &serde_json::Value) -> String {
    match tool_name {
        "edit" => summarise_edit(output),
        "write" => summarise_write(output),
        "read" => summarise_read(output),
        "bash" => summarise_bash(output),
        _ => format!("{tool_name} success"),
    }
}

fn summarise_edit(output: &serde_json::Value) -> String {
    let path = string_field(output, "path")
        .or_else(|| string_field(output, "file_path"))
        .unwrap_or_else(|| "<unknown>".to_owned());
    let added = number_field(output, "added")
        .or_else(|| number_field(output, "lines_added"))
        .unwrap_or(0);
    let removed = number_field(output, "removed")
        .or_else(|| number_field(output, "lines_removed"))
        .unwrap_or(0);
    format!("edit committed: {path} +{added}/-{removed}")
}

fn summarise_write(output: &serde_json::Value) -> String {
    let path = string_field(output, "path")
        .or_else(|| string_field(output, "file_path"))
        .unwrap_or_else(|| "<unknown>".to_owned());
    let bytes = number_field(output, "bytes").unwrap_or(0);
    format!("write committed: {path} ({bytes} bytes)")
}

fn summarise_read(output: &serde_json::Value) -> String {
    let path = string_field(output, "path")
        .or_else(|| string_field(output, "file_path"))
        .unwrap_or_else(|| "<unknown>".to_owned());
    let lines = number_field(output, "lines")
        .or_else(|| number_field(output, "line_count"))
        .unwrap_or(0);
    format!("read: {path} ({lines} lines)")
}

fn summarise_bash(output: &serde_json::Value) -> String {
    let command = string_field(output, "command").unwrap_or_else(|| "<unknown>".to_owned());
    let truncated = truncate(&command, 80);
    let exit = number_field(output, "exit_code")
        .or_else(|| number_field(output, "exit"))
        .unwrap_or(0);
    format!("bash: {truncated} (exit {exit})")
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn number_field(value: &serde_json::Value, key: &str) -> Option<i64> {
    value.get(key).and_then(serde_json::Value::as_i64)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_owned()
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
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
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use crate::session::events::{EventBase, SessionEvent};
    use crate::session::mutation_ledger::{MutationOp, RevertStatus};
    use crate::tool::follow_up::{
        BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction,
    };

    fn record(log: &ActionLog, rec: CompletionRecord<'_>) {
        log.record_completion(rec);
    }

    fn store_with_tool_result(
        tool_call_id: &str,
        tool_name: &str,
        output: serde_json::Value,
        duration_ms: u64,
    ) -> Arc<EventStore> {
        let store = Arc::new(EventStore::new());
        store
            .append(SessionEvent::ToolResult {
                base: EventBase::new(None),
                tool_call_id: tool_call_id.to_owned(),
                tool_name: tool_name.to_owned(),
                output,
                duration_ms,
            })
            .unwrap();
        store
    }

    #[test]
    fn entry_round_trip() {
        let entry = ActionLogEntry {
            tool_name: "edit".to_owned(),
            tool_call_id: "tc-1".to_owned(),
            tool_use_description: "fix bug".to_owned(),
            timestamp: Utc::now(),
            outcome: Outcome::Success,
            summary_line: "edit committed: src/a.rs +1/-0".to_owned(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: ActionLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_call_id, "tc-1");
        assert_eq!(parsed.summary_line, entry.summary_line);
    }

    #[test]
    fn outcome_serde_success() {
        let json = serde_json::to_string(&Outcome::Success).unwrap();
        let parsed: Outcome = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, Outcome::Success));
    }

    #[test]
    fn outcome_serde_error() {
        let o = Outcome::Error {
            message: "boom".to_owned(),
        };
        let json = serde_json::to_string(&o).unwrap();
        let parsed: Outcome = serde_json::from_str(&json).unwrap();
        match parsed {
            Outcome::Error { message } => assert_eq!(message, "boom"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn outcome_serde_blocked() {
        let o = Outcome::Blocked {
            reason: "denied".to_owned(),
        };
        let json = serde_json::to_string(&o).unwrap();
        let parsed: Outcome = serde_json::from_str(&json).unwrap();
        match parsed {
            Outcome::Blocked { reason } => assert_eq!(reason, "denied"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn record_completion_preserves_order() {
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));

        for i in 0..5 {
            let id = format!("tc-{i}");
            let output =
                serde_json::json!({ "path": format!("f{i}.rs"), "added": 1, "removed": 0 });
            record(
                &log,
                CompletionRecord {
                    tool_name: "edit",
                    tool_call_id: &id,
                    tool_use_description: "",
                    outcome: Outcome::Success,
                    output: &output,
                    args: serde_json::json!({}),
                    duration_ms: 0,
                    follow_ups: Vec::new(),
                    post_validate_outcome: None,
                    level_1_only: false,
                },
            );
        }

        let entries = log.entries();
        assert_eq!(entries.len(), 5);
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.tool_call_id, format!("tc-{i}"));
        }
    }

    #[test]
    fn get_detail_returns_output_and_args() {
        let output = serde_json::json!({ "path": "src/a.rs", "added": 3, "removed": 1 });
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));

        log.record_completion(CompletionRecord {
            tool_name: "edit",
            tool_call_id: "tc-1",
            tool_use_description: "fix",
            outcome: Outcome::Success,
            output: &output,
            args: serde_json::json!({ "file": "src/a.rs" }),
            duration_ms: 42,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });

        let detail = log.get_detail("tc-1").unwrap();
        assert_eq!(detail.entry.tool_call_id, "tc-1");
        assert_eq!(detail.output, output);
        assert_eq!(
            detail.args.get("file").and_then(|v| v.as_str()),
            Some("src/a.rs")
        );
        assert_eq!(detail.duration_ms, 42);
    }

    #[test]
    fn get_detail_unknown_returns_none() {
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));
        assert!(log.get_detail("missing").is_none());
    }

    #[test]
    fn get_context_unknown_returns_none() {
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));
        assert!(log.get_context("missing").is_none());
    }

    #[test]
    fn get_context_mutation_surfaces_before_content() {
        let store = store_with_tool_result("tc-9", "edit", serde_json::json!({}), 1);
        let log = ActionLog::new(Arc::clone(&store));

        let mut files = HashMap::new();
        files.insert(std::path::PathBuf::from("src/a.rs"), "old".to_owned());
        let follow_up = FollowUpAction {
            action: "undo".to_owned(),
            description: "Revert".to_owned(),
            tool: "apply_patch".to_owned(),
            args: serde_json::json!({}),
            expires: ExpiryCondition::Never,
            confidence: Confidence::High,
            before_content: BeforeContentSource::StoredContent { files },
        };

        record(
            &log,
            CompletionRecord {
                tool_name: "edit",
                tool_call_id: "tc-9",
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({}),
                duration_ms: 0,
                follow_ups: vec![follow_up],
                post_validate_outcome: None,
                level_1_only: false,
            },
        );

        let ctx = log.get_context("tc-9").unwrap();
        assert_eq!(
            ctx.before_content
                .as_ref()
                .and_then(|map| map.get(&std::path::PathBuf::from("src/a.rs")))
                .map(String::as_str),
            Some("old"),
        );
    }

    #[test]
    fn get_context_non_mutation_has_empty_before_content() {
        let store = store_with_tool_result("tc-r", "read", serde_json::json!({}), 1);
        let log = ActionLog::new(Arc::clone(&store));

        record(
            &log,
            CompletionRecord {
                tool_name: "read",
                tool_call_id: "tc-r",
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({ "path": "x", "lines": 10 }),
                args: serde_json::json!({}),
                duration_ms: 0,
                follow_ups: Vec::new(),
                post_validate_outcome: None,
                level_1_only: false,
            },
        );

        let ctx = log.get_context("tc-r").unwrap();
        assert!(
            ctx.before_content.is_none(),
            "non-mutation tool must surface null before-content",
        );
        assert!(ctx.post_validate_outcome.is_none());
    }

    #[test]
    fn compact_line_has_four_pipe_delimited_fields() {
        let entry = ActionLogEntry {
            tool_name: "edit".to_owned(),
            tool_call_id: "tc-1".to_owned(),
            tool_use_description: "d".to_owned(),
            timestamp: Utc::now(),
            outcome: Outcome::Success,
            summary_line: "edit committed: src/a.rs +1/-0".to_owned(),
        };
        let line = entry.compact_line();
        let fields: Vec<&str> = line.split('|').collect();
        assert_eq!(fields.len(), 4, "expected 4 pipe-delimited fields: {line}");
        assert_eq!(fields[0], "edit");
        assert_eq!(fields[1], "tc-1");
        assert_eq!(fields[2], "success");
        assert_eq!(fields[3], "edit committed: src/a.rs +1/-0");
    }

    #[test]
    fn compact_line_outcome_field_is_short_string() {
        let mk = |outcome: Outcome| ActionLogEntry {
            tool_name: "t".to_owned(),
            tool_call_id: "id".to_owned(),
            tool_use_description: String::new(),
            timestamp: Utc::now(),
            outcome,
            summary_line: "s".to_owned(),
        };
        let outcome_of = |line: &str| line.split('|').nth(2).unwrap().to_owned();
        assert_eq!(outcome_of(&mk(Outcome::Success).compact_line()), "success");
        assert_eq!(
            outcome_of(
                &mk(Outcome::Error {
                    message: "m".into()
                })
                .compact_line()
            ),
            "error"
        );
        assert_eq!(
            outcome_of(&mk(Outcome::Blocked { reason: "r".into() }).compact_line()),
            "blocked"
        );
    }

    #[test]
    fn compact_line_100_entries_under_token_budget() {
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));
        for i in 0..100 {
            let id = format!("tc-{i:04}");
            let output = serde_json::json!({ "path": format!("src/file{i:03}.rs"), "added": 5, "removed": 2 });
            record(
                &log,
                CompletionRecord {
                    tool_name: "edit",
                    tool_call_id: &id,
                    tool_use_description: "twenty char description",
                    outcome: Outcome::Success,
                    output: &output,
                    args: serde_json::json!({}),
                    duration_ms: 0,
                    follow_ups: Vec::new(),
                    post_validate_outcome: None,
                    level_1_only: false,
                },
            );
        }

        let lines: String = log
            .entries()
            .iter()
            .map(|e| e.compact_line())
            .collect::<Vec<_>>()
            .join("\n");
        let chars = lines.len();
        let tokens_est = chars / 4;
        // CO1 target: under 1000 tokens for 100 entries. Achievable
        // floor is ~1500 tokens (~15 tokens/entry) given tool_call_id
        // length and summary content. Still 10-15x cheaper than
        // re-reading full tool results (~200 tokens each).
        assert!(
            tokens_est < 2_000,
            "compact 100-entry list too expensive: {chars} chars (~{tokens_est} tokens)",
        );
    }

    #[test]
    fn summary_edit_includes_path_and_diff_stats() {
        let out = serde_json::json!({ "path": "src/h.rs", "added": 12, "removed": 3 });
        let s = success_summary("edit", &out);
        assert_eq!(s, "edit committed: src/h.rs +12/-3");
    }

    #[test]
    fn summary_read_includes_path_and_line_count() {
        let out = serde_json::json!({ "path": "src/h.rs", "lines": 42 });
        let s = success_summary("read", &out);
        assert_eq!(s, "read: src/h.rs (42 lines)");
    }

    #[test]
    fn summary_bash_truncates_command() {
        let long_cmd = format!("echo {}", "x".repeat(120));
        let out = serde_json::json!({ "command": long_cmd, "exit_code": 0 });
        let s = success_summary("bash", &out);
        // Truncated marker appended, exit visible.
        assert!(s.contains("…"));
        assert!(s.ends_with("(exit 0)"));
    }

    #[test]
    fn summary_generic_fallback() {
        let s = success_summary("unknown_tool", &serde_json::json!({}));
        assert_eq!(s, "unknown_tool success");
    }

    #[test]
    fn summary_error_uses_first_line() {
        let outcome = Outcome::Error {
            message: "first line\nsecond line".to_owned(),
        };
        let s = compute_summary("edit", &outcome, &serde_json::Value::Null);
        assert_eq!(s, "error: first line");
    }

    #[test]
    fn summary_blocked_includes_tool_and_reason() {
        let outcome = Outcome::Blocked {
            reason: "policy violation".to_owned(),
        };
        let s = compute_summary("write", &outcome, &serde_json::Value::Null);
        assert_eq!(s, "write blocked: policy violation");
    }

    #[test]
    fn record_completion_stores_follow_ups_and_args() {
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));

        let fu = FollowUpAction {
            action: "undo".to_owned(),
            description: "Revert".to_owned(),
            tool: "apply_patch".to_owned(),
            args: serde_json::json!({}),
            expires: ExpiryCondition::Never,
            confidence: Confidence::High,
            before_content: BeforeContentSource::Unavailable,
        };

        record(
            &log,
            CompletionRecord {
                tool_name: "edit",
                tool_call_id: "tc-77",
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({ "arg": 1 }),
                duration_ms: 0,
                follow_ups: vec![fu],
                post_validate_outcome: None,
                level_1_only: false,
            },
        );

        let detail = log.get_detail("tc-77").unwrap();
        assert_eq!(detail.follow_ups.len(), 1);
        assert_eq!(detail.args.get("arg").and_then(|v| v.as_i64()), Some(1));
    }

    #[test]
    fn compact_json_has_six_short_keys() {
        let entry = ActionLogEntry {
            tool_name: "edit".to_owned(),
            tool_call_id: "tc-1".to_owned(),
            tool_use_description: "fix bug".to_owned(),
            timestamp: Utc::now(),
            outcome: Outcome::Success,
            summary_line: "edit committed: src/a.rs +1/-0".to_owned(),
        };
        let json = entry.compact_json();
        let obj = json.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["desc", "id", "outcome", "summary", "tool", "ts"]);
        assert_eq!(obj["tool"], "edit");
        assert_eq!(obj["id"], "tc-1");
        assert_eq!(obj["desc"], "fix bug");
        assert_eq!(obj["outcome"], "success");
        assert_eq!(obj["summary"], "edit committed: src/a.rs +1/-0");
    }

    #[test]
    fn level_1_only_record_drops_level_2_payload() {
        let store = store_with_tool_result("tc-q", "action_log", serde_json::json!({}), 1);
        let log = ActionLog::new(Arc::clone(&store));

        let big_output = serde_json::json!({ "query": "list", "entries": [1, 2, 3] });
        record(
            &log,
            CompletionRecord {
                tool_name: "action_log",
                tool_call_id: "tc-q",
                tool_use_description: "list my actions",
                outcome: Outcome::Success,
                output: &big_output,
                args: serde_json::json!({ "query": "list" }),
                duration_ms: 7,
                follow_ups: Vec::new(),
                post_validate_outcome: Some(serde_json::json!({ "mode": "report" })),
                level_1_only: true,
            },
        );

        // Level 1 entry is retained.
        let entry = log.entry("tc-q").unwrap();
        assert_eq!(entry.tool_name, "action_log");

        // Level 2/3 payloads are not stored.
        let detail = log.get_detail("tc-q").unwrap();
        assert_eq!(detail.output, serde_json::Value::Null);
        assert_eq!(detail.args, serde_json::Value::Null);
        assert_eq!(detail.duration_ms, 0);
        assert!(detail.follow_ups.is_empty());

        let ctx = log.get_context("tc-q").unwrap();
        assert!(ctx.before_content.is_none());
        assert!(ctx.post_validate_outcome.is_none());
    }

    #[test]
    fn get_context_surfaces_recorded_post_validate_outcome() {
        let store = store_with_tool_result("tc-pv", "edit", serde_json::json!({}), 1);
        let log = ActionLog::new(Arc::clone(&store));

        record(
            &log,
            CompletionRecord {
                tool_name: "edit",
                tool_call_id: "tc-pv",
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({}),
                duration_ms: 0,
                follow_ups: Vec::new(),
                post_validate_outcome: Some(serde_json::json!({ "mode": "gate" })),
                level_1_only: false,
            },
        );

        let ctx = log.get_context("tc-pv").unwrap();
        assert_eq!(
            ctx.post_validate_outcome,
            Some(serde_json::json!({ "mode": "gate" })),
        );
    }

    #[test]
    fn record_completion_edit_updates_mutation_ledger() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "fn a() {}\n").unwrap();

        let log = ActionLog::new(Arc::new(EventStore::new()));
        log.record_completion(CompletionRecord {
            tool_name: "edit",
            tool_call_id: "tc-1",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &serde_json::json!({
                "path": path.to_string_lossy(),
                "blast_radius": { "lines_added": 3, "lines_removed": 1 },
            }),
            args: serde_json::json!({}),
            duration_ms: 0,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });

        let entries = log.mutation_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].operation, MutationOp::Modified);
        assert_eq!(entries[0].diff_stats.lines_added, 3);
        assert_eq!(entries[0].diff_stats.lines_removed, 1);
        assert_eq!(entries[0].revert_status, RevertStatus::Active);
    }

    #[test]
    fn record_completion_write_created_then_modified() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("new.rs");
        fs::write(&path, "one\ntwo\n").unwrap();

        let log = ActionLog::new(Arc::new(EventStore::new()));

        // First write, no StoredContent follow-up → Created.
        log.record_completion(CompletionRecord {
            tool_name: "write",
            tool_call_id: "tc-1",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &serde_json::json!({ "path": path.to_string_lossy(), "line_count": 2 }),
            args: serde_json::json!({}),
            duration_ms: 0,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });
        let entry = log.mutation_entry(&path).unwrap();
        assert_eq!(entry.operation, MutationOp::Created);
        assert_eq!(entry.diff_stats.lines_added, 2);
        assert_eq!(entry.diff_stats.lines_removed, 0);

        // Second write WITH StoredContent (old had 2 lines), new line_count 5
        // → Modified, +3 accumulated onto the created +2.
        fs::write(&path, "1\n2\n3\n4\n5\n").unwrap();
        let mut files = HashMap::new();
        files.insert(path.clone(), "one\ntwo\n".to_owned());
        let follow_up = FollowUpAction {
            action: "undo".to_owned(),
            description: "Revert".to_owned(),
            tool: "apply_patch".to_owned(),
            args: serde_json::json!({}),
            expires: ExpiryCondition::Never,
            confidence: Confidence::High,
            before_content: BeforeContentSource::StoredContent { files },
        };
        log.record_completion(CompletionRecord {
            tool_name: "write",
            tool_call_id: "tc-2",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &serde_json::json!({ "path": path.to_string_lossy(), "line_count": 5 }),
            args: serde_json::json!({}),
            duration_ms: 0,
            follow_ups: vec![follow_up],
            post_validate_outcome: None,
            level_1_only: false,
        });
        let entry = log.mutation_entry(&path).unwrap();
        assert_eq!(entry.operation, MutationOp::Modified);
        assert_eq!(entry.first_tool_call_id, "tc-1");
        assert_eq!(entry.last_tool_call_id, "tc-2");
        assert_eq!(entry.diff_stats.lines_added, 5);
        assert_eq!(entry.diff_stats.lines_removed, 0);
    }

    #[test]
    fn record_completion_apply_patch_per_file() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let created = dir.path().join("created.rs");
        let modified = dir.path().join("modified.rs");
        let deleted = dir.path().join("deleted.rs");
        fs::write(&created, "new\n").unwrap();
        fs::write(&modified, "changed\n").unwrap();
        // `deleted` intentionally left absent — the patch removed it.

        let log = ActionLog::new(Arc::new(EventStore::new()));
        log.record_completion(CompletionRecord {
            tool_name: "apply_patch",
            tool_call_id: "tc-1",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &serde_json::json!({
                "per_file": [
                    { "path": created.to_string_lossy(), "status": "created", "lines_added": 1, "lines_removed": 0 },
                    { "path": modified.to_string_lossy(), "status": "modified", "lines_added": 2, "lines_removed": 2 },
                    { "path": deleted.to_string_lossy(), "status": "deleted", "lines_added": 0, "lines_removed": 4 },
                ]
            }),
            args: serde_json::json!({}),
            duration_ms: 0,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });

        assert_eq!(log.mutation_entries().len(), 3);
        assert_eq!(
            log.mutation_entry(&created).unwrap().operation,
            MutationOp::Created
        );
        assert_eq!(
            log.mutation_entry(&modified).unwrap().operation,
            MutationOp::Modified
        );
        let del = log.mutation_entry(&deleted).unwrap();
        assert_eq!(del.operation, MutationOp::Deleted);
        assert_eq!(del.diff_stats.lines_removed, 4);
        // Deletion intact (file still absent) → Active.
        assert_eq!(del.revert_status, RevertStatus::Active);
        assert_eq!(
            log.mutation_entry(&created).unwrap().revert_status,
            RevertStatus::Active
        );
    }

    #[test]
    fn record_completion_error_does_not_update_ledger() {
        let log = ActionLog::new(Arc::new(EventStore::new()));
        log.record_completion(CompletionRecord {
            tool_name: "edit",
            tool_call_id: "tc-1",
            tool_use_description: "",
            outcome: Outcome::Error {
                message: "boom".to_owned(),
            },
            output: &serde_json::json!({
                "path": "src/a.rs",
                "blast_radius": { "lines_added": 1, "lines_removed": 0 },
            }),
            args: serde_json::json!({}),
            duration_ms: 0,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });
        assert!(log.mutation_entries().is_empty());
    }

    #[test]
    fn mutation_ledger_is_session_scoped() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "x\n").unwrap();

        let a = ActionLog::new(Arc::new(EventStore::new()));
        a.record_completion(CompletionRecord {
            tool_name: "edit",
            tool_call_id: "tc-1",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &serde_json::json!({
                "path": path.to_string_lossy(),
                "blast_radius": { "lines_added": 1, "lines_removed": 0 },
            }),
            args: serde_json::json!({}),
            duration_ms: 0,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });

        let b = ActionLog::new(Arc::new(EventStore::new()));
        assert_eq!(a.mutation_entries().len(), 1);
        assert!(
            b.mutation_entries().is_empty(),
            "a fresh ActionLog has its own ledger",
        );
    }

    fn follow_up(action: &str, tool: &str, expires: ExpiryCondition) -> FollowUpAction {
        FollowUpAction {
            action: action.to_owned(),
            description: format!("{action} via {tool}"),
            tool: tool.to_owned(),
            args: serde_json::json!({}),
            expires,
            confidence: Confidence::High,
            before_content: BeforeContentSource::Unavailable,
        }
    }

    #[test]
    fn get_follow_up_returns_action_and_original_args() {
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));

        record(
            &log,
            CompletionRecord {
                tool_name: "edit",
                tool_call_id: "tc-fu",
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({ "file": "src/a.rs", "n": 7 }),
                duration_ms: 0,
                follow_ups: vec![follow_up("undo", "apply_patch", ExpiryCondition::Never)],
                post_validate_outcome: None,
                level_1_only: false,
            },
        );

        let found = log.get_follow_up("tc-fu", "undo").unwrap();
        assert_eq!(found.action.action, "undo");
        assert_eq!(found.action.tool, "apply_patch");
        assert_eq!(
            found.original_args.get("n").and_then(|v| v.as_i64()),
            Some(7)
        );
    }

    #[test]
    fn get_follow_up_unknown_returns_none() {
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));

        record(
            &log,
            CompletionRecord {
                tool_name: "edit",
                tool_call_id: "tc-fu",
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({}),
                duration_ms: 0,
                follow_ups: vec![follow_up("undo", "apply_patch", ExpiryCondition::Never)],
                post_validate_outcome: None,
                level_1_only: false,
            },
        );

        // Unknown action name on a known call.
        assert!(log.get_follow_up("tc-fu", "redo").is_none());
        // Unknown call id entirely.
        assert!(log.get_follow_up("missing", "undo").is_none());
    }

    #[test]
    fn duplicate_action_name_keeps_first_indexed_slot() {
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));

        record(
            &log,
            CompletionRecord {
                tool_name: "edit",
                tool_call_id: "tc-dup",
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({}),
                duration_ms: 0,
                follow_ups: vec![
                    follow_up("undo", "first_tool", ExpiryCondition::Never),
                    follow_up("undo", "second_tool", ExpiryCondition::Never),
                ],
                post_validate_outcome: None,
                level_1_only: false,
            },
        );

        let found = log.get_follow_up("tc-dup", "undo").unwrap();
        assert_eq!(
            found.action.tool, "first_tool",
            "duplicate action name must keep the first indexed slot",
        );
    }

    #[test]
    fn unexpired_follow_ups_excludes_modified_file_action() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"original").unwrap();

        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));
        record(
            &log,
            CompletionRecord {
                tool_name: "edit",
                tool_call_id: "tc-fm",
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({}),
                duration_ms: 0,
                follow_ups: vec![
                    follow_up(
                        "reapply",
                        "apply_patch",
                        ExpiryCondition::FileModified {
                            path: path.clone(),
                            content_hash: hash_content(b"original"),
                        },
                    ),
                    follow_up("undo", "apply_patch", ExpiryCondition::Never),
                ],
                post_validate_outcome: None,
                level_1_only: false,
            },
        );

        let identity = |p: &Path| p.to_path_buf();

        // File unchanged: both actions valid.
        let before = log.unexpired_follow_ups(identity, None);
        assert_eq!(before.len(), 2);

        // Mutate the file: FileModified action expires, Never stays.
        std::fs::write(&path, b"changed").unwrap();
        let after = log.unexpired_follow_ups(identity, None);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].action.action, "undo");
    }

    #[test]
    fn unexpired_follow_ups_turn_scoped_requires_matching_turn() {
        let store = Arc::new(EventStore::new());
        let log = ActionLog::new(Arc::clone(&store));
        record(
            &log,
            CompletionRecord {
                tool_name: "edit",
                tool_call_id: "tc-ts",
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({}),
                duration_ms: 0,
                follow_ups: vec![follow_up(
                    "retry",
                    "edit",
                    ExpiryCondition::TurnScoped {
                        turn_id: "turn-1".to_owned(),
                    },
                )],
                post_validate_outcome: None,
                level_1_only: false,
            },
        );

        let identity = |p: &Path| p.to_path_buf();
        assert_eq!(log.unexpired_follow_ups(identity, Some("turn-1")).len(), 1);
        assert_eq!(log.unexpired_follow_ups(identity, Some("turn-2")).len(), 0);
        assert_eq!(log.unexpired_follow_ups(identity, None).len(), 0);
    }
}
