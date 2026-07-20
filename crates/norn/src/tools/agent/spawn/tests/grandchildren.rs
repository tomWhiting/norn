use super::*;

pub(super) fn idle_grandchild_entry(
    registry: &RwLock<AgentRegistry>,
) -> Option<crate::agent::registry::AgentEntry> {
    registry.read().list().into_iter().find(|entry| {
        entry.path.matches("/spawn/").count() == 2 && entry.status == AgentStatus::Idle
    })
}

/// Routes provider scripts by conversation identity (the first user
/// message) so a mid-tree child and its grandchild can share the one
/// workspace provider deterministically; the child's would-stop turn
/// is held until the registry shows the grandchild parked idle, which
/// guarantees its result is already in the child's channel.
struct TreeProvider {
    registry: Arc<RwLock<AgentRegistry>>,
    child_calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl Provider for TreeProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        use std::sync::atomic::Ordering as AtomicOrdering;
        // The grandchild's run always ends its request with its own
        // task prompt ("grandchild-task"); every child turn ends with
        // something else (the child's prompt, a tool result, or an
        // injected <agent_result> frame). Note the *first* user
        // message would be wrong here: in session-tree mode a spawned
        // child's branch store is seeded with its parent's context,
        // so every conversation in the tree starts with the root
        // prompt.
        // The managed dynamic-context Developer message now rides at the
        // tail of every request (prompt-cache fix), so route on the last
        // non-Developer message — the turn content that actually seeds
        // this child.
        let last = request
            .messages
            .iter()
            .rev()
            .find(|m| !matches!(m.role, crate::provider::request::MessageRole::Developer))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        if last == "grandchild-task" {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta {
                    text: "grandchild says hi".to_string(),
                }),
                Ok(done_event()),
            ])));
        }
        let call = self.child_calls.fetch_add(1, AtomicOrdering::SeqCst);
        match call {
            0 => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::ToolCallDelta {
                    item_id: "tc-grandchild".to_string(),
                    call_id: None,
                    name: Some("spawn_agent".to_string()),
                    arguments_delta: json!({
                        "task": "grandchild-task",
                        "model": CATALOG_MODEL,
                        "role": "leaf",
                    })
                    .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                }),
                Ok(done_event_tool_use()),
            ]))),
            1 => {
                let registry = Arc::clone(&self.registry);
                let s = stream::once(async move {
                    for _ in 0..2400 {
                        if idle_grandchild_entry(&registry).is_some() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    assert!(
                        idle_grandchild_entry(&registry).is_some(),
                        "grandchild never parked idle - the test cannot proceed",
                    );
                })
                .flat_map(|()| {
                    stream::iter(vec![
                        Ok(ProviderEvent::TextDelta {
                            text: "waited for grandchild".to_string(),
                        }),
                        Ok(done_event()),
                    ])
                });
                Ok(Box::pin(s))
            }
            _ => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta {
                    text: "child done after grandchild".to_string(),
                }),
                Ok(done_event()),
            ]))),
        }
    }
}

