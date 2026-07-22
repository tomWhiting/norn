use super::*;

/// Schema is an explicit per-spawn decision: the `output_schema`
/// argument flows into the child's loop, which enforces it — the
/// structured result reaches the parent through the result channel.
/// (Without the argument the child runs free-form; children never
/// inherit the parent's schema implicitly.)
#[tokio::test]
async fn spawn_output_schema_enforces_structured_output() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"answer": 42}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        // Fallback done-turn in case the runner loops after structured
        // output.
        vec![done_event()],
    ]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({
            "task": "answer the question",
            "model": CATALOG_MODEL,
            "role": "worker",
            "output_schema": {
                "type": "object",
                "required": ["answer"],
                "additionalProperties": false,
                "properties": { "answer": { "type": "integer" } }
            }
        }),
    )
    .await;

    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, child_id);
    assert!(result.succeeded, "schema-valid output completes the child");
    assert!(
        result.formatted_message.contains("42"),
        "the structured output must reach the parent: {}",
        result.formatted_message,
    );
    Ok(())
}

/// R2: a disallowed tool name surfaces as `ToolNotFound` from the
/// child's executor; the loop falls back to its next turn's text and the
/// spawn still completes.
#[tokio::test]
async fn spawn_agent_tool_subset_gates_disallowed_tools() -> TestResult {
    let turn1 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc1".to_string(),
            call_id: None,
            name: Some("bash".to_string()),
            arguments_delta: "{}".to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let turn2 = vec![
        ProviderEvent::TextDelta {
            text: "fell back to text".to_string(),
        },
        done_event(),
    ];
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoStubTool {
        tool_name: "search",
    }));
    registry.register(Box::new(EchoStubTool { tool_name: "bash" }));
    let registry = Arc::new(registry);

    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        registry,
        Arc::new(MessageRouter::new()),
    );

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "try bash", "model": CATALOG_MODEL, "role": "worker", "tools": ["search"]}),
    )
    .await;
    // The child completed its step — the disallowed tool call did not fail
    // the run. The entry stays observable and wakeable with Idle status.
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
    );
    Ok(())
}

/// R4: a named profile resolved from a temp `.md` file supplies the
/// child's source-typed stable prompt fragment.
#[test]
fn build_child_loop_context_uses_profile_body() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let profile_path = dir
        .path()
        .join(".norn")
        .join("profiles")
        .join("researcher.md");
    let Some(profile_parent) = profile_path.parent() else {
        return Err(std::io::Error::other("profile path must have a parent").into());
    };
    std::fs::create_dir_all(profile_parent)?;
    std::fs::write(
        &profile_path,
        "---\nname: researcher\nmodel: gpt-5\ntools: read, grep\n---\nYou are a focused researcher.\n",
    )?;

    let launch_root = dir.path().canonicalize()?;
    let (loop_ctx, tools) = build_child_loop_context(None, Some("researcher"), &launch_root)?;
    assert!(
        loop_ctx.system_sections[0].contains("You are a focused researcher."),
        "profile body must remain visible in the compatibility base view",
    );
    let plan = loop_ctx
        .stable_prompt_plan()
        .ok_or("profile child must carry a typed prompt plan")?;
    let profile_fragment = plan
        .fragments()
        .iter()
        .find(|fragment| fragment.source() == crate::system_prompt::PromptSource::WorkspaceProfile)
        .ok_or("workspace profile fragment must retain its source")?;
    assert_eq!(
        profile_fragment.authority(),
        crate::system_prompt::PromptAuthority::User,
    );
    assert_eq!(
        tools,
        Some(vec!["read".to_owned(), "grep".to_owned()]),
        "profile's resolved tool list flows back as the allow-list",
    );
    Ok(())
}

#[test]
fn child_profile_resolution_stays_pinned_to_the_launch_root()
-> Result<(), Box<dyn std::error::Error>> {
    let launch = tempfile::tempdir()?;
    let moved = tempfile::tempdir()?;
    for (root, body) in [
        (launch.path(), "original launch-root profile"),
        (moved.path(), "mutable execution-cwd profile"),
    ] {
        let profile_path = root.join(".norn/profiles/shared.md");
        let Some(profile_parent) = profile_path.parent() else {
            return Err(std::io::Error::other("profile path must have a parent").into());
        };
        std::fs::create_dir_all(profile_parent)?;
        std::fs::write(
            profile_path,
            format!("---\nname: shared\nmodel: gpt-5\n---\n{body}\n"),
        )?;
    }

    let launch_root = launch.path().canonicalize()?;
    let moved_root = moved.path().canonicalize()?;
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(crate::runtime_init::extensions::LaunchWorkingDir(
        launch_root.clone(),
    )));
    ctx.set_working_dir(moved_root);

    let profile_root = resolve_profile_root(&ctx, true)?;
    let (loop_ctx, _) = build_child_loop_context(None, Some("shared"), &profile_root)?;
    let prompt = loop_ctx.base_system_instruction();

    assert_eq!(profile_root, launch_root);
    assert!(prompt.contains("original launch-root profile"));
    assert!(!prompt.contains("mutable execution-cwd profile"));
    Ok(())
}

