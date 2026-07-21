use super::*;

// ----- agent-variants (R3/R6/§7) -------------------------------------

/// Install the built-in variant catalog on a parent context, the way
/// the assembly seam does.
fn install_builtin_catalog(ctx: &ToolContext) -> TestResult {
    let catalog = crate::agent::variants::VariantCatalog::build(None, &std::env::temp_dir())?;
    ctx.insert_extension(Arc::new(catalog));
    Ok(())
}

/// R3 + acceptance: a spawned `explorer`'s provider-facing tool list
/// (the actual first provider call payload) contains NO
/// `write/edit/bash/apply_patch` — registry-level filtering, not call
/// rejection. The parent's model is inherited (no explicit model)
/// from the published `AgentModel` ground truth.
#[tokio::test]
async fn spawned_explorer_provider_tool_list_has_no_write_tools() -> TestResult {
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(CapturingProvider {
        captured: Arc::clone(&captured),
        responses: StdMutex::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "explored".to_string(),
            },
            done_event(),
        ]]),
    });

    let mut registry = ToolRegistry::new();
    for name in ["read", "search", "write", "edit", "bash", "apply_patch"] {
        registry.register(Box::new(EchoStubTool { tool_name: name }));
    }
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(registry),
        Arc::new(MessageRouter::new()),
    );
    install_builtin_catalog(&ctx)?;
    ctx.insert_extension(Arc::new(crate::tools::agent::infra::AgentModel {
        model: CATALOG_MODEL.to_owned(),
        reasoning_effort: None,
    }));

    let tool = SpawnAgentTool::new();
    spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "map the crate", "variant": "explorer"}),
    )
    .await;

    let defs = captured.lock().clone();
    let names: Vec<String> = defs
        .iter()
        .map(|def| match def {
            ProviderToolDefinition::Function(function) => function.name.clone(),
            other @ ProviderToolDefinition::Hosted(_) => format!("{other:?}"),
        })
        .collect();
    for forbidden in ["write", "edit", "bash", "apply_patch"] {
        assert!(
            !names.iter().any(|n| n == forbidden),
            "explorer's provider payload must not offer '{forbidden}': {names:?}",
        );
    }
    assert!(
        names.iter().any(|n| n == "read") && names.iter().any(|n| n == "search"),
        "explorer keeps its read-only subset: {names:?}",
    );
    Ok(())
}

/// R6: a leaf child (granted `remaining_depth` == 0 — the default
/// derivation from a depth-1 parent) is shown NEITHER `spawn_agent` nor
/// fork in its provider payload, even with no allow-list at all.
#[tokio::test]
async fn leaf_child_provider_tool_list_omits_spawn_and_fork() -> TestResult {
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(CapturingProvider {
        captured: Arc::clone(&captured),
        responses: StdMutex::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ]]),
    });

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoStubTool { tool_name: "read" }));
    registry.register(Box::new(SpawnAgentTool::new()));
    registry.register(Box::new(crate::tools::agent::fork_tool::ForkTool::new()));
    let agent_registry = AgentRegistry::shared();
    // test_envelope grants the parent depth 1 → the child's derived
    // grant is depth 0: a leaf.
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(registry),
        Arc::new(MessageRouter::new()),
    );

    let tool = SpawnAgentTool::new();
    spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "leaf work", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let defs = captured.lock().clone();
    let names: Vec<String> = defs
        .iter()
        .map(|def| match def {
            ProviderToolDefinition::Function(function) => function.name.clone(),
            other @ ProviderToolDefinition::Hosted(_) => format!("{other:?}"),
        })
        .collect();
    assert!(
        !names.iter().any(|n| n == "spawn_agent") && !names.iter().any(|n| n == "fork"),
        "a leaf must not SEE delegation tools: {names:?}",
    );
    assert!(
        names.iter().any(|n| n == "read"),
        "non-delegation tools survive: {names:?}",
    );
    Ok(())
}

