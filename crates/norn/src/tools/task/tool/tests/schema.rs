use super::*;

#[test]
fn schema_is_derived_one_of_with_per_command_required_fields() -> TestResult {
    let tool = TaskTool::new();
    let schema = as_tool(&tool).input_schema();
    // OpenAI rejects function schemas whose root is not `type: "object"`
    // (regression: HTTP 400 invalid_function_parameters on every request
    // carrying this tool).
    assert_eq!(schema["type"], "object");
    let variants = json_array(&schema["oneOf"], "oneOf")?;
    assert_eq!(variants.len(), 11);

    let create = variants
        .iter()
        .find(|v| v["properties"]["action"]["const"] == "create")
        .ok_or_else(|| std::io::Error::other("create variant was absent"))?;
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
        .ok_or_else(|| std::io::Error::other("claim variant was absent"))?;
    assert_eq!(
        claim["required"],
        json!(["action", "task_id", "agent_path"])
    );
    Ok(())
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
fn catalog_entries_cover_every_command_with_derived_fields() -> TestResult {
    let tool = TaskTool::new();
    let entries = as_tool(&tool).catalog_entries();
    // 1 tool entry + 11 command entries.
    assert_eq!(entries.len(), 12);
    assert_eq!(entries[0].name, "task");
    assert!(entries[0].parent_tool.is_none());

    let claim = entries
        .iter()
        .find(|e| e.command_value.as_deref() == Some("claim"))
        .ok_or_else(|| std::io::Error::other("claim catalog entry was absent"))?;
    assert_eq!(claim.parent_tool.as_deref(), Some("task"));
    assert_eq!(
        claim.description,
        "Atomically assign an agent to an unclaimed task."
    );
    let agent_path = claim
        .fields
        .iter()
        .find(|f| f.name == "agent_path")
        .ok_or_else(|| std::io::Error::other("agent_path field hint was absent"))?;
    assert!(agent_path.required);
    assert_eq!(agent_path.type_hint, "string");
    assert_eq!(
        agent_path.description,
        "Registry path of the claiming agent."
    );

    let list = entries
        .iter()
        .find(|e| e.command_value.as_deref() == Some("list"))
        .ok_or_else(|| std::io::Error::other("list catalog entry was absent"))?;
    let status = &list.fields[0];
    assert_eq!(status.name, "status");
    assert!(!status.required);
    assert_eq!(
        status.enum_values,
        vec!["pending", "in_progress", "completed", "blocked", "failed"]
    );
    Ok(())
}

#[tokio::test]
async fn unknown_action_is_typed_invalid_arguments_soft_failure() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();
    let out = execute(&tool, json!({"action": "explode"}), &ctx).await?;
    assert!(out.is_error());
    let payload = output_error(&out)?;
    assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
    let valid = json_array(&payload.detail["valid_commands"], "valid_commands")?;
    assert!(valid.iter().any(|v| v == "create"));
    assert!(valid.iter().any(|v| v == "list_groups"));
    Ok(())
}

#[tokio::test]
async fn missing_required_field_is_typed_invalid_arguments() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();
    // `create` without `description`.
    let out = execute(&tool, json!({"action": "create"}), &ctx).await?;
    assert!(out.is_error());
    assert_eq!(output_error(&out)?.kind, ToolErrorKind::InvalidArguments);
    Ok(())
}

