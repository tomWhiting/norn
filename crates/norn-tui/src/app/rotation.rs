//! Store-rotation rewiring for `/new`.
//!
//! Swapping [`RuntimeRefs::store`](super::event_loop::RuntimeRefs) alone
//! is not a session rotation: two components captured the startup store
//! at driver wiring time (`norn-cli/src/tui/driver.rs`) and keep serving
//! the OLD conversation if they are not repointed:
//!
//! 1. The [`ActionLog`] installed by norn-cli's `install_action_log` —
//!    referenced from both `LoopContext::action_log` (loop dispatch
//!    recording) and the executor's shared
//!    [`ToolContext`](norn::tool::context::ToolContext) extension slot
//!    (the `action_log` tool's queries).
//! 2. The [`AgentToolInfra`] installed by `install_agent_tool_infra` —
//!    its `event_store` seeds forked and spawned children, so a stale
//!    handle forks the *previous* conversation.
//!
//! [`rotate_store_dependents`] performs the whole swap as one infallible
//! commit: callers finish every fallible step (session creation, sink
//! attach) **before** invoking it, matching `handle_new`'s
//! no-partially-rotated-state contract. The old store receives an
//! explicit final [`EventStore::checkpoint`] before being replaced —
//! components may pin `Arc` clones of it, deferring the sink's
//! drop-flush indefinitely, so the rotation cannot rely on drop to
//! land the final index delta (event count, usage, `updated_at`).

use std::sync::Arc;

use norn::agent_loop::loop_context::LoopContext;
use norn::session::action_log::ActionLog;
use norn::session::store::EventStore;
use norn::tool::context::ToolContext;
use norn::tools::agent::AgentToolInfra;

