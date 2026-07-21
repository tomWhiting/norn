use super::*;

/// Sink double that accepts everything EXCEPT `Custom` events — lets
/// the `ChildBranch` reservation through but refuses the
/// `subagent.started` audit, isolating the Started-audit failure
/// path.
struct CustomRefusingSink;
impl crate::session::store::PersistenceSink for CustomRefusingSink {
    fn persist(
        &mut self,
        event: &SessionEvent,
    ) -> Result<(), crate::session::persistence::SessionPersistError> {
        match event {
            SessionEvent::Custom { .. } => {
                Err(crate::session::persistence::SessionPersistError::Io(
                    std::io::Error::other("sink refused the audit"),
                ))
            }
            _ => Ok(()),
        }
    }
}

/// F1 regression: a `subagent.started` persist failure aborts the
/// spawn BEFORE the reservation is confirmed — the tool errors AND
/// the registry holds no phantom Active child afterwards (the
/// unconfirmed guard's RAII rollback reclaims the slot; a
/// post-confirm failure would have pinned the parent's
/// `max_concurrent_children` budget forever, with no wrapper left to
/// transition the entry).
#[tokio::test]
async fn started_audit_failure_aborts_spawn_without_phantom_child() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(&agent_registry),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::with_sink(Box::new(CustomRefusingSink))),
        agent_id: parent,
        parent_id: None,
        grant: None,
        tool_registry: Some(Arc::new(ToolRegistry::new())),
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    });
    let ctx = ToolContext::empty();
    ctx.insert_extension(infra);
    ctx.insert_extension(Arc::new(AgentHandles::new()));
    ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
    ctx.insert_extension(Arc::new(test_envelope()));

    let tool = SpawnAgentTool::new();
    let result = tool
        .execute(
            &envelope_for(json!({"task": "t", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err("a Started-audit persist failure must abort the spawn".into());
    };
    assert!(
        err.to_string().contains("subagent.started"),
        "the failure names the refused audit: {err}",
    );

    // No phantom child: the unconfirmed reservation rolled back, so
    // the registry lists nothing and the parent's concurrency budget
    // is untouched.
    let reg = agent_registry.read();
    assert!(
        reg.list().is_empty(),
        "the rolled-back reservation must leave no registry entry: {:?}",
        reg.list(),
    );
    assert!(reg.tombstones().is_empty(), "and no tombstone");
    Ok(())
}
