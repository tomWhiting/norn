//! Internal pipeline used by [`crate::tools::agent::fork_tool::ForkTool`].
//!
//! Houses the helpers that build the fork's per-child
//! [`ToolContext`](crate::tool::context::ToolContext), seed
//! the child [`EventStore`] with the parent's events plus a synthetic
//! tool-result for the fork call (R2), branch the
//! [`SessionTree`](crate::session::tree::SessionTree) when one is published
//! (R4), filter the parent registry's tool definitions through the per-fork
//! allow-list (R8), and drive the `tokio::spawn` launch / completion
//! transitions (R1, R4). Lives next to the public tool surface so
//! [`crate::tools::agent::fork_tool::ForkTool::execute`] reads top-to-bottom
//! while staying inside the per-file 500-line production-code limit (CO5).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use parking_lot::RwLock;
use tokio::sync::watch;
use uuid::Uuid;

use super::handle::{AgentHandle, AgentHandles, ChildBranchMetadata, SharedSessionTree};
use super::infra::{AgentToolInfra, SubAgentExecutor};
use crate::agent::fork::{ContextFilter, OrphanToolCall, ParentSystemInstruction};
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::error::{NornError, SessionError};
use crate::integration::DiagnosticCollector;
use crate::internal::extraction::SharedProvider;
use crate::r#loop::inbound::{InboundChannel, InboundSender};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::runner::{AgentLoopConfig, AgentStepRequest, AgentStepResult, run_agent_step};
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;
use crate::session::tree::{BranchConfig, SessionId, SessionMetadata, SessionStatus};
use crate::tool::context::ToolContext;
use crate::tool::registry::ToolRegistry;
use crate::tools::task::SharedTaskStore;
use crate::tools::tool_search::SharedToolCatalog;

/// Bounded capacity of the fork's inbound steering channel — mirrors the
/// value used by [`super::spawn`] so the two surfaces behave identically.
pub(super) const FORK_INBOUND_BUFFER: usize = 32;

/// Project the parent registry's tools through the optional allow-list (R8).
pub(super) fn build_fork_tool_definitions(
    registry: &ToolRegistry,
    allow_list: Option<&[String]>,
) -> Vec<ToolDefinition> {
    let allow_set: Option<HashSet<&str>> =
        allow_list.map(|names| names.iter().map(String::as_str).collect());
    registry
        .names()
        .filter(|name| allow_set.as_ref().is_none_or(|set| set.contains(name)))
        .filter_map(|name| {
            registry.get(name).map(|tool| ToolDefinition {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: crate::tool::wrap_schema_with_envelope(tool.input_schema()),
            })
        })
        .collect()
}

/// Construct the per-fork [`ToolContext`](crate::tool::context::ToolContext) (R3).
///
/// Fresh [`AgentToolInfra`] carrying the child's own `agent_id` / `parent_id`
/// and its own [`EventStore`], plus a fresh [`AgentHandles`] so the fork can
/// spawn grandchildren in turn. Shared infrastructure is forwarded from the
/// parent context so tasks, tool discovery, the parent's base system
/// instruction, and any orchestrator-published [`SharedSessionTree`] stay
/// reachable from inside the fork.
pub(super) fn build_fork_context(
    parent_infra: &AgentToolInfra,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    parent_ctx: &ToolContext,
    child_tree: Option<SharedSessionTree>,
) -> Arc<ToolContext> {
    let child_infra = AgentToolInfra {
        registry: Arc::clone(&parent_infra.registry),
        mailbox: Arc::clone(&parent_infra.mailbox),
        provider: Arc::clone(&parent_infra.provider),
        event_store: child_store,
        agent_id: child_id,
        parent_id: Some(parent_infra.agent_id),
        tool_registry: parent_infra.tool_registry.as_ref().map(Arc::clone),
    };

    let child_ctx = ToolContext::empty();
    child_ctx.insert_extension(Arc::new(child_infra));
    child_ctx.insert_extension(Arc::new(AgentHandles::new()));
    if let Some(task_store) = parent_ctx.get_extension::<SharedTaskStore>() {
        child_ctx.insert_extension(task_store);
    }
    if let Some(catalog) = parent_ctx.get_extension::<SharedToolCatalog>() {
        child_ctx.insert_extension(catalog);
    }
    if let Some(diagnostics) = parent_ctx.get_extension::<DiagnosticCollector>() {
        child_ctx.insert_extension(diagnostics);
    }
    if let Some(sp) = parent_ctx.get_extension::<SharedProvider>() {
        child_ctx.insert_extension(sp);
    }
    if let Some(parent_base) = parent_ctx.get_extension::<ParentSystemInstruction>() {
        child_ctx.insert_extension(parent_base);
    }
    if let Some(ch) =
        parent_ctx.get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
    {
        child_ctx.insert_extension(ch);
    }
    if let Some(tree) = child_tree {
        child_ctx.insert_extension(Arc::new(tree));
    }
    Arc::new(child_ctx)
}