/// Repoint every store-captured component at `new_store`, then swap it
/// into `store_slot`.
///
/// Infallible by design — all fallible rotation work (creating and
/// sink-registering the new session) happens in the caller before any
/// state is touched. The steps, in order:
///
/// 1. **Checkpoint the OLD store.** Flushes the sink's pending index
///    delta now, because `Arc` clones of the old store held elsewhere
///    can defer its drop-flush indefinitely. Failure is warn-logged and
///    never aborts the rotation: the write-through JSONL event file is
///    already durable; only the index entry lags until its next flush.
/// 2. **Rebuild the [`ActionLog`]** against the new store with the loop
///    context's live working-dir handle — the same construction
///    norn-cli's `install_action_log` uses at startup. The rebuild from
///    the new store's events is a no-op for a fresh `/new` session but
///    keeps this helper correct for any store handed to it.
/// 3. **Repoint the shared [`ToolContext`] extensions**: the action-log
///    slot gets the new ledger, and [`AgentToolInfra`] is rebuilt with
///    `event_store` pointing at the new store while **reusing** every
///    cross-rotation-stable handle from the existing infra — the live
///    agent registry (the status panel's hold-window state lives
///    there), the mailbox, the provider, the agent identity, and the
///    tool registry. When no infra was installed (the startup wiring is
///    conditional on a shared context existing), none is fabricated —
///    agent tools keep reporting the missing [`AgentToolInfra`]
///    extension (a typed `MissingExtension` error) exactly as before
///    the rotation.
/// 4. **Swap** `LoopContext::action_log` and `store_slot`.
pub(super) fn rotate_store_dependents(
    shared_ctx: Option<Arc<ToolContext>>,
    store_slot: &mut Arc<EventStore>,
    loop_context: &mut LoopContext,
    new_store: Arc<EventStore>,
) {
    // 1. Final checkpoint of the OLD store. Components pinning Arc
    //    clones of it defer the sink's drop-flush indefinitely, so the
    //    pending index delta must be flushed here, before the swap.
    if let Err(err) = store_slot.checkpoint() {
        tracing::warn!(
            error = %err,
            "final checkpoint of the rotated-out session store failed; \
             its index entry will lag until the store's sink drops",
        );
    }

    // 2. Fresh ActionLog wrapping the NEW store — same construction as
    //    norn-cli's startup `install_action_log`, including the live
    //    working-dir handle and the rebuild (a no-op for a fresh store).
    let action_log = Arc::new(ActionLog::with_working_dir(
        Arc::clone(&new_store),
        loop_context.working_dir.clone(),
    ));
    norn::agent::rebuild_action_log(&action_log, &new_store.events());

    // 3. Repoint the shared tool-context extensions.
    if let Some(ctx) = shared_ctx {
        ctx.insert_extension(Arc::clone(&action_log));
        if let Some(old_infra) = ctx.get_extension::<AgentToolInfra>() {
            ctx.insert_extension(Arc::new(AgentToolInfra {
                registry: Arc::clone(&old_infra.registry),
                mailbox: Arc::clone(&old_infra.mailbox),
                provider: Arc::clone(&old_infra.provider),
                event_store: Arc::clone(&new_store),
                agent_id: old_infra.agent_id,
                parent_id: old_infra.parent_id,
                tool_registry: old_infra.tool_registry.clone(),
            }));
        } else {
            // Startup wiring never installed one (it is conditional on
            // a shared context with agent tools); nothing to repoint.
            tracing::debug!(
                "no AgentToolInfra on the shared tool context; \
                 skipping event-store repoint during session rotation",
            );
        }
    } else {
        tracing::debug!(
            "executor exposes no shared tool context; \
             rotating the loop-side action log only",
        );
    }

    // 4. Swap the loop-side ledger and the store slot.
    loop_context.action_log = Some(action_log);
    *store_slot = new_store;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    use norn::agent::mailbox::Mailbox;
    use norn::agent::registry::AgentRegistry;
    use norn::error::ProviderError;
    use norn::provider::request::ProviderRequest;
    use norn::provider::traits::{Provider, ProviderStream};
    use norn::session::events::{EventBase, SessionEvent};
    use norn::session::{CreateSessionOptions, DurabilityPolicy, SessionManager, read_index};
    use norn::tool::registry::ToolRegistry;
    use uuid::Uuid;

    /// Provider stub for constructing an [`AgentToolInfra`] — rotation
    /// never dispatches through it, so streaming always errors.
    struct StubProvider;

    impl Provider for StubProvider {
        fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            Err(ProviderError::ConnectionFailed {
                reason: "stub provider — rotation tests never stream".to_owned(),
            })
        }
    }

    /// A `ToolResult` event with the given call id, used to prove which
    /// store an [`ActionLog`] rebuild observed.
    fn tool_result(call_id: &str) -> SessionEvent {
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: call_id.to_owned(),
            tool_name: "read".to_owned(),
            output: serde_json::json!({ "content": "ok" }),
            duration_ms: 3,
        }
    }

    #[test]
    fn rotation_repoints_action_log_to_new_store() {
        // Regression: `/new` previously swapped `runtime.store` only,
        // leaving `LoopContext::action_log` and the shared-context
        // extension slot wrapping the OLD store — action-log queries
        // answered from the previous conversation.
        let ctx = Arc::new(ToolContext::empty());
        let old_store = Arc::new(EventStore::new());
        old_store.append(tool_result("old-call")).unwrap();
        let old_log = Arc::new(ActionLog::new(Arc::clone(&old_store)));
        norn::agent::rebuild_action_log(&old_log, &old_store.events());
        ctx.insert_extension(Arc::clone(&old_log));
        let mut loop_context = LoopContext {
            action_log: Some(Arc::clone(&old_log)),
            ..LoopContext::default()
        };

        // Seeding the new store before rotation proves the rebuilt
        // ledger observed the NEW store's events, not the old one's.
        let new_store = Arc::new(EventStore::new());
        new_store.append(tool_result("new-call")).unwrap();

        let mut store_slot = Arc::clone(&old_store);
        rotate_store_dependents(
            Some(Arc::clone(&ctx)),
            &mut store_slot,
            &mut loop_context,
            Arc::clone(&new_store),
        );

        assert!(
            Arc::ptr_eq(&store_slot, &new_store),
            "store slot must hold the new store",
        );
        let rotated = loop_context
            .action_log
            .clone()
            .expect("loop context must still carry an action log");
        assert!(
            !Arc::ptr_eq(&rotated, &old_log),
            "loop context must observe a fresh ActionLog, not the old ledger",
        );
        assert!(
            rotated.entry("new-call").is_some(),
            "rotated ActionLog must be rebuilt from the NEW store's events",
        );
        assert!(
            rotated.entry("old-call").is_none(),
            "rotated ActionLog must not carry the previous conversation",
        );
        let ctx_log = ctx
            .get_extension::<ActionLog>()
            .expect("shared context must keep an action-log extension");
        assert!(
            Arc::ptr_eq(&ctx_log, &rotated),
            "the action_log tool and the loop must share one ledger",
        );
    }

    #[test]
    fn rotation_repoints_agent_infra_event_store_reusing_runtime_identity() {
        // Regression: `AgentToolInfra.event_store` previously kept the
        // startup store after `/new`, so fork/spawn seeded children
        // from the previous conversation. Everything that must stay
        // stable across rotation — the live agent registry (status-line
        // hold windows), mailbox, provider, identity, tool registry —
        // is reused, never re-created.
        let ctx = Arc::new(ToolContext::empty());
        let old_store = Arc::new(EventStore::new());
        let agent_registry = AgentRegistry::shared();
        let mailbox = Arc::new(Mailbox::new());
        let provider: Arc<dyn Provider> = Arc::new(StubProvider);
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_id = Uuid::now_v7();
        let parent_id = Some(Uuid::now_v7());
        ctx.insert_extension(Arc::new(AgentToolInfra {
            registry: Arc::clone(&agent_registry),
            mailbox: Arc::clone(&mailbox),
            provider: Arc::clone(&provider),
            event_store: Arc::clone(&old_store),
            agent_id,
            parent_id,
            tool_registry: Some(Arc::clone(&tool_registry)),
        }));
        let mut loop_context = LoopContext::default();
        let mut store_slot = Arc::clone(&old_store);
        let new_store = Arc::new(EventStore::new());

        rotate_store_dependents(
            Some(Arc::clone(&ctx)),
            &mut store_slot,
            &mut loop_context,
            Arc::clone(&new_store),
        );

        let infra = ctx
            .get_extension::<AgentToolInfra>()
            .expect("infra must survive rotation");
        assert!(
            Arc::ptr_eq(&infra.event_store, &new_store),
            "fork/spawn must seed children from the NEW session store",
        );
        assert!(
            Arc::ptr_eq(&infra.registry, &agent_registry),
            "the live agent registry must be reused, not re-created",
        );
        assert!(
            Arc::ptr_eq(&infra.mailbox, &mailbox),
            "the mailbox must be reused so in-flight signals keep routing",
        );
        assert!(
            Arc::ptr_eq(&infra.provider, &provider),
            "the provider handle must be reused",
        );
        assert_eq!(infra.agent_id, agent_id, "root identity must be stable");
        assert_eq!(infra.parent_id, parent_id, "genealogy must be stable");
        let kept_registry = infra
            .tool_registry
            .clone()
            .expect("tool registry must survive rotation");
        assert!(
            Arc::ptr_eq(&kept_registry, &tool_registry),
            "the tool registry must be reused",
        );
    }

    #[test]
    fn rotation_checkpoints_old_store_index_delta_before_swap() {
        // Regression (integration-track flag): `/new` relied on the old
        // store's drop-flush for its final index delta, but components
        // holding Arc clones of the old store defer that drop
        // indefinitely. The rotation must checkpoint explicitly.
        let tmp = tempfile::tempdir().unwrap();
        let opened = SessionManager::new(tmp.path())
            .create(
                CreateSessionOptions {
                    model: "test-model".to_owned(),
                    working_dir: "/tmp".to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .unwrap();
        let entry = opened.entry;
        let old_store = Arc::new(opened.store);
        // Simulate a component still pinning the old store — its
        // drop-flush cannot run while this clone is alive.
        let pinned = Arc::clone(&old_store);
        old_store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "before rotation".to_owned(),
            })
            .unwrap();

        let mut loop_context = LoopContext::default();
        let mut store_slot = Arc::clone(&old_store);
        rotate_store_dependents(
            None,
            &mut store_slot,
            &mut loop_context,
            Arc::new(EventStore::new()),
        );

        // The old store is still alive via `pinned`, so only the
        // explicit checkpoint can have flushed the index delta.
        let index = read_index(tmp.path()).unwrap();
        let indexed = index.iter().find(|e| e.id == entry.id).unwrap();
        assert_eq!(
            indexed.event_count, 1,
            "the old session's index delta must be flushed before the swap, \
             not deferred to a drop that pinning components prevent",
        );
        drop(pinned);
    }

    #[test]
    fn rotation_without_shared_context_still_rebuilds_action_log() {
        // Ephemeral runs can lack a shared tool context; the loop-side
        // ActionLog must still be repointed so dispatch recording lands
        // in the new conversation.
        let old_store = Arc::new(EventStore::new());
        let old_log = Arc::new(ActionLog::new(Arc::clone(&old_store)));
        let mut loop_context = LoopContext {
            action_log: Some(Arc::clone(&old_log)),
            ..LoopContext::default()
        };
        let new_store = Arc::new(EventStore::new());
        new_store.append(tool_result("new-call")).unwrap();

        let mut store_slot = Arc::clone(&old_store);
        rotate_store_dependents(
            None,
            &mut store_slot,
            &mut loop_context,
            Arc::clone(&new_store),
        );

        assert!(Arc::ptr_eq(&store_slot, &new_store));
        let rotated = loop_context.action_log.clone().unwrap();
        assert!(!Arc::ptr_eq(&rotated, &old_log));
        assert!(
            rotated.entry("new-call").is_some(),
            "loop-side ActionLog must wrap the NEW store even without a \
             shared tool context",
        );
    }

    #[test]
    fn rotation_without_existing_infra_does_not_fabricate_one() {
        // Startup wiring installs AgentToolInfra conditionally; when it
        // was never installed, rotation must not invent one (it has no
        // provider or identity to give it) — agent tools keep failing
        // with their configuration error exactly as before `/new`.
        let ctx = Arc::new(ToolContext::empty());
        let old_store = Arc::new(EventStore::new());
        let mut loop_context = LoopContext::default();
        let mut store_slot = Arc::clone(&old_store);
        let new_store = Arc::new(EventStore::new());

        rotate_store_dependents(
            Some(Arc::clone(&ctx)),
            &mut store_slot,
            &mut loop_context,
            Arc::clone(&new_store),
        );

        assert!(
            ctx.get_extension::<AgentToolInfra>().is_none(),
            "no AgentToolInfra existed before rotation, none may appear after",
        );
        assert!(
            ctx.get_extension::<ActionLog>().is_some(),
            "the action-log extension is still installed for the new store",
        );
        assert!(Arc::ptr_eq(&store_slot, &new_store));
    }
}
