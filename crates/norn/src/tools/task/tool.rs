//! The `task` tool: CRUD plus hierarchy and group operations.
//!
//! [`TaskTool`] is a [`CompositeTool`]: its operations are the
//! [`TaskCommand`] enum (internally tagged on `action`), so the input
//! schema, per-command catalog entries, per-call effects, and
//! invalid-command handling are all derived rather than hand-maintained.
//! Read commands (`get`, `list`, `children`, `ancestors`, `list_groups`)
//! classify as `ReadOnly` and may be scheduled concurrently; mutating
//! commands classify as `Write`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use norn_macros::ToolArgs;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::memory::{InMemoryTaskStore, SharedTaskStore};
use super::rollup::effective_status;
use super::types::{TaskEntry, TaskStatus, TaskStore};
use crate::error::ToolError;
use crate::tool::composite::CompositeTool;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{ToolCategory, ToolOutput};

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

/// One `task` operation, dispatched on `action`.
#[derive(Debug, Deserialize, ToolArgs)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum TaskCommand {
    /// Create a new task.
    Create {
        /// Human-readable task description.
        description: String,
        /// Explicit task identifier; a UUID is generated when omitted.
        task_id: Option<String>,
        /// Initial lifecycle status; defaults to pending.
        status: Option<TaskStatus>,
        /// IDs of tasks that must complete before this one can start.
        depends_on: Option<Vec<String>>,
        /// Free-form structured metadata to attach to the task.
        metadata: Option<Value>,
    },
    /// Retrieve a task by id.
    Get {
        /// Task identifier.
        task_id: String,
    },
    /// List tasks, optionally filtered by status.
    List {
        /// Filter to tasks with this lifecycle status.
        status: Option<TaskStatus>,
    },
    /// Update fields of an existing task; omitted fields are untouched.
    Update {
        /// Task identifier.
        task_id: String,
        /// New lifecycle status.
        status: Option<TaskStatus>,
        /// New task description.
        description: Option<String>,
        /// Replacement dependency list.
        depends_on: Option<Vec<String>>,
        /// Replacement metadata.
        metadata: Option<Value>,
    },
    /// Mark a task as completed.
    Complete {
        /// Task identifier.
        task_id: String,
    },
    /// Create a task as a child of an existing parent task.
    CreateSubtask {
        /// Parent task id.
        parent_task_id: String,
        /// Human-readable task description.
        description: String,
        /// Explicit task identifier; a UUID is generated when omitted.
        task_id: Option<String>,
        /// Initial lifecycle status; defaults to pending.
        status: Option<TaskStatus>,
        /// IDs of tasks that must complete before this one can start.
        depends_on: Option<Vec<String>>,
        /// Free-form structured metadata to attach to the task.
        metadata: Option<Value>,
    },
    /// List the direct children of a parent task with roll-up status.
    Children {
        /// Parent task id.
        parent_task_id: String,
    },
    /// Walk the parent chain from a task to its root, inclusive.
    Ancestors {
        /// Task identifier.
        task_id: String,
    },
    /// Atomically assign an agent to an unclaimed task.
    Claim {
        /// Task identifier.
        task_id: String,
        /// Registry path of the claiming agent.
        agent_path: String,
    },
    /// Create a named task group.
    CreateGroup {
        /// Human-readable task group slug.
        group_slug: String,
    },
    /// List all known task group slugs.
    ListGroups,
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
    Err(ToolError::MissingExtension {
        extension: std::any::type_name::<SharedTaskStore>().to_string(),
    })
}

/// Fields shared by the two create-style commands.
struct CreateFields {
    description: String,
    task_id: Option<String>,
    status: Option<TaskStatus>,
    depends_on: Option<Vec<String>>,
    metadata: Option<Value>,
}