/// Regression for the exact production case: a model called
/// `complete` with a `metadata` field; `Complete { task_id }` takes
/// only `task_id`, so serde silently dropped the metadata and the
/// task completed as if nothing was lost. Dispatch must now reject
/// the call naming `metadata`, leave the task untouched, and tell
/// the model what `complete` actually accepts.
#[tokio::test]
async fn complete_with_metadata_is_rejected_and_task_untouched() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, store) = ctx_with_store();
    execute(
        &tool,
        json!({"action": "create", "task_id": "t-1", "description": "work"}),
        &ctx,
    )
    .await?;

    let out = execute(
        &tool,
        json!({
            "action": "complete",
            "task_id": "t-1",
            "metadata": {"result": "shipped"}
        }),
        &ctx,
    )
    .await?;
    assert!(out.is_error());
    let payload = output_error(&out)?;
    assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
    assert!(
        payload.message.contains("'metadata'"),
        "message names the dropped field: {}",
        payload.message
    );
    assert!(
        payload.message.contains("'complete'"),
        "message names the resolved command: {}",
        payload.message
    );
    assert_eq!(payload.detail["command"], "complete");
    assert_eq!(payload.detail["unknown_fields"], json!(["metadata"]));
    let accepted = json_array(&payload.detail["accepted_fields"], "accepted_fields")?;
    assert_eq!(accepted.len(), 1, "complete takes only task_id");
    assert_eq!(accepted[0]["name"], "task_id");
    assert_eq!(accepted[0]["required"], true);

    // Nothing executed: the task is still pending and its metadata
    // was not silently discarded into a completed state.
    let entry = store
        .get("t-1")?
        .ok_or_else(|| std::io::Error::other("task disappeared after rejected command"))?;
    assert_eq!(entry.status, TaskStatus::Pending);
    assert_eq!(entry.metadata, Value::Null);
    Ok(())
}

/// A cross-command field on a read command is rejected the same way
/// (the loosened OpenAI flat projection invites these).
#[tokio::test]
async fn cross_command_field_on_get_is_rejected() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();
    execute(
        &tool,
        json!({"action": "create", "task_id": "t-1", "description": "work"}),
        &ctx,
    )
    .await?;
    let out = execute(
        &tool,
        json!({"action": "get", "task_id": "t-1", "description": "stray"}),
        &ctx,
    )
    .await?;
    assert!(out.is_error());
    let payload = output_error(&out)?;
    assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
    assert_eq!(payload.detail["unknown_fields"], json!(["description"]));
    Ok(())
}

/// Explicit nulls for optional fields deserialize as absent and must
/// keep passing — models routinely send them.
#[tokio::test]
async fn explicit_null_optional_fields_still_pass() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();
    let out = execute(
        &tool,
        json!({
            "action": "create",
            "description": "work",
            "task_id": null,
            "status": null,
            "depends_on": null,
            "metadata": null
        }),
        &ctx,
    )
    .await?;
    assert!(!out.is_error(), "{:?}", out.content);
    let out = execute(&tool, json!({"action": "list", "status": null}), &ctx).await?;
    assert!(!out.is_error(), "{:?}", out.content);
    Ok(())
}

/// No-regression sweep: an exact-field call for every `TaskCommand`
/// variant passes canonical-schema enforcement and executes.
#[tokio::test]
async fn every_command_with_exact_fields_passes_validation() -> TestResult {
    let tool = TaskTool::new();
    let (ctx, _store) = ctx_with_store();
    let calls = [
        json!({
            "action": "create", "task_id": "t-1", "description": "work",
            "status": "pending", "depends_on": [], "metadata": {"k": "v"}
        }),
        json!({"action": "get", "task_id": "t-1"}),
        json!({"action": "list", "status": "pending"}),
        json!({
            "action": "update", "task_id": "t-1", "status": "in_progress",
            "description": "more work", "depends_on": [], "metadata": {"k": "v2"}
        }),
        json!({
            "action": "create_subtask", "parent_task_id": "t-1",
            "task_id": "t-2", "description": "child",
            "status": "pending", "depends_on": [], "metadata": {}
        }),
        json!({"action": "children", "parent_task_id": "t-1"}),
        json!({"action": "ancestors", "task_id": "t-2"}),
        json!({"action": "claim", "task_id": "t-2", "agent_path": "root/worker"}),
        json!({"action": "complete", "task_id": "t-2"}),
        json!({"action": "create_group", "group_slug": "g-1"}),
        json!({"action": "list_groups"}),
    ];
    for call in calls {
        let out = execute(&tool, call.clone(), &ctx).await?;
        assert!(
            !out.is_error(),
            "call must pass: {call} → {:?}",
            out.content
        );
    }
    Ok(())
}

// -- Behaviour ------------------------------------------------------
