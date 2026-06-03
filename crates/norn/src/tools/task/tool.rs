//! The `task` tool: CRUD plus hierarchy and group operations.
//!
//! [`TaskTool`] dispatches over a [`TaskStore`] resolved from the
//! [`ToolContext`] extension map. Effect is `Write` because most actions
//! mutate the store and `ToolEffect` is a single value per tool — picking
//! `Write` keeps task operations serialised under the scheduler.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::memory::{InMemoryTaskStore, SharedTaskStore};
use super::rollup::effective_status;
use super::types::{TaskEntry, TaskStatus, TaskStore, parse_status};
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// CRUD-plus-hierarchy tool over [`TaskStore`].
pub struct TaskTool;

impl TaskTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for TaskTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct TaskArgs {
    action: String,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    depends_on: Option<Vec<String>>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    parent_task_id: Option<String>,
    #[serde(default)]
    agent_path: Option<String>,
    #[serde(default)]
    group_slug: Option<String>,
}

fn entry_to_json(entry: &TaskEntry) -> Value {
    serde_json::to_value(entry).unwrap_or(Value::Null)
}

fn store_from(ctx: &ToolContext) -> Result<Arc<dyn TaskStore>, ToolError> {
    if let Some(shared) = ctx.get_extension::<SharedTaskStore>() {
        return Ok(Arc::clone(&shared.0));
    }
    if let Some(concrete) = ctx.get_extension::<InMemoryTaskStore>() {
        let trait_store: Arc<dyn TaskStore> = concrete;
        return Ok(trait_store);
    }
    Err(ToolError::ExecutionFailed {
        reason: "task store not configured in tool context".to_string(),
    })
}

fn require<'a>(value: Option<&'a str>, action: &str, field: &str) -> Result<&'a str, ToolError> {
    value.ok_or_else(|| ToolError::ExecutionFailed {
        reason: format!("{action} requires `{field}`"),
    })
}

/// Build a fresh [`TaskEntry`] from create-style arguments.
fn build_entry(args: &TaskArgs, description: String) -> Result<TaskEntry, ToolError> {
    let now = Utc::now();
    let id = args
        .task_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let status = match args.status.as_deref() {
        Some(s) => parse_status(s)?,
        None => TaskStatus::Pending,
    };
    Ok(TaskEntry {
        id,
        description,
        status,
        depends_on: args.depends_on.clone().unwrap_or_default(),
        metadata: args.metadata.clone().unwrap_or(Value::Null),
        created_at: now,
        updated_at: now,
        parent_task_id: None,
        assigned_agent: None,
    })
}