/// Build a fresh [`TaskEntry`] from create-style fields.
fn build_entry(fields: CreateFields) -> TaskEntry {
    let now = Utc::now();
    TaskEntry {
        id: fields.task_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        description: fields.description,
        status: fields.status.unwrap_or(TaskStatus::Pending),
        depends_on: fields.depends_on.unwrap_or_default(),
        metadata: fields.metadata.unwrap_or(Value::Null),
        created_at: now,
        updated_at: now,
        parent_task_id: None,
        assigned_agent: None,
    }
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
impl CompositeTool for TaskTool {
    type Command = TaskCommand;

    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/task.description.md")
    }

    fn command_field(&self) -> &'static str {
        "action"
    }

    fn input_schema(&self) -> Value {
        TaskCommand::json_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::TaskManagement
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/task.usage.md"))
    }

    fn command_effect(&self, command: &TaskCommand) -> ToolEffect {
        match command {
            TaskCommand::Get { .. }
            | TaskCommand::List { .. }
            | TaskCommand::Children { .. }
            | TaskCommand::Ancestors { .. }
            | TaskCommand::ListGroups => ToolEffect::ReadOnly,
            TaskCommand::Create { .. }
            | TaskCommand::Update { .. }
            | TaskCommand::Complete { .. }
            | TaskCommand::CreateSubtask { .. }
            | TaskCommand::Claim { .. }
            | TaskCommand::CreateGroup { .. } => ToolEffect::Write,
        }
    }

    fn conservative_effect(&self) -> ToolEffect {
        ToolEffect::Write
    }

    async fn run(
        &self,
        command: TaskCommand,
        _envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let store = store_from(ctx)?;

        let content = match command {
            TaskCommand::Create {
                description,
                task_id,
                status,
                depends_on,
                metadata,
            } => {
                let entry = build_entry(CreateFields {
                    description,
                    task_id,
                    status,
                    depends_on,
                    metadata,
                });
                store.create(entry.clone())?;
                serde_json::json!({ "action": "create", "task": entry_to_json(&entry) })
            }
            TaskCommand::Get { task_id } => match store.get(&task_id) {
                Some(entry) => {
                    serde_json::json!({ "action": "get", "task": entry_to_json(&entry) })
                }
                None => {
                    return Ok(ToolOutput::failure_with_content(
                        serde_json::json!({ "action": "get", "task_id": task_id }),
                        ToolErrorPayload::new(
                            ToolErrorKind::NotFound,
                            format!("task '{task_id}' not found"),
                        )
                        .with_detail(serde_json::json!({ "task_id": task_id })),
                    ));
                }
            },
            TaskCommand::List { status } => {
                let items: Vec<Value> = store.list(status).iter().map(entry_to_json).collect();
                serde_json::json!({ "action": "list", "tasks": items })
            }
            TaskCommand::Update {
                task_id,
                status,
                description,
                depends_on,
                metadata,
            } => {
                let entry = store.update(&task_id, status, description, depends_on, metadata)?;
                serde_json::json!({ "action": "update", "task": entry_to_json(&entry) })
            }
            TaskCommand::Complete { task_id } => {
                let entry = store.complete(&task_id)?;
                serde_json::json!({ "action": "complete", "task": entry_to_json(&entry) })
            }
            TaskCommand::CreateSubtask {
                parent_task_id,
                description,
                task_id,
                status,
                depends_on,
                metadata,
            } => {
                let mut entry = build_entry(CreateFields {
                    description,
                    task_id,
                    status,
                    depends_on,
                    metadata,
                });
                entry.parent_task_id = Some(parent_task_id.clone());
                store.create_subtask(&parent_task_id, entry.clone())?;
                serde_json::json!({
                    "action": "create_subtask",
                    "task": entry_to_json(&entry)
                })
            }
            TaskCommand::Children { parent_task_id } => {
                let items: Vec<Value> = store
                    .children(&parent_task_id)
                    .iter()
                    .map(|child| child_with_rollup(store.as_ref(), child))
                    .collect();
                serde_json::json!({
                    "action": "children",
                    "parent_task_id": parent_task_id,
                    "tasks": items
                })
            }
            TaskCommand::Ancestors { task_id } => {
                let chain: Vec<Value> = store
                    .ancestors(&task_id)?
                    .iter()
                    .map(entry_to_json)
                    .collect();
                serde_json::json!({ "action": "ancestors", "task_id": task_id, "tasks": chain })
            }
            TaskCommand::Claim {
                task_id,
                agent_path,
            } => {
                let entry = store.claim(&task_id, &agent_path)?;
                serde_json::json!({ "action": "claim", "task": entry_to_json(&entry) })
            }
            TaskCommand::CreateGroup { group_slug } => {
                store.create_group(&group_slug)?;
                serde_json::json!({ "action": "create_group", "group_slug": group_slug })
            }
            TaskCommand::ListGroups => {
                serde_json::json!({ "action": "list_groups", "groups": store.list_groups() })
            }
        };

        Ok(ToolOutput::success(content))
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
    use crate::tool::traits::Tool;
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

    fn as_tool(tool: &TaskTool) -> &dyn Tool {
        tool
    }

    async fn execute(tool: &TaskTool, args: Value, ctx: &ToolContext) -> ToolOutput {
        as_tool(tool)
            .execute(&envelope_for(args), ctx)
            .await
            .unwrap()
    }

    // -- Composite derivation -------------------------------------------

    #[test]
    fn schema_is_derived_one_of_with_per_command_required_fields() {
        let tool = TaskTool::new();
        let schema = as_tool(&tool).input_schema();
        // OpenAI rejects function schemas whose root is not `type: "object"`
        // (regression: HTTP 400 invalid_function_parameters on every request
        // carrying this tool).
        assert_eq!(schema["type"], "object");
        let variants = schema["oneOf"].as_array().expect("oneOf array");
        assert_eq!(variants.len(), 11);

        let create = variants
            .iter()
            .find(|v| v["properties"]["action"]["const"] == "create")
            .expect("create variant");
        assert_eq!(create["description"], "Create a new task.");
        assert_eq!(create["required"], json!(["action", "description"]));
        // Status delegates to TaskStatus's derived string-enum schema.
        assert_eq!(
            create["properties"]["status"]["enum"],
            json!(["pending", "in_progress", "completed", "blocked", "failed"])
        );

        let claim = variants
            .iter()
            .find(|v| v["properties"]["action"]["const"] == "claim")
            .expect("claim variant");
        assert_eq!(
            claim["required"],
            json!(["action", "task_id", "agent_path"])
        );
    }

    #[test]
    fn per_command_effects_split_reads_from_writes() {
        let tool = TaskTool::new();
        let dyn_tool = as_tool(&tool);
        assert_eq!(dyn_tool.effect(), ToolEffect::Write);

        for read in [
            json!({"action": "get", "task_id": "t"}),
            json!({"action": "list"}),
            json!({"action": "children", "parent_task_id": "p"}),
            json!({"action": "ancestors", "task_id": "t"}),
            json!({"action": "list_groups"}),
        ] {
            assert_eq!(
                dyn_tool.effect_for_args(&read),
                ToolEffect::ReadOnly,
                "read command must classify ReadOnly: {read}",
            );
        }
        for write in [
            json!({"action": "create", "description": "d"}),
            json!({"action": "update", "task_id": "t"}),
            json!({"action": "complete", "task_id": "t"}),
            json!({"action": "claim", "task_id": "t", "agent_path": "a"}),
            json!({"action": "create_group", "group_slug": "g"}),
        ] {
            assert_eq!(
                dyn_tool.effect_for_args(&write),
                ToolEffect::Write,
                "mutating command must classify Write: {write}",
            );
        }
        // Unknown command / malformed args → conservative Write.
        assert_eq!(
            dyn_tool.effect_for_args(&json!({"action": "explode"})),
            ToolEffect::Write,
        );
    }

    /// Contract pin (doc-mandated for every `CompositeTool` impl): the
    /// conservative effect covers every command's effect, one constructed
    /// value per `TaskCommand` variant. Adding a variant without listing
    /// it here is caught by the exhaustive `command_effect` match; adding
    /// it here with a wider effect than `conservative_effect` fails this
    /// test.
    #[test]
    fn conservative_effect_covers_every_command() {
        crate::tool::composite::assert_conservative_effect_covers_all_commands(
            &TaskTool::new(),
            [
                TaskCommand::Create {
                    description: "d".to_owned(),
                    task_id: None,
                    status: None,
                    depends_on: None,
                    metadata: None,
                },
                TaskCommand::Get {
                    task_id: "t".to_owned(),
                },
                TaskCommand::List { status: None },
                TaskCommand::Update {
                    task_id: "t".to_owned(),
                    status: None,
                    description: None,
                    depends_on: None,
                    metadata: None,
                },
                TaskCommand::Complete {
                    task_id: "t".to_owned(),
                },
                TaskCommand::CreateSubtask {
                    parent_task_id: "p".to_owned(),
                    description: "d".to_owned(),
                    task_id: None,
                    status: None,
                    depends_on: None,
                    metadata: None,
                },
                TaskCommand::Children {
                    parent_task_id: "p".to_owned(),
                },
                TaskCommand::Ancestors {
                    task_id: "t".to_owned(),
                },
                TaskCommand::Claim {
                    task_id: "t".to_owned(),
                    agent_path: "/a".to_owned(),
                },
                TaskCommand::CreateGroup {
                    group_slug: "g".to_owned(),
                },
                TaskCommand::ListGroups,
            ],
        );
    }

    #[test]
    fn catalog_entries_cover_every_command_with_derived_fields() {
        let tool = TaskTool::new();
        let entries = as_tool(&tool).catalog_entries();
        // 1 tool entry + 11 command entries.
        assert_eq!(entries.len(), 12);
        assert_eq!(entries[0].name, "task");
        assert!(entries[0].parent_tool.is_none());

        let claim = entries
            .iter()
            .find(|e| e.command_value.as_deref() == Some("claim"))
            .expect("claim entry");
        assert_eq!(claim.parent_tool.as_deref(), Some("task"));
        assert_eq!(
            claim.description,
            "Atomically assign an agent to an unclaimed task."
        );
        let agent_path = claim
            .fields
            .iter()
            .find(|f| f.name == "agent_path")
            .expect("agent_path hint");
        assert!(agent_path.required);
        assert_eq!(agent_path.type_hint, "string");
        assert_eq!(
            agent_path.description,
            "Registry path of the claiming agent."
        );

        let list = entries
            .iter()
            .find(|e| e.command_value.as_deref() == Some("list"))
            .expect("list entry");
        let status = &list.fields[0];
        assert_eq!(status.name, "status");
        assert!(!status.required);
        assert_eq!(
            status.enum_values,
            vec!["pending", "in_progress", "completed", "blocked", "failed"]
        );
    }

    #[tokio::test]
    async fn unknown_action_is_typed_invalid_arguments_soft_failure() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();
        let out = execute(&tool, json!({"action": "explode"}), &ctx).await;
        assert!(out.is_error());
        let payload = out.error().expect("typed payload");
        assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
        let valid = payload.detail["valid_commands"]
            .as_array()
            .expect("valid commands listed");
        assert!(valid.iter().any(|v| v == "create"));
        assert!(valid.iter().any(|v| v == "list_groups"));
    }

    #[tokio::test]
    async fn missing_required_field_is_typed_invalid_arguments() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();
        // `create` without `description`.
        let out = execute(&tool, json!({"action": "create"}), &ctx).await;
        assert!(out.is_error());
        assert_eq!(out.error().unwrap().kind, ToolErrorKind::InvalidArguments);
    }

    // -- Behaviour ------------------------------------------------------

    #[tokio::test]
    async fn create_list_update_complete_full_cycle() {
        let tool = TaskTool::new();
        let (ctx, store) = ctx_with_store();

        for i in 0..3 {
            let out = execute(
                &tool,
                json!({ "action": "create", "description": format!("task {i}") }),
                &ctx,
            )
            .await;
            assert!(!out.is_error(), "create {i}: {:?}", out.content);
            assert_eq!(out.content["task"]["status"], "pending");
        }

        let out = execute(&tool, json!({"action": "list", "status": "pending"}), &ctx).await;
        let listed = out.content["tasks"].as_array().unwrap();
        assert_eq!(listed.len(), 3);

        let target_id = listed[0]["id"].as_str().unwrap().to_string();
        let out = execute(
            &tool,
            json!({ "action": "update", "task_id": target_id, "status": "in_progress" }),
            &ctx,
        )
        .await;
        assert_eq!(out.content["task"]["status"], "in_progress");

        let second_id = listed[1]["id"].as_str().unwrap().to_string();
        let out = execute(
            &tool,
            json!({"action": "complete", "task_id": second_id}),
            &ctx,
        )
        .await;
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
    async fn get_missing_task_is_typed_not_found() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();
        let out = execute(&tool, json!({"action": "get", "task_id": "ghost"}), &ctx).await;
        assert!(out.is_error());
        let payload = out.error().expect("typed payload");
        assert_eq!(payload.kind, ToolErrorKind::NotFound);
        assert_eq!(payload.detail["task_id"], "ghost");
        // Model-facing content keeps the tool-specific fields plus the error.
        assert_eq!(out.content["action"], "get");
        assert_eq!(out.content["error"]["kind"], "not_found");
    }

    #[tokio::test]
    async fn invalid_status_string_is_typed_invalid_arguments() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();
        let out = execute(&tool, json!({"action": "list", "status": "bogus"}), &ctx).await;
        assert!(out.is_error());
        assert_eq!(out.error().unwrap().kind, ToolErrorKind::InvalidArguments);
    }

    #[tokio::test]
    async fn list_filters_by_status() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();

        execute(&tool, json!({"action": "create", "description": "a"}), &ctx).await;
        let out = execute(&tool, json!({"action": "create", "description": "b"}), &ctx).await;
        let b_id = out.content["task"]["id"].as_str().unwrap().to_string();
        execute(&tool, json!({"action": "complete", "task_id": b_id}), &ctx).await;

        let out = execute(&tool, json!({"action": "list", "status": "pending"}), &ctx).await;
        assert_eq!(out.content["tasks"].as_array().unwrap().len(), 1);
        let out = execute(
            &tool,
            json!({"action": "list", "status": "completed"}),
            &ctx,
        )
        .await;
        assert_eq!(out.content["tasks"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn update_missing_task_errors() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();
        let err = as_tool(&tool)
            .execute(
                &envelope_for(json!({
                    "action": "update",
                    "task_id": "ghost",
                    "status": "in_progress"
                })),
                &ctx,
            )
            .await
            .expect_err("ghost");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn missing_store_returns_missing_extension() {
        let tool = TaskTool::new();
        let ctx = ToolContext::empty();
        let err = as_tool(&tool)
            .execute(&envelope_for(json!({"action": "list"})), &ctx)
            .await
            .expect_err("no store");
        match err {
            ToolError::MissingExtension { extension } => {
                assert!(extension.contains("SharedTaskStore"), "{extension}");
            }
            other => panic!("expected MissingExtension, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hierarchy_end_to_end() {
        let tool = TaskTool::new();
        let (ctx, _store) = ctx_with_store();

        // Create a named task group.
        let out = execute(
            &tool,
            json!({ "action": "create_group", "group_slug": "norn-agents-wiring" }),
            &ctx,
        )
        .await;
        assert_eq!(out.content["group_slug"], "norn-agents-wiring");

        let out = execute(&tool, json!({"action": "list_groups"}), &ctx).await;
        assert_eq!(out.content["groups"][0], "norn-agents-wiring");

        // Create a parent task.
        let out = execute(
            &tool,
            json!({ "action": "create", "task_id": "parent", "description": "parent work" }),
            &ctx,
        )
        .await;
        assert_eq!(out.content["task"]["id"], "parent");

        // Create three subtasks under the parent.
        for i in 0..3 {
            let out = execute(
                &tool,
                json!({
                    "action": "create_subtask",
                    "parent_task_id": "parent",
                    "task_id": format!("child-{i}"),
                    "description": format!("subtask {i}")
                }),
                &ctx,
            )
            .await;
            assert_eq!(out.content["task"]["parent_task_id"], "parent");
        }

        // Claim one subtask.
        let out = execute(
            &tool,
            json!({ "action": "claim", "task_id": "child-0", "agent_path": "root/worker" }),
            &ctx,
        )
        .await;
        assert_eq!(out.content["task"]["assigned_agent"], "root/worker");

        // A second claim on the same task fails.
        let err = as_tool(&tool)
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
        execute(
            &tool,
            json!({ "action": "update", "task_id": "child-1", "status": "in_progress" }),
            &ctx,
        )
        .await;

        // List children of the parent.
        let out = execute(
            &tool,
            json!({ "action": "children", "parent_task_id": "parent" }),
            &ctx,
        )
        .await;
        let children = out.content["tasks"].as_array().unwrap();
        assert_eq!(children.len(), 3);

        // Ancestors of a child walk back to the root parent.
        let out = execute(
            &tool,
            json!({ "action": "ancestors", "task_id": "child-2" }),
            &ctx,
        )
        .await;
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
        execute(
            &tool,
            json!({ "action": "create", "task_id": "parent", "description": "p" }),
            &ctx,
        )
        .await;
        execute(
            &tool,
            json!({
                "action": "create_subtask", "parent_task_id": "parent",
                "task_id": "mid", "description": "m"
            }),
            &ctx,
        )
        .await;
        execute(
            &tool,
            json!({
                "action": "create_subtask", "parent_task_id": "mid",
                "task_id": "leaf", "description": "l", "status": "failed"
            }),
            &ctx,
        )
        .await;

        let out = execute(
            &tool,
            json!({"action": "children", "parent_task_id": "parent"}),
            &ctx,
        )
        .await;
        let children = out.content["tasks"].as_array().unwrap();
        assert_eq!(children.len(), 1);
        // `mid` is stored as pending but rolls up to failed via its leaf.
        assert_eq!(children[0]["id"], "mid");
        assert_eq!(children[0]["status"], "failed");
    }
}
