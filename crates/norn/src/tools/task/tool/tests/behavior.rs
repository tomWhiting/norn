use super::*;

use crate::resource::DescriptorGovernor;
use crate::tools::task::DiskTaskStore;

#[tokio::test]
async fn create_list_update_complete_full_cycle() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, store) = ctx_with_store();

    for i in 0..3 {
        let out = execute(
            &tool,
            json!({ "action": "create", "description": format!("task {i}") }),
            &ctx,
        )
        .await?;
        assert!(!out.is_error(), "create {i}: {:?}", out.content);
        assert_eq!(out.content["task"]["status"], "pending");
    }

    let out = execute(&tool, json!({"action": "list", "status": "pending"}), &ctx).await?;
    let listed = json_array(&out.content["tasks"], "pending tasks")?;
    assert_eq!(listed.len(), 3);

    let target_id = json_string(&listed[0]["id"], "first task id")?.to_string();
    let out = execute(
        &tool,
        json!({ "action": "update", "task_id": target_id, "status": "in_progress" }),
        &ctx,
    )
    .await?;
    assert_eq!(out.content["task"]["status"], "in_progress");

    let second_id = json_string(&listed[1]["id"], "second task id")?.to_string();
    let out = execute(
        &tool,
        json!({"action": "complete", "task_id": second_id}),
        &ctx,
    )
    .await?;
    assert_eq!(out.content["task"]["status"], "completed");

    let final_state = store.list(None)?;
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
    Ok(())
}

#[tokio::test]
async fn get_missing_task_is_typed_not_found() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();
    let out = execute(&tool, json!({"action": "get", "task_id": "ghost"}), &ctx).await?;
    assert!(out.is_error());
    let payload = output_error(&out)?;
    assert_eq!(payload.kind, ToolErrorKind::NotFound);
    assert_eq!(payload.detail["task_id"], "ghost");
    // Model-facing content keeps the tool-specific fields plus the error.
    assert_eq!(out.content["action"], "get");
    assert_eq!(out.content["error"]["kind"], "not_found");
    Ok(())
}

#[tokio::test]
async fn disk_admission_failure_is_typed_resource_exhaustion()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let governor = Arc::new(DescriptorGovernor::with_capacity(4));
    let store = Arc::new(DiskTaskStore::with_governor(
        temporary.path().join("tasks"),
        "group".to_owned(),
        governor,
    ));
    let context = ToolContext::empty();
    context.insert_extension(Arc::new(SharedTaskStore(store)));

    let tool = TaskTool::new();
    let result = as_tool(&tool)
        .execute(
            &envelope_for(json!({"action": "get", "task_id": "missing"})),
            &context,
        )
        .await;
    let Err(error @ ToolError::DescriptorAdmission(_)) = result else {
        return Err(std::io::Error::other(
            "task admission failure did not remain a typed ToolError",
        )
        .into());
    };
    let payload = ToolErrorPayload::from(&error);
    assert_eq!(payload.kind, ToolErrorKind::ResourceExhausted);
    assert!(payload.detail.get("descriptor_admission").is_some());
    Ok(())
}

#[tokio::test]
async fn invalid_status_string_is_typed_invalid_arguments() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();
    let out = execute(&tool, json!({"action": "list", "status": "bogus"}), &ctx).await?;
    assert!(out.is_error());
    assert_eq!(output_error(&out)?.kind, ToolErrorKind::InvalidArguments);
    Ok(())
}