/// Serialise a child with its roll-up status substituted in.
///
/// The stored [`TaskEntry`] is never mutated — only the JSON view carries
/// the effective status computed from the child's own children.
fn child_with_rollup(store: &dyn TaskStore, child: &TaskEntry) -> Value {
    let grandchild_statuses: Vec<_> = store
        .children(&child.id)
        .iter()
        .map(|gc| gc.status)
        .collect();
    let rolled = effective_status(&grandchild_statuses, child.status);
    let mut json = entry_to_json(child);
    if let Value::Object(map) = &mut json
        && let Ok(status_value) = serde_json::to_value(rolled)
    {
        map.insert("status".to_string(), status_value);
    }
    json
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/task.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::TaskManagement
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/task.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "create", "get", "list", "update", "complete",
                        "create_subtask", "children", "ancestors", "claim",
                        "create_group", "list_groups"
                    ],
                    "description": "Operation to perform."
                },
                "task_id": {
                    "type": "string",
                    "description": "Task identifier. Required for get, update, complete, ancestors, and claim."
                },
                "description": {
                    "type": "string",
                    "description": "Human-readable task description. Required for create and create_subtask, optional for update."
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed", "blocked", "failed"],
                    "description": "Task lifecycle status. Used to filter in list or set in create/update."
                },
                "depends_on": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "IDs of tasks that must complete before this one can start."
                },
                "metadata": {
                    "type": "object",
                    "description": "Free-form structured metadata to attach to the task."
                },
                "parent_task_id": {
                    "type": "string",
                    "description": "Parent task id. Required for create_subtask and children."
                },
                "agent_path": {
                    "type": "string",
                    "description": "Registry path of the claiming agent. Required for claim."
                },
                "group_slug": {
                    "type": "string",
                    "description": "Human-readable task group slug. Required for create_group."
                }
            },
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Write
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let started = Instant::now();
        let args: TaskArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;
        let store = store_from(ctx)?;

        let content = match args.action.as_str() {
            "create" => {
                let description =
                    require(args.description.as_deref(), "create", "description")?.to_string();
                let entry = build_entry(&args, description)?;
                store.create(entry.clone())?;
                serde_json::json!({ "action": "create", "task": entry_to_json(&entry) })
            }
            "get" => {
                let id = require(args.task_id.as_deref(), "get", "task_id")?.to_string();
                match store.get(&id) {
                    Some(entry) => {
                        serde_json::json!({ "action": "get", "task": entry_to_json(&entry) })
                    }
                    None => {
                        return Ok(ToolOutput {
                            content: serde_json::json!({
                                "action": "get",
                                "task_id": id,
                                "error": "not found"
                            }),
                            is_error: true,
                            duration: started.elapsed(),
                        });
                    }
                }
            }
            "list" => {
                let filter = match args.status.as_deref() {
                    Some(s) => Some(parse_status(s)?),
                    None => None,
                };
                let items: Vec<Value> = store.list(filter).iter().map(entry_to_json).collect();
                serde_json::json!({ "action": "list", "tasks": items })
            }
            "update" => {
                let id = require(args.task_id.as_deref(), "update", "task_id")?.to_string();
                let status = match args.status.as_deref() {
                    Some(s) => Some(parse_status(s)?),
                    None => None,
                };
                let entry = store.update(
                    &id,
                    status,
                    args.description.clone(),
                    args.depends_on.clone(),
                    args.metadata.clone(),
                )?;
                serde_json::json!({ "action": "update", "task": entry_to_json(&entry) })
            }
            "complete" => {
                let id = require(args.task_id.as_deref(), "complete", "task_id")?.to_string();
                let entry = store.complete(&id)?;
                serde_json::json!({ "action": "complete", "task": entry_to_json(&entry) })
            }
            "create_subtask" => {
                let parent_id = require(
                    args.parent_task_id.as_deref(),
                    "create_subtask",
                    "parent_task_id",
                )?
                .to_string();
                let description =
                    require(args.description.as_deref(), "create_subtask", "description")?
                        .to_string();
                let mut entry = build_entry(&args, description)?;
                entry.parent_task_id = Some(parent_id.clone());
                store.create_subtask(&parent_id, entry.clone())?;
                serde_json::json!({
                    "action": "create_subtask",
                    "task": entry_to_json(&entry)
                })
            }
            "children" => {
                let parent_id =
                    require(args.parent_task_id.as_deref(), "children", "parent_task_id")?
                        .to_string();
                let items: Vec<Value> = store
                    .children(&parent_id)
                    .iter()
                    .map(|child| child_with_rollup(store.as_ref(), child))
                    .collect();
                serde_json::json!({
                    "action": "children",
                    "parent_task_id": parent_id,
                    "tasks": items
                })
            }
            "ancestors" => {
                let id = require(args.task_id.as_deref(), "ancestors", "task_id")?.to_string();
                let chain: Vec<Value> = store.ancestors(&id)?.iter().map(entry_to_json).collect();
                serde_json::json!({ "action": "ancestors", "task_id": id, "tasks": chain })
            }
            "claim" => {
                let id = require(args.task_id.as_deref(), "claim", "task_id")?.to_string();
                let agent_path =
                    require(args.agent_path.as_deref(), "claim", "agent_path")?.to_string();
                let entry = store.claim(&id, &agent_path)?;
                serde_json::json!({ "action": "claim", "task": entry_to_json(&entry) })
            }
            "create_group" => {
                let slug =
                    require(args.group_slug.as_deref(), "create_group", "group_slug")?.to_string();
                store.create_group(&slug)?;
                serde_json::json!({ "action": "create_group", "group_slug": slug })
            }
            "list_groups" => {
                serde_json::json!({ "action": "list_groups", "groups": store.list_groups() })
            }
            other => {
                return Err(ToolError::ExecutionFailed {
                    reason: format!("unknown action '{other}'"),
                });
            }
        };

        Ok(ToolOutput {
            content,
            is_error: false,
            duration: started.elapsed(),
        })
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
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};
    use crate::tools::task::TaskStatus;

    fn envelope_for(args: Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "task".to_string(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: Value::Null,
        }
    }

    fn ctx_with_store() -> (ToolContext, Arc<InMemoryTaskStore>) {
        let store = Arc::new(InMemoryTaskStore::new());
        let ctx = ToolContext::empty();
        let shared = Arc::new(SharedTaskStore(Arc::clone(&store) as Arc<dyn TaskStore>));
        ctx.insert_extension(shared);
        (ctx, store)
    }

    #[tokio::test]
    async fn create_list_update_complete_full_cycle() {
        let tool = TaskTool::new();
        let (ctx, store) = ctx_with_store();

        for i in 0..3 {
            let env = envelope_for(json!({
                "action": "create",
                "description": format!("task {i}"),
            }));
            let out = tool.execute(&env, &ctx).await.unwrap();
            assert!(!out.is_error, "create {i}: {:?}", out.content);
            assert_eq!(out.content["task"]["status"], "pending");
        }

        let env = envelope_for(json!({"action": "list", "status": "pending"}));
        let out = tool.execute(&env, &ctx).await.unwrap();
        let listed = out.content["tasks"].as_array().unwrap();
        assert_eq!(listed.len(), 3);

        let target_id = listed[0]["id"].as_str().unwrap().to_string();
        let env = envelope_for(json!({
            "action": "update",
            "task_id": target_id,
            "status": "in_progress",
        }));
        let out = tool.execute(&env, &ctx).await.unwrap();
        assert_eq!(out.content["task"]["status"], "in_progress");

        let second_id = listed[1]["id"].as_str().unwrap().to_string();
        let env = envelope_for(json!({"action": "complete", "task_id": second_id}));
        let out = tool.execute(&env, &ctx).await.unwrap();
        assert_eq!(out.content["task"]["status"], "completed");

        let final_state = store.list(None);
        assert_eq!(final_state.len(), 3);
        let in_prog = final_state
            .iter()
            .filter(|e| e.status == TaskStatus::InProgress)
            .count();
        let done = final_state
            .iter()
            .filter(|e| e.status == TaskStatus::Completed)
            .count();
        assert_eq!(in_prog, 1);
        assert_eq!(done, 1);
    }

    #[tokio::test]
    async fn list_filters_by_status() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();

        tool.execute(
            &envelope_for(json!({"action": "create", "description": "a"})),
            &ctx,
        )
        .await
        .unwrap();
        let out = tool
            .execute(
                &envelope_for(json!({"action": "create", "description": "b"})),
                &ctx,
            )
            .await
            .unwrap();
        let b_id = out.content["task"]["id"].as_str().unwrap().to_string();
        tool.execute(
            &envelope_for(json!({"action": "complete", "task_id": b_id})),
            &ctx,
        )
        .await
        .unwrap();

        let out = tool
            .execute(
                &envelope_for(json!({"action": "list", "status": "pending"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["tasks"].as_array().unwrap().len(), 1);
        let out = tool
            .execute(
                &envelope_for(json!({"action": "list", "status": "completed"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["tasks"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn update_missing_task_errors() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();
        let env = envelope_for(json!({
            "action": "update",
            "task_id": "ghost",
            "status": "in_progress"
        }));
        let err = tool.execute(&env, &ctx).await.expect_err("ghost");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn missing_store_returns_execution_failed() {
        let tool = TaskTool::new();
        let ctx = ToolContext::empty();
        let env = envelope_for(json!({"action": "list"}));
        let err = tool.execute(&env, &ctx).await.expect_err("no store");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(reason.contains("task store"), "{reason}");
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hierarchy_end_to_end() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();

        // Create a named task group.
        let out = tool
            .execute(
                &envelope_for(json!({
                    "action": "create_group",
                    "group_slug": "norn-agents-wiring"
                })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["group_slug"], "norn-agents-wiring");

        let out = tool
            .execute(&envelope_for(json!({"action": "list_groups"})), &ctx)
            .await
            .unwrap();
        assert_eq!(out.content["groups"][0], "norn-agents-wiring");

        // Create a parent task.
        let out = tool
            .execute(
                &envelope_for(json!({
                    "action": "create",
                    "task_id": "parent",
                    "description": "parent work"
                })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["task"]["id"], "parent");

        // Create three subtasks under the parent.
        for i in 0..3 {
            let out = tool
                .execute(
                    &envelope_for(json!({
                        "action": "create_subtask",
                        "parent_task_id": "parent",
                        "task_id": format!("child-{i}"),
                        "description": format!("subtask {i}")
                    })),
                    &ctx,
                )
                .await
                .unwrap();
            assert_eq!(out.content["task"]["parent_task_id"], "parent");
        }

        // Claim one subtask.
        let out = tool
            .execute(
                &envelope_for(json!({
                    "action": "claim",
                    "task_id": "child-0",
                    "agent_path": "root/worker"
                })),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["task"]["assigned_agent"], "root/worker");

        // A second claim on the same task fails.
        let err = tool
            .execute(
                &envelope_for(json!({
                    "action": "claim",
                    "task_id": "child-0",
                    "agent_path": "root/other"
                })),
                &ctx,
            )
            .await
            .expect_err("double claim");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));

        // Move one child to in_progress so the parent rolls up to in_progress.
        tool.execute(
            &envelope_for(json!({
                "action": "update",
                "task_id": "child-1",
                "status": "in_progress"
            })),
            &ctx,
        )
        .await
        .unwrap();

        // List children of the parent.
        let out = tool
            .execute(
                &envelope_for(json!({
                    "action": "children",
                    "parent_task_id": "parent"
                })),
                &ctx,
            )
            .await
            .unwrap();
        let children = out.content["tasks"].as_array().unwrap();
        assert_eq!(children.len(), 3);

        // Ancestors of a child walk back to the root parent.
        let out = tool
            .execute(
                &envelope_for(json!({
                    "action": "ancestors",
                    "task_id": "child-2"
                })),
                &ctx,
            )
            .await
            .unwrap();
        let chain = out.content["tasks"].as_array().unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0]["id"], "child-2");
        assert_eq!(chain[1]["id"], "parent");
    }

    #[tokio::test]
    async fn children_action_applies_rollup_status() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();

        // parent -> mid -> leaf, with leaf failed so mid rolls up to failed.
        tool.execute(
            &envelope_for(json!({
                "action": "create", "task_id": "parent", "description": "p"
            })),
            &ctx,
        )
        .await
        .unwrap();
        tool.execute(
            &envelope_for(json!({
                "action": "create_subtask", "parent_task_id": "parent",
                "task_id": "mid", "description": "m"
            })),
            &ctx,
        )
        .await
        .unwrap();
        tool.execute(
            &envelope_for(json!({
                "action": "create_subtask", "parent_task_id": "mid",
                "task_id": "leaf", "description": "l", "status": "failed"
            })),
            &ctx,
        )
        .await
        .unwrap();

        let out = tool
            .execute(
                &envelope_for(json!({"action": "children", "parent_task_id": "parent"})),
                &ctx,
            )
            .await
            .unwrap();
        let children = out.content["tasks"].as_array().unwrap();
        assert_eq!(children.len(), 1);
        // `mid` is stored as pending but rolls up to failed via its leaf.
        assert_eq!(children[0]["id"], "mid");
        assert_eq!(children[0]["status"], "failed");
    }
}