/// Resolved child store, optional tree handle, and optional session id.
pub(super) type ForkStoreResolution = (
    Arc<EventStore>,
    Option<SharedSessionTree>,
    Option<SessionId>,
);

/// Resolve the fork's child [`EventStore`] and (when published) the child's
/// own [`SharedSessionTree`] handle (R4).
///
/// When a [`SharedSessionTree`] is present, `tree.branch()` seeds the child
/// store with the full parent context, so the caller must NOT re-seed
/// those events. The returned `tree_seeded` flag signals this to
/// [`seed_fork_events`].
pub(super) fn resolve_fork_store(
    parent_ctx: &ToolContext,
    model: &str,
) -> Result<(ForkStoreResolution, bool), NornError> {
    let Some(parent_tree) = parent_ctx.get_extension::<SharedSessionTree>() else {
        return Ok(((Arc::new(EventStore::new()), None, None), false));
    };

    let branch_config = BranchConfig {
        context_filter: ContextFilter::default(),
        metadata: SessionMetadata {
            created_at: Utc::now(),
            model: model.to_owned(),
            role: Some(format!("fork/{model}")),
            status: SessionStatus::Active,
        },
    };
    let child_session_id = parent_tree
        .tree
        .branch(parent_tree.session_id, branch_config)?;
    let store = parent_tree
        .tree
        .get_store(child_session_id)
        .ok_or_else(|| {
            NornError::Session(SessionError::StorageError {
                reason: "fork: branched session id missing from session tree".to_owned(),
            })
        })?;
    let child_tree = SharedSessionTree {
        tree: Arc::clone(&parent_tree.tree),
        session_id: child_session_id,
    };
    Ok(((store, Some(child_tree), Some(child_session_id)), true))
}

/// Seed the fork's child [`EventStore`] with the full parent events plus
/// the synthetic tool result that closes the orphan fork `tool_call` (R2).
///
/// When `tree_seeded` is true, `tree.branch()` already populated the child
/// store with the full parent context — only the synthetic fork result is
/// appended. When false (standalone mode, no tree), all parent events are
/// copied then the synthetic result is appended.
pub(super) fn seed_fork_events(
    child_store: &EventStore,
    parent_events: &[SessionEvent],
    fork_call_id: Option<&str>,
    fork_id: Uuid,
    tree_seeded: bool,
) -> Result<(), NornError> {
    if tree_seeded {
        // Close ALL orphan tool_calls in the child store unconditionally.
        // No guard conditions — if any tool_call anywhere in the child's
        // event history lacks a matching ToolResult, inject a synthetic one.
        // This mirrors Codex's ensure_call_outputs_present approach: fix
        // orphans generically rather than trying to predict specific causes.
        let child_events = child_store.events();
        let all_orphans = find_all_orphan_tool_calls(&child_events);
        if !all_orphans.is_empty() {
            let ids: Vec<&str> = all_orphans.iter().map(|o| o.id.as_str()).collect();
            tracing::info!(
                fork_id = %fork_id,
                fork_call_id = ?fork_call_id,
                orphan_count = all_orphans.len(),
                orphan_ids = ?ids,
                child_event_count = child_events.len(),
                "fork: closing orphan tool_calls in child context",
            );
        }
        for orphan in &all_orphans {
            let is_fork_call = fork_call_id.is_some_and(|fid| fid == orphan.id);
            let output = if is_fork_call {
                serde_json::json!({
                    "fork_id": fork_id.to_string(),
                    "status": "active",
                    "message": crate::agent::fork::FORK_SYNTHETIC_RESULT_MESSAGE,
                })
            } else {
                serde_json::json!({
                    "status": "in_progress",
                    "message": "executing on parent agent",
                })
            };
            let tool_name = if is_fork_call {
                "fork".to_owned()
            } else {
                orphan.name.clone()
            };
            child_store
                .append(SessionEvent::ToolResult {
                    base: EventBase::new(child_store.last_event_id()),
                    tool_call_id: orphan.id.clone(),
                    tool_name,
                    output,
                    duration_ms: 0,
                })
                .map_err(|e| {
                    NornError::Session(SessionError::EventAppendFailed {
                        reason: e.to_string(),
                    })
                })?;
        }
    } else {
        // Standalone mode (no tree) — copy all parent events then close
        // ALL orphan tool_calls unconditionally, same as the tree path.
        let mut events = parent_events.to_vec();
        let all_orphans = find_all_orphan_tool_calls(&events);
        if !all_orphans.is_empty() {
            let ids: Vec<&str> = all_orphans.iter().map(|o| o.id.as_str()).collect();
            tracing::info!(
                fork_id = %fork_id,
                fork_call_id = ?fork_call_id,
                orphan_count = all_orphans.len(),
                orphan_ids = ?ids,
                "fork: closing orphan tool_calls in standalone mode",
            );
        }
        for orphan in &all_orphans {
            let is_fork_call = fork_call_id.is_some_and(|fid| fid == orphan.id);
            let output = if is_fork_call {
                serde_json::json!({
                    "fork_id": fork_id.to_string(),
                    "status": "active",
                    "message": crate::agent::fork::FORK_SYNTHETIC_RESULT_MESSAGE,
                })
            } else {
                serde_json::json!({
                    "status": "in_progress",
                    "message": "executing on parent agent",
                })
            };
            let tool_name = if is_fork_call {
                "fork".to_owned()
            } else {
                orphan.name.clone()
            };
            let parent_id = events.last().map(|e| e.base().id.clone());
            events.push(SessionEvent::ToolResult {
                base: EventBase::new(parent_id),
                tool_call_id: orphan.id.clone(),
                tool_name,
                output,
                duration_ms: 0,
            });
        }
        for event in &events {
            child_store.append(event.clone()).map_err(|e| {
                NornError::Session(SessionError::EventAppendFailed {
                    reason: e.to_string(),
                })
            })?;
        }
    }
    Ok(())
}