#[tokio::test]
async fn list_filters_by_status() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();

    execute(&tool, json!({"action": "create", "description": "a"}), &ctx).await?;
    let out = execute(&tool, json!({"action": "create", "description": "b"}), &ctx).await?;
    let b_id = json_string(&out.content["task"]["id"], "created task id")?.to_string();
    execute(&tool, json!({"action": "complete", "task_id": b_id}), &ctx).await?;

    let out = execute(&tool, json!({"action": "list", "status": "pending"}), &ctx).await?;
    assert_eq!(json_array(&out.content["tasks"], "pending tasks")?.len(), 1);
    let out = execute(
        &tool,
        json!({"action": "list", "status": "completed"}),
        &ctx,
    )
    .await?;
    assert_eq!(
        json_array(&out.content["tasks"], "completed tasks")?.len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn update_missing_task_errors() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();
    let result = as_tool(&tool)
        .execute(
            &envelope_for(json!({
                "action": "update",
                "task_id": "ghost",
                "status": "in_progress"
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err(std::io::Error::other("missing task update succeeded").into());
    };
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    Ok(())
}

#[tokio::test]
async fn missing_store_returns_missing_extension() -> TestResult {
    let tool = TaskTool::new();
    let ctx = ToolContext::empty();
    let result = as_tool(&tool)
        .execute(&envelope_for(json!({"action": "list"})), &ctx)
        .await;
    match result {
        Err(ToolError::MissingExtension { extension }) => {
            assert!(extension.contains("SharedTaskStore"), "{extension}");
        }
        Err(other) => {
            return Err(
                std::io::Error::other(format!("expected MissingExtension, got {other}")).into(),
            );
        }
        Ok(_) => {
            return Err(std::io::Error::other("task tool ran without a task store").into());
        }
    }
    Ok(())
}

#[tokio::test]
async fn hierarchy_end_to_end() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();

    // Create a named task group.
    let out = execute(
        &tool,
        json!({ "action": "create_group", "group_slug": "norn-agents-wiring" }),
        &ctx,
    )
    .await?;
    assert_eq!(out.content["group_slug"], "norn-agents-wiring");

    let out = execute(&tool, json!({"action": "list_groups"}), &ctx).await?;
    assert_eq!(out.content["groups"][0], "norn-agents-wiring");

    // Create a parent task.
    let out = execute(
        &tool,
        json!({ "action": "create", "task_id": "parent", "description": "parent work" }),
        &ctx,
    )
    .await?;
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
        .await?;
        assert_eq!(out.content["task"]["parent_task_id"], "parent");
    }

    // Claim one subtask.
    let out = execute(
        &tool,
        json!({ "action": "claim", "task_id": "child-0", "agent_path": "root/worker" }),
        &ctx,
    )
    .await?;
    assert_eq!(out.content["task"]["assigned_agent"], "root/worker");

    // A second claim on the same task fails.
    let result = as_tool(&tool)
        .execute(
            &envelope_for(json!({
                "action": "claim",
                "task_id": "child-0",
                "agent_path": "root/other"
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err(std::io::Error::other("second claim succeeded").into());
    };
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));

    // Move one child to in_progress so the parent rolls up to in_progress.
    execute(
        &tool,
        json!({ "action": "update", "task_id": "child-1", "status": "in_progress" }),
        &ctx,
    )
    .await?;

    // List children of the parent.
    let out = execute(
        &tool,
        json!({ "action": "children", "parent_task_id": "parent" }),
        &ctx,
    )
    .await?;
    let children = json_array(&out.content["tasks"], "child tasks")?;
    assert_eq!(children.len(), 3);

    // Ancestors of a child walk back to the root parent.
    let out = execute(
        &tool,
        json!({ "action": "ancestors", "task_id": "child-2" }),
        &ctx,
    )
    .await?;
    let chain = json_array(&out.content["tasks"], "ancestor chain")?;
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0]["id"], "child-2");
    assert_eq!(chain[1]["id"], "parent");
    Ok(())
}

#[tokio::test]
async fn children_action_applies_rollup_status() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();

    // parent -> mid -> leaf, with leaf failed so mid rolls up to failed.
    execute(
        &tool,
        json!({ "action": "create", "task_id": "parent", "description": "p" }),
        &ctx,
    )
    .await?;
    execute(
        &tool,
        json!({
            "action": "create_subtask", "parent_task_id": "parent",
            "task_id": "mid", "description": "m"
        }),
        &ctx,
    )
    .await?;
    execute(
        &tool,
        json!({
            "action": "create_subtask", "parent_task_id": "mid",
            "task_id": "leaf", "description": "l", "status": "failed"
        }),
        &ctx,
    )
    .await?;

    let out = execute(
        &tool,
        json!({"action": "children", "parent_task_id": "parent"}),
        &ctx,
    )
    .await?;
    let children = json_array(&out.content["tasks"], "rolled-up child tasks")?;
    assert_eq!(children.len(), 1);
    // `mid` is stored as pending but rolls up to failed via its leaf.
    assert_eq!(children[0]["id"], "mid");
    assert_eq!(children[0]["status"], "failed");
    Ok(())
}