#[test]
fn build_child_loop_context_rejects_workspace_profile_prompt_commands()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let profile_path = dir
        .path()
        .join(".norn")
        .join("profiles")
        .join("hostile.json");
    let Some(profile_parent) = profile_path.parent() else {
        return Err(std::io::Error::other("profile path must have a parent").into());
    };
    std::fs::create_dir_all(profile_parent)?;
    std::fs::write(
        profile_path,
        r#"{
                "name": "hostile",
                "model": "gpt-5.6-sol",
                "prompt_commands": [{
                    "name": "private",
                    "command": "touch child-profile-command-secret",
                    "cache_ttl": null
                }]
            }"#,
    )?;

    let launch_root = dir.path().canonicalize()?;
    let result = build_child_loop_context(None, Some("hostile"), &launch_root);
    assert!(
        matches!(&result, Err(ToolError::ExecutionFailed { .. })),
        "workspace prompt command must be rejected as ExecutionFailed",
    );
    if let Err(ToolError::ExecutionFailed { reason }) = result {
        assert!(reason.contains("prompt_commands"));
        assert!(!reason.contains("child-profile-command-secret"));
    }
    assert!(!dir.path().join("child-profile-command-secret").exists());
    Ok(())
}

/// With no profile or variant, the stable plan contains compiled child policy
/// only. The task belongs exclusively to the run's User prompt.
#[test]
fn build_child_loop_context_default_has_no_task() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let (loop_ctx, tools) = build_child_loop_context(None, None, dir.path())?;
    let plan = loop_ctx
        .stable_prompt_plan()
        .ok_or("plain child must carry a typed prompt plan")?;
    assert_eq!(plan.fragments().len(), 1);
    assert_eq!(
        plan.fragments()[0].source(),
        crate::system_prompt::PromptSource::ChildAgentPolicy,
    );
    assert!(
        tools.is_none(),
        "no profile means no allow-list from a profile"
    );
    Ok(())
}

/// R4: an unresolvable profile name surfaces as `ExecutionFailed` — no
/// silent fallback to a default profile.
#[test]
fn build_child_loop_context_unknown_profile_errors() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    // `LoopContext` is not `Debug`, so the `Ok` arm cannot use
    // `expect_err`; match the result explicitly instead.
    let result = build_child_loop_context(None, Some("missing"), dir.path());
    assert!(
        matches!(&result, Err(ToolError::ExecutionFailed { .. })),
        "unknown profile must return ExecutionFailed",
    );
    if let Err(ToolError::ExecutionFailed { reason }) = result {
        assert!(reason.contains("missing"), "{reason}");
    }
    Ok(())
}

#[test]
fn child_profile_errors_do_not_echo_control_characters() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    for name in [
        "sentinel\nnewline",
        "sentinel\u{1b}[31m",
        "sentinel\u{7}bell",
    ] {
        let result = build_child_loop_context(None, Some(name), dir.path());
        assert!(result.is_err(), "unsafe profile name must be rejected");
        if let Err(error) = result {
            let display = error.to_string();
            let debug = format!("{error:?}");
            assert!(!display.contains("sentinel"));
            assert!(!debug.contains("sentinel"));
            assert!(!display.contains(['\n', '\u{1b}', '\u{7}']));
        }
    }
    Ok(())
}

/// R5: with no allow-list, every available parent tool is offered.
#[test]
fn build_tool_definitions_includes_all_when_no_allow_list() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoStubTool { tool_name: "read" }));
    registry.register(Box::new(EchoStubTool { tool_name: "edit" }));
    let defs = build_tool_definitions(&registry, None);
    let mut names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["edit", "read"]);
}

/// Stub tool counting how many times it actually executed, so a test
/// can prove a denied tool never ran inside a child.
pub(super) struct CountingStubTool {
    pub(super) tool_name: &'static str,
    pub(super) executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl TestTool for CountingStubTool {
    fn name(&self) -> &'static str {
        self.tool_name
    }
    fn description(&self) -> &'static str {
        "counts executions"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }
    async fn execute(
        &self,
        _envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> Result<TestToolOutput, ToolError> {
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
    }
}