/// Scan ALL `AssistantMessage` events for `tool_call`s without a matching
/// `ToolResult` anywhere after them. Returns every orphan across the entire
/// history, not just the latest turn. This is the unconditional safety net
/// that ensures the child context never reaches the API with orphans.
fn find_all_orphan_tool_calls(events: &[SessionEvent]) -> Vec<OrphanToolCall> {
    use std::collections::HashSet;

    let mut result_ids: HashSet<String> = HashSet::new();
    for event in events {
        if let SessionEvent::ToolResult { tool_call_id, .. } = event {
            result_ids.insert(tool_call_id.clone());
        }
    }

    let mut orphans = Vec::new();
    for event in events {
        if let SessionEvent::AssistantMessage { tool_calls, .. } = event {
            for tc in tool_calls {
                if !result_ids.contains(&tc.call_id) {
                    orphans.push(OrphanToolCall {
                        id: tc.call_id.clone(),
                        name: tc.name.clone(),
                    });
                }
            }
        }
    }
    orphans
}

/// Outcome bundle the fork's `tokio::spawn` task hands back to the parent's
/// timeline and result channel.
pub(crate) struct ForkOutcome {
    pub(crate) status: AgentStatus,
    pub(crate) result_summary: serde_json::Value,
    pub(crate) usage: Usage,
    pub(crate) duration: std::time::Duration,
    pub(crate) error_message: Option<String>,
}

/// Mark the fork's terminal registry status and project the agent loop's
/// result into a transport-friendly payload.
pub(super) fn finish_fork(
    registry: &RwLock<AgentRegistry>,
    fork_id: Uuid,
    outcome: Result<AgentStepResult, NornError>,
    started: Instant,
) -> ForkOutcome {
    let duration = started.elapsed();
    match outcome {
        Ok(result) => {
            {
                let mut reg = registry.write();
                if let Err(e) = reg.mark_completing(fork_id) {
                    tracing::warn!(fork_id = %fork_id, error = %e, "fork: mark_completing failed");
                }
                if let Err(e) = reg.mark_completed(fork_id) {
                    tracing::warn!(fork_id = %fork_id, error = %e, "fork: mark_completed failed");
                }
            }
            let (summary, usage) = match result {
                AgentStepResult::Completed { output, usage } => (output, usage),
                AgentStepResult::SchemaUnreachable {
                    best_attempt,
                    usage,
                    ..
                } => (best_attempt.unwrap_or(serde_json::Value::Null), usage),
                AgentStepResult::MaxIterationsReached { usage }
                | AgentStepResult::Cancelled { usage } => (serde_json::Value::Null, usage),
                AgentStepResult::TimedOut { partial_output, .. } => (
                    partial_output.unwrap_or(serde_json::Value::Null),
                    Usage::default(),
                ),
            };
            ForkOutcome {
                status: AgentStatus::Completed,
                result_summary: summary,
                usage,
                duration,
                error_message: None,
            }
        }
        Err(err) => {
            if let Err(mark_err) = registry.write().mark_failed(fork_id) {
                tracing::warn!(
                    fork_id = %fork_id,
                    error = %mark_err,
                    "fork: mark_failed failed after run error",
                );
            }
            ForkOutcome {
                status: AgentStatus::Failed,
                result_summary: serde_json::Value::Null,
                usage: Usage::default(),
                duration,
                error_message: Some(err.to_string()),
            }
        }
    }
}

