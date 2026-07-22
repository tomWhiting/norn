use super::*;

#[test]
fn build_with_runtime_base_publishes_shared_task_store_on_active_context() {
    use crate::tools::task::SharedTaskStore;

    let temp = tempfile::tempdir().expect("tempdir");
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .load_runtime_base()
        .build()
        .expect("build succeeds");

    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    ctx.get_extension::<SharedTaskStore>()
        .expect("runtime base task store is installed on the active tool context");
}

#[test]
fn build_publishes_action_log_on_both_contexts_with_same_arc() {
    use crate::session::action_log::ActionLog;

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("build succeeds");

    let loop_log = agent
        .loop_context
        .action_log
        .clone()
        .expect("loop context action log is populated after build");

    let ctx_log = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context")
        .get_extension::<ActionLog>()
        .expect("tool context publishes the ActionLog extension");

    assert!(
        Arc::ptr_eq(&loop_log, &ctx_log),
        "loop context and tool context must share the same ActionLog instance",
    );
}

#[tokio::test]
async fn built_action_log_tool_runs_list_query() {
    use crate::session::action_log::ActionLog;
    use crate::tool::envelope::ToolEnvelope;

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("build succeeds");

    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    let log = ctx
        .get_extension::<ActionLog>()
        .expect("tool context publishes the ActionLog extension");

    // Seed one completion so the list query has something to return.
    log.record_completion(crate::session::action_log::CompletionRecord {
        tool_name: "read",
        tool_call_id: "tc-built",
        tool_use_description: "",
        outcome: crate::session::action_log::Outcome::Success,
        output: &serde_json::json!({ "path": "src/a.rs", "lines": 3 }),
        args: serde_json::json!({ "path": "src/a.rs" }),
        duration_ms: 1,
        follow_ups: Vec::new(),
        post_validate_outcome: None,
        level_1_only: false,
    });

    let tool = agent.registry.get("action_log").expect("action_log tool");
    let envelope = ToolEnvelope {
        tool_call_id: "self-call".to_string(),
        tool_name: "action_log".to_string(),
        model_args: serde_json::json!({ "query": "list" }),
        metadata: Value::Null,
    };
    let out = tool
        .execute(&envelope, ctx.as_ref())
        .await
        .expect("action_log list query runs through the built context");
    assert!(!out.is_error());
    assert_eq!(out.content["query"], "list");
    assert_eq!(out.content["count"], 1);
    assert_eq!(out.content["entries"][0]["id"], "tc-built");
}

/// R5 (root): after `build`, the root's shared tool context carries
/// its OWN base system instruction under `ParentSystemInstruction`
/// (the previously never-published extension the fork tool consumes)
/// and its launch model under `AgentModel` (the parent-model ground
/// truth for spawns that omit `model`).
#[test]
fn build_publishes_own_base_instruction_and_model_on_shared_context() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .system_prompt("ROOT-BASE-MARKER")
        .build()
        .expect("build succeeds");

    let shared = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    let base = shared
        .get_extension::<crate::agent::fork::ParentSystemInstruction>()
        .expect("the root context must publish ParentSystemInstruction");
    assert_eq!(
        base.as_str(),
        agent.loop_context.base_system_instruction(),
        "the published value is exactly the root's installed base",
    );
    assert!(
        base.as_str().contains("ROOT-BASE-MARKER"),
        "the override flows into the published base: {}",
        base.as_str(),
    );
    let live = shared
        .get_extension::<crate::tools::agent::AgentModel>()
        .expect("the root context must publish AgentModel");
    assert_eq!(
        live.model, "test-model",
        "stamped from the actual launch model"
    );
    assert_eq!(
        live.reasoning_effort, agent.loop_context.reasoning_effort,
        "the paired effort is stamped from the same assembled loop context",
    );
}

#[test]
fn extension_is_published_on_tool_context() {
    #[derive(Debug, PartialEq, Eq)]
    struct Marker(u32);

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .extension(Arc::new(Marker(7)))
        .build()
        .expect("build succeeds");

    let marker = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context")
        .get_extension::<Marker>()
        .expect("custom extension is retrievable through the builder hook");
    assert_eq!(*marker, Marker(7));
}

#[test]
fn without_tools_excludes_named_tools() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .without_tools(&["bash", "write"])
        .build()
        .expect("build succeeds");
    assert!(agent.registry.get("bash").is_none(), "bash excluded");
    assert!(agent.registry.get("write").is_none(), "write excluded");
    assert!(agent.registry.get("read").is_some(), "read remains");
}

#[test]
fn build_succeeds_when_all_tools_excluded() {
    // Excluding the entire standard set is a supported zero-tool
    // configuration (pure text transform) — build must succeed with an
    // empty gated registry rather than reject it. Owner decision
    // 2026-07-02 (docs/DECISIONS-2026-07.md) removing the former
    // ≥1-tool rejection.
    let names = [
        "read",
        "write",
        "edit",
        "bash",
        "apply_patch",
        "search",
        "lsp",
        "task",
        "tool_search",
        "action_log",
        "web_fetch",
        "web_search",
        "spawn_agent",
        "fork",
        "signal_agent",
        "wake_agent",
        "close_agent",
        "agents",
        "cron",
        "process",
    ];
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .without_tools(&names)
        .build()
        .expect("a fully-excluded tool set must still build");
    assert_eq!(
        agent.registry.names().count(),
        0,
        "every standard tool must be gated out",
    );
}
