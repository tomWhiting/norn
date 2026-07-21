use super::*;

/// Hardening (owner ruling 2026-07-03): a spawned child must run with
/// auto-compaction armed exactly like the root — otherwise a
/// long-running delegated child dies `ContextWindowExceeded`. The
/// spawn launch path calls the shared `arm_auto_compaction`, which
/// installs the token estimator and fills the child's context window
/// from the catalog for the child's own model. This drives a spawned
/// child whose first turn reports an oversized usage (setting the
/// context-edit usage floor above the window) and asserts the child's
/// next preflight emitted a `loop.token_warning` on the child's store.
/// That event is structurally impossible without the estimator and the
/// window the shared arming installs (the preflight returns early with
/// no estimator, and the warning is gated on a `Some` window), so its
/// presence proves the spawn site armed the child.
#[tokio::test]
async fn spawn_child_arms_auto_compaction_preflight() -> TestResult {
    let catalog_model = crate::model_catalog::default_selection().model;
    // Turn 1: a tool call (forces a second provider round-trip so a
    // second preflight runs) whose reported usage dwarfs any context
    // window — this becomes the usage floor. Turn 2: end the turn.
    let oversized_done = ProviderEvent::Done {
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 100_000_000,
            output_tokens: 0,
            ..Usage::default()
        },
        response_id: None,
    };
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-noop".to_string(),
                call_id: None,
                name: Some("noop".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            oversized_done,
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ],
    ]));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoStubTool { tool_name: "noop" }));
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
        json!({"task": "run", "model": catalog_model, "role": "worker"}),
    )
    .await;

    let child_store = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .event_store(child_id)
        .ok_or("required test value")?;
    let warned = child_store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. } if event_type == "loop.token_warning"
        )
    });
    assert!(
        warned,
        "the spawned child's preflight must emit loop.token_warning, \
             proving the estimator and catalog window were armed",
    );
    Ok(())
}

/// Installs a one-skill catalog + search path on `ctx` and returns the
/// temp dir (kept alive for the skill files) — the shared setup for the
/// spawned-child skill tests.
fn install_greet_skill(ctx: &ToolContext) -> TestResult<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    let skill_dir = dir.path().join("greet");
    std::fs::create_dir(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: greet the user\n---\nHELLO_FROM_GREET",
    )?;
    let paths = vec![dir.path().to_path_buf()];
    let catalog = Arc::new(crate::skill::SkillCatalog::scan(&paths));
    ctx.insert_extension(Arc::new(crate::tools::skill::SkillSearchPaths(paths)));
    ctx.insert_extension(catalog);
    Ok(dir)
}

fn skill_registry() -> Arc<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(crate::tools::skill::SkillTool::with_config(
        crate::tools::skill::SkillToolConfig {
            shell_execution: false,
        },
    )));
    Arc::new(registry)
}

/// Defect 1 regression (critical): a spawned child must be able to load
/// a skill end-to-end. Previously `build_child_context` never forwarded
/// `SkillSearchPaths`/`SkillCatalog`, so the child saw the `skill` tool
/// but every call failed `MissingExtension`. Here the child calls
/// `skill` and its store must carry a successful `skill` tool result
/// containing the skill body — impossible unless the extensions were
/// forwarded.
#[tokio::test]
async fn spawned_child_loads_a_skill_end_to_end() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc1".to_string(),
                call_id: None,
                name: Some("skill".to_string()),
                arguments_delta: json!({"name": "greet"}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ],
    ]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        skill_registry(),
        Arc::new(MessageRouter::new()),
    );
    let _skill_dir = install_greet_skill(&ctx)?;

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "greet the user", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let child_store = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .event_store(child_id)
        .ok_or("required test value")?;
    let loaded = child_store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::ToolResult { tool_name, output, .. }
                if tool_name == "skill" && output.to_string().contains("HELLO_FROM_GREET")
        )
    });
    assert!(
        loaded,
        "spawned child must load the skill successfully (extensions forwarded): {:?}",
        child_store.events(),
    );
    Ok(())
}

/// Defect 1 regression: the spawned child's system prompt carries the
/// "# Available Skills" section when the skill tool is on its surface.
#[tokio::test]
async fn spawned_child_system_prompt_lists_available_skills() -> TestResult {
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
        captured: Arc::clone(&captured),
        responses: StdMutex::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ]]),
    });
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        skill_registry(),
        Arc::new(MessageRouter::new()),
    );
    let _skill_dir = install_greet_skill(&ctx)?;

    let tool = SpawnAgentTool::new();
    spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "greet the user", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let requests = captured.lock().clone();
    let advertises = requests.iter().flat_map(|r| r.messages.iter()).any(|m| {
        m.content.as_deref().is_some_and(|content| {
            content.contains("# Available Skills") && content.contains("greet")
        })
    });
    assert!(
        advertises,
        "the child's system prompt must advertise the skill listing: {requests:?}",
    );
    Ok(())
}

/// Defect 1 regression: the "# Available Skills" section is absent when
/// the child's allow-list excludes the skill tool (never advertise a
/// skill the child has no tool to load).
#[tokio::test]
async fn spawned_child_system_prompt_omits_skills_when_tool_excluded() -> TestResult {
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
        captured: Arc::clone(&captured),
        responses: StdMutex::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ]]),
    });
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(crate::tools::skill::SkillTool::with_config(
        crate::tools::skill::SkillToolConfig {
            shell_execution: false,
        },
    )));
    registry.register(Box::new(EchoStubTool { tool_name: "read" }));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(registry),
        Arc::new(MessageRouter::new()),
    );
    let _skill_dir = install_greet_skill(&ctx)?;

    let tool = SpawnAgentTool::new();
    spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "read only", "model": CATALOG_MODEL, "role": "worker", "tools": ["read"]}),
    )
    .await;

    let requests = captured.lock().clone();
    let advertises = requests.iter().flat_map(|r| r.messages.iter()).any(|m| {
        m.content
            .as_deref()
            .is_some_and(|content| content.contains("# Available Skills"))
    });
    assert!(
        !advertises,
        "a child without the skill tool must not advertise skills: {requests:?}",
    );
    Ok(())
}