/// Append a [`SessionEvent::ForkComplete`] to the parent's store (R4).
///
/// Best-effort: a failure here is logged but does not propagate. The fork's
/// own audit trail already lives on its branch — this event is the
/// completion reference on the parent's timeline.
pub(super) fn append_fork_complete(
    parent_store: &EventStore,
    forked_session_id: Option<SessionId>,
    outcome: &ForkOutcome,
    fork_id: Uuid,
) {
    let event = SessionEvent::ForkComplete {
        base: EventBase::new(parent_store.last_event_id()),
        forked_session_id: forked_session_id
            .map_or_else(|| fork_id.to_string(), |id| id.to_string()),
        result_summary: outcome.result_summary.clone(),
        usage: EventUsage {
            input_tokens: outcome.usage.input_tokens,
            output_tokens: outcome.usage.output_tokens,
            cache_read_tokens: outcome.usage.cache_read_tokens,
            cache_write_tokens: outcome.usage.cache_write_tokens,
            cost_usd: outcome.usage.cost_usd,
        },
        duration_ms: u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX),
    };
    if let Err(e) = parent_store.append(event) {
        tracing::warn!(
            fork_id = %fork_id,
            error = %e,
            "fork: failed to append ForkComplete event to parent store",
        );
    }
}

/// Resources moved into a fork's `tokio::spawn` task.
pub(super) struct ForkLaunch {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) executor: SubAgentExecutor,
    pub(super) child_store: Arc<EventStore>,
    pub(super) parent_store: Arc<EventStore>,
    pub(super) loop_ctx: LoopContext,
    pub(super) tool_defs: Vec<ToolDefinition>,
    pub(super) output_schema: serde_json::Value,
    pub(super) inbound_rx: InboundChannel,
    pub(super) request: String,
    pub(super) model: String,
    pub(super) agent_registry: Arc<RwLock<AgentRegistry>>,
    pub(super) result_sender: Option<ChildResultSender>,
    pub(super) requirement_names: Vec<String>,
    pub(super) fork_id: Uuid,
    pub(super) parent_id: Uuid,
    pub(super) forked_session_id: Option<SessionId>,
    pub(super) event_sender: Option<AgentEventSender>,
}

/// Launch the fork on its own `tokio::spawn` task and build the parent-side
/// [`AgentHandle`].
pub(super) fn launch_fork(launch: ForkLaunch, inbound_tx: InboundSender) -> AgentHandle {
    let ForkLaunch {
        provider,
        executor,
        child_store,
        parent_store,
        mut loop_ctx,
        tool_defs,
        output_schema,
        mut inbound_rx,
        request,
        model,
        agent_registry,
        result_sender,
        requirement_names,
        fork_id,
        parent_id,
        forked_session_id,
        event_sender,
    } = launch;

    let handle_store = Arc::clone(&child_store);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    let agent_role = format!("fork/{model}");

    let join_handle = tokio::spawn(async move {
        let started = Instant::now();
        let step_result = run_agent_step(AgentStepRequest {
            provider: provider.as_ref(),
            executor: &executor,
            store: child_store.as_ref(),
            user_prompt: &request,
            tools: &tool_defs,
            output_schema: Some(&output_schema),
            model: &model,
            config: &AgentLoopConfig::default(),
            event_tx: event_sender.as_ref(),
            inbound: Some(&mut inbound_rx),
            loop_context: &mut loop_ctx,
            cancel: None,
        })
        .await;

        if let Err(ref e) = step_result {
            tracing::error!(
                fork_id = %fork_id,
                model = %model,
                error = %e,
                elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "fork: run_agent_step failed",
            );
        }

        let outcome = finish_fork(&agent_registry, fork_id, step_result, started);
        append_fork_complete(parent_store.as_ref(), forked_session_id, &outcome, fork_id);

        if let Some(sender) = result_sender {
            let (succeeded, formatted_message, error) =
                crate::agent::fork::format_fork_outcome(fork_id, &outcome, &requirement_names);
            let result = ChildAgentResult {
                agent_id: fork_id,
                agent_role,
                succeeded,
                formatted_message,
                error,
            };
            if let Err(e) = sender.0.send(result).await {
                tracing::error!(
                    fork_id = %fork_id,
                    error = %e,
                    "fork: failed to send result through child result channel",
                );
            }
        }

        let _ = status_tx.send_replace(outcome.status);
    });

    AgentHandle {
        agent_id: fork_id,
        status_rx,
        inbound_tx,
        join_handle,
        event_store: handle_store,
        branch_metadata: ChildBranchMetadata {
            child_agent_id: fork_id,
            parent_agent_id: parent_id,
            profile_name: None,
            spawned_at: Utc::now(),
        },
    }
}
