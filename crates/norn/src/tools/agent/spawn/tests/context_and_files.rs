use super::*;

/// Confinement-escape regression (blocker): `workspace_root` is a
/// plain field on [`ToolContext`] — not an extension — so
/// `build_child_context` must forward it explicitly, and the child's
/// working dir must be seeded from the parent's *current* working dir
/// on the child's own handle (snapshot semantics), never from the
/// process CWD.
#[test]
fn child_context_forwards_workspace_root_and_snapshots_working_dir() {
    use crate::tool::context::SharedWorkingDir;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let infra = AgentToolInfra {
        registry: AgentRegistry::shared(),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: Uuid::new_v4(),
        parent_id: None,
        grant: None,
        tool_registry: Some(Arc::new(ToolRegistry::new())),
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    };
    let mut parent_ctx =
        ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from("/tmp/parent-wd")));
    parent_ctx.confine_to_workspace(PathBuf::from("/tmp/workspace-root"));

    let child_ctx = build_child_context(
        &infra,
        Uuid::new_v4(),
        Arc::new(EventStore::new()),
        &parent_ctx,
        Arc::new(crate::session::SessionBinding::ephemeral_root()),
        test_envelope().child_policy,
        tokio_util::sync::CancellationToken::new(),
    );

    assert_eq!(
        child_ctx.workspace_root(),
        Some(std::path::Path::new("/tmp/workspace-root")),
        "the parent's confinement root must be forwarded to the child",
    );
    assert_eq!(
        child_ctx.working_dir(),
        PathBuf::from("/tmp/parent-wd"),
        "the child's working dir must be seeded from the parent's current dir",
    );

    // Snapshot semantics: the child owns its handle, so a child-side
    // `cd` must not move the parent's working dir.
    child_ctx.set_working_dir(PathBuf::from("/tmp/child-moved"));
    assert_eq!(
        parent_ctx.working_dir(),
        PathBuf::from("/tmp/parent-wd"),
        "child working-dir mutations must not propagate to the parent",
    );
}

/// Hook-coverage regression: the parent's shared [`HookRegistry`]
/// extension must be forwarded to the child context so the child's
/// own spawn sites (grandchildren) can reach it.
#[test]
fn child_context_forwards_hook_registry_extension() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let infra = AgentToolInfra {
        registry: AgentRegistry::shared(),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: Uuid::new_v4(),
        parent_id: None,
        grant: None,
        tool_registry: Some(Arc::new(ToolRegistry::new())),
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    };
    let parent_ctx = ToolContext::empty();
    let hooks = Arc::new(HookRegistry::new());
    parent_ctx.insert_extension(Arc::clone(&hooks));

    let child_ctx = build_child_context(
        &infra,
        Uuid::new_v4(),
        Arc::new(EventStore::new()),
        &parent_ctx,
        Arc::new(crate::session::SessionBinding::ephemeral_root()),
        test_envelope().child_policy,
        tokio_util::sync::CancellationToken::new(),
    );

    let forwarded = child_ctx
        .get_extension::<HookRegistry>()
        .ok_or("required test value")?;
    assert!(
        Arc::ptr_eq(&forwarded, &hooks),
        "the child must share the parent's hook registry instance",
    );
    Ok(())
}

/// Builds a provider turn carrying a single `read` tool call.
fn read_call_turn(item_id: &str, path: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::ToolCallDelta {
            item_id: item_id.to_string(),
            call_id: None,
            name: Some("read".to_string()),
            arguments_delta: json!({ "path": path }).to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ]
}

/// Collects the `read` tool results from a child store in event order.
fn read_results(events: &[SessionEvent]) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolResult {
                tool_name, output, ..
            } if tool_name == "read" => Some(output.clone()),
            _ => None,
        })
        .collect()
}

