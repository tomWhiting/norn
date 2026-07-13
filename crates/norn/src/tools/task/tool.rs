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
fn child_with_rollup(store: &dyn TaskStore, child: &TaskEntry) -> Result<Value, ToolError> {
    let grandchild_statuses: Vec<_> = store
        .children(&child.id)?
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
    Ok(json)
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
            TaskCommand::Get { task_id } => match store.get(&task_id)? {
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
                let items: Vec<Value> = store.list(status)?.iter().map(entry_to_json).collect();
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
                let children = store.children(&parent_task_id)?;
                let items: Vec<Value> = children
                    .iter()
                    .map(|child| child_with_rollup(store.as_ref(), child))
                    .collect::<Result<_, _>>()?;
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
                serde_json::json!({ "action": "list_groups", "groups": store.list_groups()? })
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
#[path = "tool/tests/mod.rs"]
mod tests;