/// W3.4 end-to-end: with an envelope granting depth 2, a spawned child
/// spawns a grandchild; the grandchild's result is delivered into the
/// **child's** conversation (one hop — never to the root), the child's
/// own result reaches the root's channel, the agents tree nests, and
/// every spawned actor remains idle and addressable.
#[tokio::test]
async fn grandchild_results_bubble_one_hop_and_idle_at_every_level() -> TestResult {
    let agent_registry = AgentRegistry::shared();
    let provider: Arc<dyn Provider> = Arc::new(TreeProvider {
        registry: Arc::clone(&agent_registry),
        child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(SpawnAgentTool::new()));
    let root_id = Uuid::new_v4();
    // Persistent parent (V2-R2): child and grandchild conversations
    // land on REAL on-disk timelines, readable after reclamation —
    // and Gap 11's depth-2 traffic is asserted from disk below.
    let tmp = tempfile::tempdir()?;
    let (ctx, manager, root_session_id) = persistent_parent_ctx(
        tmp.path(),
        provider,
        root_id,
        &agent_registry,
        Arc::new(tool_registry),
    )?;
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 2;
    ctx.insert_extension(Arc::new(envelope));

    // Root result channel + delivery-anchored marker. Persistent
    // spawned children deliberately ignore the marker on natural
    // completion so they remain wakeable.
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "child-task", "model": CATALOG_MODEL, "role": "lead"})),
            &ctx,
        )
        .await?;
    assert!(!out.is_error(), "{:?}", out.content);
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;
    let child_path = out.content["path"]
        .as_str()
        .ok_or("required test value")?
        .to_string();
    assert!(child_path.starts_with("/spawn/"), "{child_path}");

    // The root receives exactly one result: the child's — the
    // grandchild's bubbled one hop, never skipping a level.
    let child_result = tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await?
        .ok_or("required test value")?;
    assert_eq!(child_result.agent_id, child_id);
    assert!(child_result.succeeded, "{:?}", child_result.error);
    assert!(
        child_result
            .formatted_message
            .contains("child done after grandchild"),
        "the child's final answer is the delivered result: {}",
        child_result.formatted_message,
    );
    assert!(
        rx.try_recv().is_err(),
        "the grandchild's result must never reach the root directly",
    );

    // Wakeable tree at every level: both entries remain observable,
    // idle, and correctly linked.
    wait_for_condition(
        || {
            let reg = agent_registry.read();
            let child_idle = reg
                .get(child_id)
                .is_some_and(|entry| entry.status == AgentStatus::Idle);
            let grandchild_idle = reg.children(child_id).iter().any(|entry| {
                entry.status == AgentStatus::Idle
                    && entry.path.starts_with(&format!("{child_path}/spawn/"))
            });
            child_idle && grandchild_idle
        },
        "child and grandchild must both park idle",
    )
    .await;
    let reg = agent_registry.read();
    assert_eq!(
        reg.tombstones().len(),
        0,
        "natural idle creates no tombstones"
    );
    let child_entry = reg.get(child_id).ok_or("required test value")?;
    assert_eq!(child_entry.parent_id, Some(root_id));
    assert_eq!(child_entry.status, AgentStatus::Idle);
    let grandchild_entry = reg
        .children(child_id)
        .into_iter()
        .find(|entry| entry.path.starts_with(&format!("{child_path}/spawn/")))
        .ok_or("required test value")?;
    assert_eq!(
        grandchild_entry.parent_id,
        Some(child_id),
        "the grandchild's parent is the mid-tree child, not the root",
    );
    assert_eq!(grandchild_entry.status, AgentStatus::Idle);
    assert!(
        grandchild_entry
            .path
            .starts_with(&format!("{child_path}/spawn/")),
        "grandchild path nests under the child: {}",
        grandchild_entry.path,
    );
    drop(reg);

    // One-hop delivery into the child's conversation, DURABLY: the
    // child's on-disk session holds the framed grandchild result
    // (Gap 1 + Gap 11 closure asserted from disk, not memory).
    let rows = crate::session::persistence::index::read_index(manager.data_dir())?;
    let child_row = rows
        .iter()
        .find(|r| r.parent_id.as_deref() == Some(root_session_id.as_str()))
        .ok_or("required test value")?;
    let child_events = events_on_disk(&manager, &child_row.id);
    let injected = child_events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::UserMessage { content, .. }
                if content.contains("<agent_result")
                    && content.contains("grandchild says hi")
        )
    });
    assert!(
        injected,
        "the grandchild's framed result must be durably injected into \
             the child's conversation",
    );
    // And the grandchild's session persisted under the child, keyed by
    // the full-path slug in the SAME root-keyed children/ dir.
    let grandchild_row = rows
        .iter()
        .find(|r| r.parent_id.as_deref() == Some(child_row.id.as_str()))
        .ok_or("required test value")?;
    let grandchild_rel = grandchild_row
        .rel_path
        .as_deref()
        .ok_or("required test value")?;
    assert!(
        grandchild_rel.starts_with(&format!("{root_session_id}/children/"))
            && grandchild_rel.contains("--"),
        "grandchild file must be keyed by the full path slug: {grandchild_rel}",
    );
    assert!(
        tmp.path().join(grandchild_rel).exists(),
        "grandchild timeline file must exist on disk",
    );
    assert!(
        !events_on_disk(&manager, &grandchild_row.id).is_empty(),
        "the grandchild's own events must reach disk (Gap 11)",
    );
    Ok(())
}