/// Confinement-escape regression (blocker), end to end: a parent
/// confined to a workspace root spawns a child; the child's `read`
/// of an out-of-root file is REFUSED while an in-root read works.
#[tokio::test]
async fn spawned_child_file_tools_respect_parent_confinement() -> TestResult {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let in_path = root.path().join("inside.txt");
    std::fs::write(&in_path, "inside-content")?;
    let out_path = outside.path().join("secret.txt");
    std::fs::write(&out_path, "secret-content")?;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        read_call_turn("tc-out", &out_path.to_string_lossy()),
        read_call_turn("tc-in", &in_path.to_string_lossy()),
        vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ],
    ]));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(crate::tools::read::ReadTool::new()));
    let registry = Arc::new(registry);

    let agent_registry = AgentRegistry::shared();
    let mut ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        registry,
        Arc::new(MessageRouter::new()),
    );
    ctx.confine_to_workspace(root.path().to_path_buf());

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "read files", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;
    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
    let child_store = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .event_store(child_id)
        .ok_or("required test value")?;

    let results = read_results(&child_store.events());
    assert_eq!(results.len(), 2, "both reads produced results: {results:?}");
    assert_eq!(
        results[0]["kind"], "confinement_refused",
        "the out-of-root read must be refused inside the child: {}",
        results[0],
    );
    assert_eq!(
        results[1]["kind"], "text",
        "the in-root read must succeed inside the child: {}",
        results[1],
    );
    assert!(
        results[1]["content"]
            .as_str()
            .ok_or("required test value")?
            .contains("inside-content"),
        "the in-root read must return the file content: {}",
        results[1],
    );
    Ok(())
}

/// Working-dir regression (blocker): a child must resolve relative
/// paths under the parent's working dir, not the process CWD.
#[tokio::test]
async fn spawned_child_resolves_relative_paths_under_parent_working_dir() -> TestResult {
    let wd = tempfile::tempdir()?;
    std::fs::write(wd.path().join("norn-rel-probe.txt"), "rel-probe-content")?;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        read_call_turn("tc-rel", "norn-rel-probe.txt"),
        vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ],
    ]));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(crate::tools::read::ReadTool::new()));
    let registry = Arc::new(registry);

    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        registry,
        Arc::new(MessageRouter::new()),
    );
    ctx.set_working_dir(wd.path().to_path_buf());

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "read rel", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;
    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
    let child_store = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .event_store(child_id)
        .ok_or("required test value")?;

    let results = read_results(&child_store.events());
    assert_eq!(results.len(), 1, "the read produced a result: {results:?}");
    assert_eq!(
        results[0]["kind"], "text",
        "the relative read must resolve under the parent's working dir, \
             not the process CWD: {}",
        results[0],
    );
    assert!(
        results[0]["content"]
            .as_str()
            .ok_or("required test value")?
            .contains("rel-probe-content"),
        "the relative read must return the probe content: {}",
        results[0],
    );
    Ok(())
}

/// Hook-coverage regression (reviewer issue): a `PreToolUse` hook
/// registered on the parent must observe a spawned child's tool
/// calls — the child's loop dispatches hooks from its own
/// `LoopContext`, so the parent's registry must be forwarded.
#[tokio::test]
async fn parent_pre_tool_hook_fires_for_spawned_child_tool_call() -> TestResult {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use crate::integration::hooks::{Hook, PreToolHook};

    struct CountingPreTool {
        tool_name: &'static str,
        count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PreToolHook for CountingPreTool {
        async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            if envelope.tool_name == self.tool_name {
                self.count.fetch_add(1, AtomicOrdering::SeqCst);
            }
            HookOutcome::Proceed
        }
    }

    let turn1 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc1".to_string(),
            call_id: None,
            name: Some("probe".to_string()),
            arguments_delta: "{}".to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let turn2 = vec![
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(),
    ];
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoStubTool { tool_name: "probe" }));
    let registry = Arc::new(registry);

    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        registry,
        Arc::new(MessageRouter::new()),
    );
    let count = Arc::new(AtomicUsize::new(0));
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(Hook::PreTool(Box::new(CountingPreTool {
        tool_name: "probe",
        count: Arc::clone(&count),
    })));
    ctx.insert_extension(Arc::new(hook_registry));

    let tool = SpawnAgentTool::new();
    spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "probe it", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    assert_eq!(
        count.load(AtomicOrdering::SeqCst),
        1,
        "a parent-registered PreToolUse hook must fire for the child's tool call",
    );
    Ok(())
}