/// Acceptance: a child spawned with NO model (and a variant that sets
/// none) launches on the PARENT's model — asserted against the actual
/// provider request, not the descriptor. The parent's model comes
/// from its agent-registry entry (runtime ground truth).
#[tokio::test]
async fn no_model_child_launches_on_parents_model_from_registry() -> TestResult {
    // A catalogued model that is NOT the catalog default, so the
    // assertion cannot pass by accident (factual catalog id).
    let parent_model = "gpt-5.4-mini";
    assert_ne!(parent_model, CATALOG_MODEL, "test precondition");

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
    let parent_guard = AgentRegistry::reserve(
        &agent_registry,
        "/parent".to_owned(),
        "orchestrator".to_owned(),
        parent_model.to_owned(),
        None,
        test_envelope().child_policy,
        None,
    )?;
    let parent_id = parent_guard.id();
    parent_guard.confirm()?;

    let ctx = parent_ctx(
        provider,
        parent_id,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    install_builtin_catalog(&ctx)?;

    let tool = SpawnAgentTool::new();
    spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "inherit", "variant": "implementer"}),
    )
    .await;

    let requests = captured.lock().clone();
    assert!(!requests.is_empty(), "the child made a provider call");
    for request in &requests {
        assert_eq!(
            request.model, parent_model,
            "the child's provider calls must run on the parent's model",
        );
    }
    Ok(())
}

/// The reviewer ruling end-to-end on the spawn surface: no model
/// anywhere → typed error naming `variants.reviewer.model`, and
/// nothing is reserved or persisted.
#[tokio::test]
async fn reviewer_without_model_fails_naming_config_key() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    install_builtin_catalog(&ctx)?;

    let tool = SpawnAgentTool::new();
    let result = tool
        .execute(
            &envelope_for(json!({"task": "review", "variant": "reviewer"})),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err("reviewer without a model must be refused".into());
    };
    assert!(
        err.to_string().contains("variants.reviewer.model"),
        "the failure names the missing config key: {err}",
    );
    assert!(
        agent_registry.read().is_empty(),
        "a refused spawn reserves nothing",
    );
    Ok(())
}

/// §7: a child on an uncatalogued model is rejected BEFORE launch —
/// mirroring the root's `oversized_explicit_window_is_rejected_at_build`
/// semantics (children have no explicit-window escape hatch, so the
/// catalog is the only source of a truthful window) — and the
/// rejection leaves no registry entry.
#[tokio::test]
async fn child_with_uncatalogued_model_is_rejected_before_launch() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let tool = SpawnAgentTool::new();
    let result = tool
        .execute(
            &envelope_for(json!({
                "task": "t", "model": "not-in-catalog-model-xyz", "role": "worker",
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err("an uncatalogued child model must be rejected".into());
    };
    assert!(
        err.to_string().contains("not-in-catalog-model-xyz"),
        "the rejection names the model: {err}",
    );
    assert!(
        agent_registry.read().is_empty(),
        "the rejection precedes the reservation",
    );
    Ok(())
}

/// H: the `subagent.started` audit on the parent's store discloses
/// the variant durably — descriptor.profile carries the variant name,
/// and the resolved role defaults to it.
#[tokio::test]
async fn subagent_started_audit_discloses_variant() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    install_builtin_catalog(&ctx)?;

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "explore", "variant": "explorer", "model": CATALOG_MODEL}),
    )
    .await;

    let infra = ctx
        .get_extension::<AgentToolInfra>()
        .ok_or("required test value")?;
    let started = infra
        .event_store
        .events()
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == crate::provider::agent_event::SUBAGENT_STARTED_EVENT_TYPE => {
                Some(data)
            }
            _ => None,
        })
        .ok_or("required test value")?;
    assert_eq!(started["child_id"], child_id.to_string());
    assert_eq!(
        started["descriptor"]["profile"], "explorer",
        "the variant is disclosed durably on the started audit: {started}",
    );
    assert_eq!(
        started["descriptor"]["role"], "explorer",
        "the role defaults to the variant name: {started}",
    );
    assert_eq!(started["descriptor"]["model"], CATALOG_MODEL);
    Ok(())
}
