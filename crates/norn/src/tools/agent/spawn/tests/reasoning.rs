use super::*;

/// Owner ruling 2026-07-07 (context window overrideable), end to
/// end through the spawn tool: an explicit
/// `child_policy.loop_config.context_window` above the catalogued
/// child model's maximum is rejected as a typed error naming the
/// child knob — never a silent clamp, never a launched child whose
/// protections sit beyond the real wall.
#[tokio::test]
async fn oversized_explicit_child_window_is_rejected_at_spawn() -> TestResult {
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
                "task": "t",
                "role": "worker",
                "model": "gpt-5.3-codex-spark",
                "child_policy": {
                    "messaging": "siblings_and_parent",
                    "delegation": { "remaining_depth": 0, "max_concurrent_children": 4 },
                    "inbound_capacity": 8,
                    "loop_config": { "context_window": 272_000 },
                },
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err("an oversized explicit child window must abort the spawn".into());
    };
    let reason = err.to_string();
    assert!(reason.contains("272000"), "names the override: {reason}");
    assert!(reason.contains("128000"), "names the catalog max: {reason}");
    assert!(
        reason.contains("child_policy.loop_config.context_window"),
        "names the child knob: {reason}",
    );
    assert!(
        agent_registry.read().list().is_empty(),
        "the refused spawn leaves no registry entry",
    );
    Ok(())
}

/// Re-review R2, end to end through the spawn tool: a variant whose
/// EXPLICITLY configured reasoning effort is not supported by the
/// child's resolved model is refused as a typed error naming the
/// setting — BEFORE the reservation, so no registry entry and no
/// audit residue survive the refusal ("none" is declared for no
/// catalogued model — factual catalog content).
#[tokio::test]
async fn variant_effort_unsupported_by_child_model_is_rejected_at_spawn() -> TestResult {
    use std::collections::BTreeMap;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let mut configured = BTreeMap::new();
    configured.insert(
        "scout".to_owned(),
        crate::config::types::VariantSettings {
            prompt: Some("Scout the area.".to_owned()),
            reasoning_effort: Some("none".to_owned()),
            ..crate::config::types::VariantSettings::default()
        },
    );
    let catalog =
        crate::agent::variants::VariantCatalog::build(Some(&configured), &std::env::temp_dir())?;
    ctx.insert_extension(Arc::new(catalog));

    let tool = SpawnAgentTool::new();
    let result = tool
        .execute(
            &envelope_for(json!({
                "task": "t",
                "variant": "scout",
                "model": CATALOG_MODEL,
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err("an unsupported explicit variant effort must abort the spawn".into());
    };
    let reason = err.to_string();
    assert!(
        reason.contains("variants.scout.reasoning_effort"),
        "names the setting: {reason}",
    );
    assert!(
        reason.contains("low, medium, high, xhigh"),
        "lists the model's catalogued efforts: {reason}",
    );
    assert!(
        agent_registry.read().list().is_empty(),
        "the refused spawn leaves no registry entry",
    );
    Ok(())
}

/// Owner ruling 2026-07-07 (reasoning effort inherited): with no
/// variant effort and no profile effort, the child inherits the
/// parent's ACTIVE effort from the live per-step stamp — asserted on
/// the child's actual provider requests. A parent running with no
/// effort passes None through unchanged (today's behavior).
#[tokio::test]
async fn child_inherits_parents_active_reasoning_effort() -> TestResult {
    use crate::provider::request::ReasoningEffort;

    for (parent_effort, expected) in [
        (Some(ReasoningEffort::High), Some(ReasoningEffort::High)),
        (None, None),
    ] {
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
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        // The parent's live per-step stamp: model + effort together.
        ctx.insert_extension(Arc::new(AgentModel {
            model: CATALOG_MODEL.to_owned(),
            reasoning_effort: parent_effort,
        }));

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "inherit effort", "role": "worker"}),
        )
        .await;

        let requests = captured.lock().clone();
        assert!(!requests.is_empty(), "the child made a provider call");
        for request in &requests {
            assert_eq!(
                request.reasoning_effort, expected,
                "the child inherits exactly the parent's active effort \
                     (parent: {parent_effort:?})",
            );
        }
    }
    Ok(())
}

/// §3.6: a variant's reasoning effort reaches the child's actual
/// provider requests.
#[tokio::test]
async fn variant_reasoning_effort_reaches_child_provider_requests() -> TestResult {
    use std::collections::BTreeMap;

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
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    // Owner ruling 2026-07-07: the variant's effort WINS over the
    // parent's live effort — stamp a conflicting parent effort to
    // prove precedence, not just presence.
    ctx.insert_extension(Arc::new(AgentModel {
        model: CATALOG_MODEL.to_owned(),
        reasoning_effort: Some(crate::provider::request::ReasoningEffort::High),
    }));
    let mut configured = BTreeMap::new();
    configured.insert(
        "scout".to_owned(),
        crate::config::types::VariantSettings {
            prompt: Some("Scout the area.".to_owned()),
            reasoning_effort: Some("low".to_owned()),
            ..crate::config::types::VariantSettings::default()
        },
    );
    let catalog =
        crate::agent::variants::VariantCatalog::build(Some(&configured), &std::env::temp_dir())?;
    ctx.insert_extension(Arc::new(catalog));

    let tool = SpawnAgentTool::new();
    spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "scout it", "variant": "scout", "model": CATALOG_MODEL}),
    )
    .await;

    let requests = captured.lock().clone();
    assert!(!requests.is_empty(), "the child made a provider call");
    for request in &requests {
        assert_eq!(
            request.reasoning_effort,
            Some(crate::provider::request::ReasoningEffort::Low),
            "the variant's reasoning effort must ride every child request \
                 (and win over the parent's live High effort)",
        );
    }
    Ok(())
}

/// R5 (spawn side): the spawned child's own context carries ITS OWN
/// base system instruction under `ParentSystemInstruction` — proven
/// from inside the child via a probe tool — so the child's own forks
/// inherit the child's context, not the grandparent's.
#[tokio::test]
async fn spawned_child_context_carries_its_own_base_instruction() -> TestResult {
    struct BaseProbe {
        seen: Arc<StdMutex<Option<String>>>,
    }
    #[async_trait]
    impl TestTool for BaseProbe {
        fn name(&self) -> &'static str {
            "base_probe"
        }
        fn description(&self) -> &'static str {
            "records the ParentSystemInstruction it sees"
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
            ctx: &ToolContext,
        ) -> Result<TestToolOutput, ToolError> {
            *self.seen.lock() = ctx
                .get_extension::<crate::agent::fork::ParentSystemInstruction>()
                .map(|ext| ext.as_str().to_owned());
            Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
        }
    }

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-probe".to_string(),
                call_id: None,
                name: Some("base_probe".to_string()),
                arguments_delta: "{}".to_string(),
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
    let seen = Arc::new(StdMutex::new(None));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(BaseProbe {
        seen: Arc::clone(&seen),
    }));
    let agent_registry = AgentRegistry::shared();
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
        json!({"task": "probe your base", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let base = seen.lock().clone().ok_or("required test value")?;
    assert!(
        base.contains("probe your base"),
        "the published value is the CHILD's own task-derived base: {base}",
    );
    Ok(())
}
