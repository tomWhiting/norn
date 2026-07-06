//! Handle-returning agent operations: `spawn_agent` and `signal_agent`.
//! Each function bridges Rhai's sync callsite into the
//! Tokio runtime via stored `runtime` handles — `spawn_agent` keeps the
//! loop running in the background.

use std::sync::Arc;

use chrono::Utc;
use rhai::{Dynamic, Engine, EvalAltResult, ImmutableString, Map};
use tokio::task::JoinHandle;
use uuid::Uuid;

use chrono::DateTime;

use super::context::{AgentHandle, NornRhaiContext, dynamic_to_json, rhai_error};
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::r#loop::LoopContext;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::r#loop::runner::{AgentStepRequest, ToolExecutor, run_agent_step};
use crate::provider::AgentEventSender;
use crate::provider::agent_event::{AgentMessageLifecycle, SubagentDescriptor, SubagentKind};
use crate::provider::usage::Usage;
use crate::session::events::ChildBranchKind;
use crate::session::{ChildBranchRequest, slugify_name_stem};
use crate::tool::context::ToolContext;
use crate::tools::agent::append_message_audit;
use crate::tools::agent::coord::sender_attribution;
use crate::tools::agent::infra::SubAgentExecutor;
use crate::tools::agent::lifecycle::{LifecycleEmitter, SubagentCompletion};
use crate::tools::agent::spawn_outcome::{extract_outcome_summary, mark_terminal_in_registry};

pub(super) fn register_handle_returning(engine: &mut Engine, context: &NornRhaiContext) {
    // spawn_agent — drives a real `run_agent_step` on the Tokio runtime
    // and stashes its JoinHandle so callers can reap the result.
    {
        let ctx = context.clone();
        engine.register_fn(
            "spawn_agent",
            move |config: Map| -> Result<AgentHandle, Box<EvalAltResult>> {
                spawn_agent(&ctx, &config)
            },
        );
    }

    // signal_agent (AgentHandle recipient, default kind: update)
    {
        let ctx = context.clone();
        engine.register_fn(
            "signal_agent",
            move |to: AgentHandle, content: Dynamic| -> Result<u64, Box<EvalAltResult>> {
                signal_agent(&ctx, to.0, &content, MessageKind::Update)
            },
        );
    }

    // signal_agent (AgentHandle recipient, explicit kind)
    {
        let ctx = context.clone();
        engine.register_fn(
            "signal_agent",
            move |to: AgentHandle,
                  content: Dynamic,
                  kind: ImmutableString|
                  -> Result<u64, Box<EvalAltResult>> {
                signal_agent(&ctx, to.0, &content, parse_kind(kind.as_str())?)
            },
        );
    }

    // signal_agent (String recipient — resolves path or UUID; default
    // kind: update)
    {
        let ctx = context.clone();
        engine.register_fn(
            "signal_agent",
            move |to: ImmutableString, content: Dynamic| -> Result<u64, Box<EvalAltResult>> {
                let id = resolve_recipient(&ctx, to.as_str())?;
                signal_agent(&ctx, id, &content, MessageKind::Update)
            },
        );
    }

    // signal_agent (String recipient, explicit kind)
    {
        let ctx = context.clone();
        engine.register_fn(
            "signal_agent",
            move |to: ImmutableString,
                  content: Dynamic,
                  kind: ImmutableString|
                  -> Result<u64, Box<EvalAltResult>> {
                let id = resolve_recipient(&ctx, to.as_str())?;
                signal_agent(&ctx, id, &content, parse_kind(kind.as_str())?)
            },
        );
    }
}

/// Parse a script-supplied message kind. Only the wire labels are
/// accepted; anything else is a typed script error, never a silent
/// coercion.
fn parse_kind(raw: &str) -> Result<MessageKind, Box<EvalAltResult>> {
    match raw {
        "steer" => Ok(MessageKind::Steer),
        "update" => Ok(MessageKind::Update),
        other => Err(Box::new(rhai_error(format!(
            "signal_agent: unknown kind '{other}' — expected \"steer\" or \"update\""
        )))),
    }
}

/// The honest already-finished script error: a terminal or reclaimed
/// recipient is reported with its recorded completion, mirroring the
/// `signal_agent` tool's failure wording.
fn finished_error(
    identifier: &str,
    status: AgentStatus,
    completed_at: Option<DateTime<Utc>>,
) -> EvalAltResult {
    let when = completed_at.map_or_else(|| "an unrecorded time".to_owned(), |ts| ts.to_rfc3339());
    let outcome = if status == AgentStatus::Failed {
        "failed"
    } else {
        "completed"
    };
    rhai_error(format!(
        "signal_agent: recipient already finished: agent '{identifier}' {outcome} at \
         {when} and can no longer receive messages"
    ))
}

/// Resolve a script-supplied recipient identifier (registry path or raw
/// UUID) to an agent id.
///
/// Paths resolve against registry ground truth, including agents that
/// already finished — a terminal or tombstoned holder produces the honest
/// already-finished error, and only identifiers with no record at all are
/// "unknown". Raw UUIDs pass through unresolved: script hosts may route
/// messages to embedder-managed recipients (e.g. root agents) that are
/// never registry entries, so registry absence is not an error for a UUID
/// — [`signal_agent`] still applies the terminal-state check for UUIDs
/// the registry does know.
fn resolve_recipient(ctx: &NornRhaiContext, to: &str) -> Result<Uuid, Box<EvalAltResult>> {
    if let Ok(parsed) = Uuid::parse_str(to) {
        return Ok(parsed);
    }
    let reg = ctx.registry.read();
    if let Some(entry) = reg.get_by_path(to) {
        if entry.status.is_terminal() {
            return Err(Box::new(finished_error(
                to,
                entry.status,
                entry.completed_at,
            )));
        }
        return Ok(entry.id);
    }
    if let Some(entry) = reg.get_terminal_by_path(to) {
        return Err(Box::new(finished_error(
            to,
            entry.status,
            entry.completed_at,
        )));
    }
    if let Some(tombstone) = reg.tombstone_by_path(to) {
        return Err(Box::new(finished_error(
            to,
            tombstone.status,
            Some(tombstone.completed_at),
        )));
    }
    Err(Box::new(rhai_error(format!(
        "signal_agent: unknown recipient '{to}'"
    ))))
}

/// Deliver `content` to `to_id` through the shared
/// [`MessageRouter`](crate::agent::message_router::MessageRouter),
/// returning the router-minted per-recipient sequence number.
///
/// Rhai is a synchronous embedding, so delivery uses the router's
/// non-blocking path: a recipient with no live inbound route, a closed
/// channel, or a full buffer is a typed script error — never a silent
/// queue into storage nothing drains (the failure mode the deleted
/// `Mailbox` had). A recipient the registry knows to be finished
/// (terminal entry or tombstone) is rejected with the honest
/// already-finished error before any delivery attempt — the same rule as
/// the `signal_agent` tool.
///
/// Attribution comes from the shared [`sender_attribution`] rule —
/// registry path, else tombstone path, else `root` for an unregistered
/// parent-less host (script hosts are root-level orchestrators) — so
/// script sends frame identically to tool sends. Every accepted send
/// appends an `agent_message.sent` audit event to the host's event store
/// (script hosts are roots: there is no scope-granting parent store).
///
/// `kind` defaults to [`MessageKind::Update`] when the script omits it
/// (FYI batching: never interrupts the recipient mid-stream and never
/// wakes a lingering recipient — the pre-router script semantics);
/// passing `"steer"` requests boundary injection and linger wake exactly
/// like the tool surface.
fn signal_agent(
    ctx: &NornRhaiContext,
    to_id: Uuid,
    content: &Dynamic,
    kind: MessageKind,
) -> Result<u64, Box<EvalAltResult>> {
    let json = dynamic_to_json(content)?;
    let body = match json {
        serde_json::Value::String(s) => s,
        other => serde_json::to_string(&other).map_err(|e| {
            Box::new(rhai_error(format!(
                "signal_agent: could not serialize content: {e}"
            )))
        })?,
    };
    let (from_label, from_role, to_label) = {
        let reg = ctx.registry.read();
        // Honest already-finished check for UUID-addressed recipients the
        // registry knows about (path-addressed recipients were checked at
        // resolution). An id with no record at all passes through: it may
        // be an embedder-managed recipient (e.g. a root agent) the router
        // alone knows.
        if let Some(entry) = reg.get(to_id) {
            if entry.status.is_terminal() {
                return Err(Box::new(finished_error(
                    &to_id.to_string(),
                    entry.status,
                    entry.completed_at,
                )));
            }
        } else if let Some(tombstone) = reg.tombstone(to_id) {
            return Err(Box::new(finished_error(
                &to_id.to_string(),
                tombstone.status,
                Some(tombstone.completed_at),
            )));
        }
        let (label, role) = sender_attribution(&reg, ctx.agent_id, None);
        let to_label = reg
            .get(to_id)
            .map_or_else(|| to_id.to_string(), |entry| entry.path);
        (label, role, to_label)
    };
    let message_id = Uuid::new_v4();
    let sent_at = Utc::now();
    let msg = ChannelMessage {
        id: message_id,
        sender_id: ctx.agent_id,
        from: from_label.clone(),
        role: from_role,
        to_id,
        content: body.clone(),
        kind,
        seq: None,
        timestamp: sent_at,
    };
    let seq = ctx
        .router
        .try_deliver(to_id, msg)
        .map_err(|e| Box::new(rhai_error(format!("signal_agent: {e}"))))?;
    // The Sent audit joins the primary write-through contract
    // (session-fidelity Gap 10). The message is ALREADY delivered at this
    // point, so the error wording rules out a duplicate resend.
    append_message_audit(
        &ctx.event_store,
        &AgentMessageLifecycle::Sent {
            message_id,
            from_id: ctx.agent_id,
            from: from_label,
            to_id,
            to: to_label,
            kind,
            seq,
            content: body,
            sent_at,
        },
    )
    .map_err(|error| {
        Box::new(rhai_error(format!(
            "signal_agent: message {message_id} WAS delivered (seq {seq}); do \
             NOT resend it. Persisting the durable Sent audit failed: {error}",
        )))
    })?;
    Ok(seq)
}

fn spawn_agent(ctx: &NornRhaiContext, config: &Map) -> Result<AgentHandle, Box<EvalAltResult>> {
    let task = map_get_string(config, "task")
        .ok_or_else(|| Box::new(rhai_error("spawn_agent: missing 'task'")))?;
    let model = map_get_string(config, "model")
        .ok_or_else(|| Box::new(rhai_error("spawn_agent: missing 'model'")))?;
    let role = map_get_string(config, "role").unwrap_or_else(|| "subagent".to_owned());
    // Auto paths nest under the host's registered path when it has one
    // (W3.4 path namespacing) — the same single implementation the
    // spawn/fork tools use, so the path shape cannot drift.
    let path = map_get_string(config, "path").unwrap_or_else(|| {
        crate::tools::agent::delegation::auto_child_path(&ctx.registry, ctx.agent_id, "spawn")
    });
    let tools = map_get_string_vec(config, "tools");

    let registry = ctx.tool_registry.as_ref().ok_or_else(|| {
        Box::new(rhai_error(
            "spawn_agent: NornRhaiContext.tool_registry is None; orchestrator must \
             supply a ToolRegistry so the sub-agent has tools available",
        ))
    })?;

    // Provenance carried on both typed lifecycle phases, and the label the
    // child's session mint slugs its per-parent name from. Captured before
    // `reserve` consumes `role`.
    let descriptor = SubagentDescriptor {
        kind: SubagentKind::Spawn,
        role: role.clone(),
        model: model.clone(),
        profile: None,
    };
    let name_stem = slugify_name_stem(&role, "spawn");

    // The child's grant is derived from the host's own policy by the same
    // inherit-with-decrement rule the spawn/fork tools use; a depth-0 host
    // is refused typed (W3.4 — script hosts have real budgets too).
    let child_grant = ctx
        .child_policy
        .grant_for_child(None)
        .map_err(|e| Box::new(rhai_error(format!("spawn_agent: {e}"))))?;
    // R5: the granted loop overrides ride the derivation unchanged (rhai
    // spawns have no per-spawn `child_policy` narrowing parameter yet —
    // named follow-up — so the inherited grant is the only source). The
    // resolved config is built inside the task below, exactly like the
    // spawn/fork launch wrappers.
    let child_loop_config = child_grant.loop_config;
    let guard = AgentRegistry::reserve(
        &ctx.registry,
        path,
        role,
        model.clone(),
        Some(ctx.agent_id),
        child_grant,
        Some(&ctx.child_policy),
    )
    .map_err(|e| Box::new(rhai_error(format!("spawn_agent: reserve: {e}"))))?;
    let real_id = guard.id();

    // Mint the child's session through the host's branching binding
    // (V2-R2) BEFORE confirming the reservation — all fallible setup
    // precedes confirm, exactly like the tool sites. A persistent host
    // yields a real write-through child timeline under the root's
    // children/ dir, with the ChildBranch reservation durably on the
    // host's timeline PARENT-FIRST; an ephemeral host propagates
    // ephemerality with the honest `session: None` reservation.
    // The mint's blocking file I/O runs off the executor when the sync
    // rhai host function is driven from inside a runtime thread (F5).
    let branched = crate::tools::agent::delegation::branch_child_off_executor(
        &ctx.session,
        &ctx.event_store,
        &ChildBranchRequest {
            child_session_id: real_id.to_string(),
            name_stem,
            kind: ChildBranchKind::Spawn,
            durability: ctx.session.child_durability(),
            model: model.clone(),
            working_dir: ctx.working_dir.get().display().to_string(),
        },
    )
    .map_err(|e| {
        Box::new(rhai_error(format!(
            "spawn_agent: session branch failed: {e}"
        )))
    })?;
    let child_store = branched.store;

    // Typed lifecycle on both carriers (the rhai-fidelity fix riding
    // Gap 1): `Started` before the child task launches; the task emits
    // `Completed`. Both land as durable Custom audit records on the
    // host's store; the live broadcast additionally fires when the
    // embedder wired a channel. The Started audit fires BEFORE the
    // reservation is confirmed: on a persist failure the guard's RAII
    // rollback reclaims the registry slot, so a refused spawn can never
    // leave a phantom Active child pinning the parent's concurrency
    // budget (the only residue is the already-tolerated burned name +
    // dangling reservation).
    let child_event_sender = ctx
        .events
        .as_ref()
        .map(|tx| AgentEventSender::new(tx.clone(), real_id, format!("spawn/{model}")));
    let lifecycle = LifecycleEmitter::new(
        child_event_sender.clone(),
        Arc::clone(&ctx.event_store),
        ctx.agent_id,
        real_id,
        descriptor,
        Utc::now(),
    );
    lifecycle.emit_started().map_err(|error| {
        Box::new(rhai_error(format!(
            "spawn_agent: failed to persist the subagent.started audit \
                 event; spawn aborted before launch: {error}"
        )))
    })?;

    // All fallible setup is done — confirm the reservation. From here
    // the launch is unconditional and the task owns the entry's
    // terminal transition.
    guard
        .confirm()
        .map_err(|e| Box::new(rhai_error(format!("spawn_agent: confirm: {e}"))))?;

    let provider = Arc::clone(&ctx.provider);
    let registry_for_executor = Arc::clone(registry);
    let agent_registry = Arc::clone(&ctx.registry);
    let model_for_task = model;
    let task_for_async = task;

    let _join_handle: JoinHandle<()> = ctx.runtime.spawn(async move {
        // Dispatch the sub-agent's tool calls through the parent
        // registry's own shared context — matching the behaviour of the
        // prior `registry.execute()` delegation before the
        // `SubAgentExecutor::new` signature gained `child_context`.
        //
        // Known boundary: this is the HOST's shared context, and the
        // model is shown no tools (`tools: &[]` below) — the child's
        // decremented grant on its registry entry is observability-only
        // here. If script children ever gain a real tool surface, they
        // must get their own child context (as the spawn/fork tools
        // build), or their spawns would be charged to the host's
        // identity and budget. Concretely for N-026 cron: this shared
        // context carries the HOST's `ScheduleHandle`, so a script child
        // that could reach the `cron` tool would create schedules against
        // the host's store and identity — but it is shown zero tools, so
        // cron is unreachable here and the trap stays closed until that
        // tool-surface change lands. (The child DOES hold its own
        // session binding for its store; grandchild minting only becomes
        // reachable with that same tool-surface change.)
        //
        // Cancellation boundary (W3.5, deliberate): script children run
        // with `cancel: None` — the rhai host owns no run token to
        // parent a child token under (NornRhaiContext carries none), so
        // there is nothing for the cancellation cascade to chain from,
        // and the host holds no `AgentHandle` for the child either:
        // `close_agent` cannot cancel a script child's run. This is the
        // pre-cascade behavior, unchanged on purpose; if script hosts
        // ever gain a run token, create this child's token via
        // `child_token()` here (and thread it into the request) exactly
        // as the spawn/fork launch paths do.
        let child_context = registry_for_executor
            .shared_context()
            .unwrap_or_else(|| Arc::new(ToolContext::empty()));
        let executor = SubAgentExecutor::new(registry_for_executor, tools, child_context);
        let mut loop_ctx = LoopContext::new("You are a sub-agent. Complete the task and stop.");
        // R5: the child's loop config is the granted ChildLoopConfig
        // applied onto AgentLoopConfig::default(); an absent grant is
        // byte-for-byte the default — the pre-R5 behavior. Same
        // resolution as the spawn/fork launch wrappers.
        let mut child_config =
            crate::agent::child_policy::ChildLoopConfig::resolve(child_loop_config);
        // Arm auto-compaction on the script child exactly as the root
        // builder does (the one shared mechanism): install the token
        // estimator and the context-edit tracker on the child's loop
        // context and fill its context window from the catalog for the
        // child's own model, so a long-running script child compacts
        // instead of dying ContextWindowExceeded. A non-catalog model
        // keeps a None window, leaving the trigger off. NOTE: the root
        // additionally hard-errors on a None/over-max window
        // (2026-07-05 incident guard); per-model child validation is
        // owned by the child-persistence/agent-variants units.
        crate::agent::arming::arm_auto_compaction(
            &mut loop_ctx,
            &mut child_config,
            &model_for_task,
        );
        let outcome = run_agent_step(AgentStepRequest {
            provider: provider.as_ref(),
            executor: &executor,
            store: child_store.as_ref(),
            user_prompt: &task_for_async,
            tools: &[],
            output_schema: None,
            model: &model_for_task,
            config: &child_config,
            event_tx: child_event_sender.as_ref(),
            inbound: None,
            loop_context: &mut loop_ctx,
            cancel: None,
        })
        .await;

        // Terminal projection + registry transition through the SAME
        // classification the spawn/fork wrappers use
        // (`extract_outcome_summary` / `mark_terminal_in_registry`): only
        // a genuine `Completed` step is a registry `Completed`; every
        // stopped variant and every hard error is a Failed run — a
        // capped-out child reported `Completed` would be a silent failure
        // in registry ground truth (REVIEW R5 HIGH-1). Script children
        // have no delegation surface (zero tools), so delivered-children
        // usage is honestly zero.
        let summary = extract_outcome_summary(outcome, Usage::default());
        let subtree_usage = summary.usage.clone() + summary.children_usage.clone();
        // A Completed-audit persist failure is typed at the source and
        // handled here, not propagated: the task has no caller left to
        // fail, and the registry terminal transition below must still
        // run (the same contract as the spawn/fork launch wrappers).
        if let Err(error) = lifecycle.emit_completed(SubagentCompletion {
            usage: summary.usage.clone(),
            subtree_usage,
            succeeded: summary.status == AgentStatus::Completed,
            error: summary.error.clone(),
            stop: summary.stop.clone(),
        }) {
            tracing::error!(
                child_id = %real_id,
                %error,
                "rhai spawn_agent: failed to persist the subagent.completed \
                 audit event on the host store",
            );
        }
        mark_terminal_in_registry(&agent_registry, real_id, summary.status);
    });

    Ok(AgentHandle(real_id))
}

fn map_get_string(map: &Map, key: &str) -> Option<String> {
    map.get(key).and_then(|v| {
        if let Some(s) = v.clone().try_cast::<String>() {
            Some(s)
        } else {
            v.clone()
                .try_cast::<ImmutableString>()
                .map(|s| s.to_string())
        }
    })
}

fn map_get_string_vec(map: &Map, key: &str) -> Option<Vec<String>> {
    map.get(key).and_then(|v| {
        v.clone().try_cast::<rhai::Array>().map(|arr| {
            arr.into_iter()
                .filter_map(|item| {
                    if let Some(s) = item.clone().try_cast::<String>() {
                        Some(s)
                    } else {
                        item.try_cast::<ImmutableString>().map(|s| s.to_string())
                    }
                })
                .collect::<Vec<_>>()
        })
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use uuid::Uuid;

    use super::super::context::{NornRhaiContext, build_norn_engine};
    use crate::agent::message_router::MessageRouter;
    use crate::agent::registry::AgentRegistry;
    use crate::r#loop::inbound::inbound_channel;
    use crate::provider::agent_event::AGENT_MESSAGE_SENT_EVENT_TYPE;
    use crate::provider::mock::MockProvider;
    use crate::provider::traits::Provider;
    use crate::session::events::SessionEvent;
    use crate::session::store::EventStore;
    use crate::tool::registry::ToolRegistry;

    fn build_context() -> NornRhaiContext {
        build_context_with_provider(Arc::new(MockProvider::new(Vec::new())))
    }

    /// A no-op tool so a rhai child's scripted tool-use turns dispatch
    /// cleanly (each tool-use round persists an assistant turn + result,
    /// growing the child's history toward the compaction threshold).
    struct NoopTool;

    #[async_trait::async_trait]
    impl crate::tool::traits::Tool for NoopTool {
        fn name(&self) -> &'static str {
            "noop"
        }
        fn description(&self) -> &'static str {
            "no-op"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> crate::tool::scheduling::ToolEffect {
            crate::tool::scheduling::ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &crate::tool::envelope::ToolEnvelope,
            _ctx: &crate::tool::context::ToolContext,
        ) -> Result<crate::tool::traits::ToolOutput, crate::error::ToolError> {
            Ok(crate::tool::traits::ToolOutput::success(
                serde_json::json!({"ok": true}),
            ))
        }
    }

    /// A provider that serves scripted tool-use turns (each reporting an
    /// oversized usage so the context-edit floor climbs above any window)
    /// and, crucially, flags when it receives an auto-compaction
    /// *summarization* request — identified by its fixed system prompt.
    /// The summarization call only happens when the preflight's estimator
    /// and catalog window (installed by the shared arming) drive the
    /// trigger, so the flag is a direct, order-independent proof that the
    /// rhai spawn site armed the child.
    struct CompactionDetectingProvider {
        saw_summarization: Arc<std::sync::atomic::AtomicBool>,
        tool_turns_remaining: std::sync::Mutex<usize>,
    }

    impl Provider for CompactionDetectingProvider {
        fn stream(
            &self,
            request: crate::provider::request::ProviderRequest,
        ) -> Result<crate::provider::traits::ProviderStream, crate::error::ProviderError> {
            use crate::provider::events::{ProviderEvent, StopReason};
            use crate::provider::usage::Usage;

            let is_summarization = request.messages.iter().any(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("You write compaction summaries"))
            });
            let events = if is_summarization {
                self.saw_summarization
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                vec![
                    ProviderEvent::TextDelta {
                        text: "summary of earlier turns".to_owned(),
                    },
                    ProviderEvent::Done {
                        stop_reason: StopReason::EndTurn,
                        usage: Usage::default(),
                        response_id: None,
                    },
                ]
            } else {
                let mut remaining = self.tool_turns_remaining.lock().expect("lock");
                if *remaining > 0 {
                    *remaining -= 1;
                    vec![
                        ProviderEvent::ToolCallDelta {
                            item_id: "tc-noop".to_owned(),
                            call_id: None,
                            name: Some("noop".to_owned()),
                            arguments_delta: "{}".to_owned(),
                            kind: crate::provider::request::ToolCallKind::Function,
                        },
                        ProviderEvent::Done {
                            stop_reason: StopReason::ToolUse,
                            usage: Usage {
                                input_tokens: 100_000_000,
                                output_tokens: 0,
                                ..Usage::default()
                            },
                            response_id: None,
                        },
                    ]
                } else {
                    vec![
                        ProviderEvent::TextDelta {
                            text: "final".to_owned(),
                        },
                        ProviderEvent::Done {
                            stop_reason: StopReason::EndTurn,
                            usage: Usage::default(),
                            response_id: None,
                        },
                    ]
                }
            };
            Ok(Box::pin(futures_util::stream::iter(
                events.into_iter().map(Ok),
            )))
        }
    }

    /// Hardening (owner ruling 2026-07-03): a rhai-spawned child must run
    /// with auto-compaction armed exactly like the root. The rhai spawn
    /// site calls the shared `arm_auto_compaction` on the child's loop
    /// context and resolved config. The child's own `EventStore` is
    /// internal and detached (no handle exposes it), so this proves the
    /// arming through the *provider*: the child accumulates enough
    /// tool-use history to cross `auto_compact_keep_recent_turns`, its
    /// oversized reported usage pushes the preflight past the compaction
    /// threshold, and the resulting summarization request — recognizable
    /// by its fixed system prompt — sets a flag. That request is
    /// impossible without the estimator and catalog window the shared
    /// arming installs, so the flag proves the rhai site armed the child.
    #[tokio::test(flavor = "multi_thread")]
    async fn rhai_spawn_child_arms_auto_compaction() {
        let saw_summarization = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider: Arc<dyn Provider> = Arc::new(CompactionDetectingProvider {
            saw_summarization: Arc::clone(&saw_summarization),
            // Comfortably more tool-use turns than the default
            // keep_recent_turns (10), so the child's history crosses the
            // compaction floor before it stops.
            tool_turns_remaining: std::sync::Mutex::new(16),
        });

        let mut host_registry = ToolRegistry::new();
        host_registry.register(Box::new(NoopTool));
        let ctx = NornRhaiContext {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            provider,
            agent_id: Uuid::new_v4(),
            runtime: tokio::runtime::Handle::current(),
            event_store: Arc::new(EventStore::new()),
            tool_registry: Some(Arc::new(host_registry)),
            working_dir: crate::tool::context::SharedWorkingDir::default(),
            child_policy: crate::agent::child_policy::ChildPolicy {
                messaging: crate::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: crate::agent::child_policy::DelegationBudget {
                    remaining_depth: 2,
                    max_concurrent_children: 8,
                },
                inbound_capacity: 8,
                loop_config: None,
            },
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
            events: None,
        };
        let registry = Arc::clone(&ctx.registry);
        let catalog_model = crate::model_catalog::default_selection().model;
        let engine = build_norn_engine(&ctx);

        let handle = engine
            .eval::<crate::integration::rhai::AgentHandle>(&format!(
                r#"spawn_agent(#{{ task: "t", model: "{catalog_model}" }})"#
            ))
            .expect("spawn succeeds");
        let child_id = handle.id();

        // The child runs detached; wait for its terminal mark.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if registry
                .read()
                .get(child_id)
                .is_some_and(|entry| entry.status.is_terminal())
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "rhai child never reached a terminal status",
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(
            saw_summarization.load(std::sync::atomic::Ordering::SeqCst),
            "the rhai child's preflight must issue an auto-compaction \
             summarization request, proving the estimator and catalog \
             window were armed on the child",
        );
    }

    fn build_context_with_provider(provider: Arc<dyn Provider>) -> NornRhaiContext {
        let registry = AgentRegistry::shared();
        let router = Arc::new(MessageRouter::new());
        let agent_id = Uuid::new_v4();
        NornRhaiContext {
            registry,
            router,
            provider,
            agent_id,
            runtime: tokio::runtime::Handle::current(),
            event_store: Arc::new(EventStore::new()),
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            working_dir: crate::tool::context::SharedWorkingDir::default(),
            child_policy: crate::agent::child_policy::ChildPolicy {
                messaging: crate::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: crate::agent::child_policy::DelegationBudget {
                    remaining_depth: 2,
                    max_concurrent_children: 8,
                },
                inbound_capacity: 8,
                loop_config: None,
            },
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
            events: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn signal_agent_delivers_via_router() {
        let ctx = build_context();
        let router = ctx.router.clone();
        let target_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        router.register(target_id, tx);

        let engine = build_norn_engine(&ctx);
        let script = format!(
            r#"signal_agent("{}", #{{ kind: "hello", text: "hi" }})"#,
            target_id
        );
        let seq = engine.eval::<u64>(&script).unwrap();
        assert_eq!(seq, 1, "router-minted sequence is returned to the script");

        let msgs = rx.drain();
        assert_eq!(msgs.len(), 1, "exactly one message delivered");
        assert_eq!(msgs[0].sender_id, ctx.agent_id);
        assert_eq!(msgs[0].from, "root", "unregistered script host is root");
        assert_eq!(msgs[0].seq, Some(1));
        let content: serde_json::Value = serde_json::from_str(&msgs[0].content).unwrap();
        assert_eq!(content["kind"], "hello");
        assert_eq!(content["text"], "hi");

        // Audit: the accepted send left a Sent record on the host store.
        let events = ctx.event_store.events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            SessionEvent::Custom {
                event_type, data, ..
            } => {
                assert_eq!(event_type, AGENT_MESSAGE_SENT_EVENT_TYPE);
                assert_eq!(data["seq"], 1);
                assert_eq!(data["to_id"], target_id.to_string());
            }
            other => panic!("expected Sent audit event, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn signal_agent_to_unrouted_recipient_is_a_script_error() {
        let ctx = build_context();
        let target_id = Uuid::new_v4();

        let engine = build_norn_engine(&ctx);
        let script = format!(r#"signal_agent("{target_id}", "hello")"#);
        let err = engine.eval::<u64>(&script).expect_err("no route");
        assert!(
            err.to_string().contains("no live inbound route"),
            "the failure names the missing route: {err}",
        );
        assert!(
            ctx.event_store.events().is_empty(),
            "a rejected send leaves no Sent audit event",
        );
    }

    /// W3.2 parity: the script surface accepts an explicit kind. A
    /// `"steer"` send carries `MessageKind::Steer` on the channel and in
    /// the Sent audit record; an unknown kind is a typed script error.
    #[tokio::test(flavor = "multi_thread")]
    async fn signal_agent_kind_parameter_controls_delivery_kind() {
        use crate::r#loop::inbound::MessageKind;

        let ctx = build_context();
        let target_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        ctx.router.register(target_id, tx);

        let engine = build_norn_engine(&ctx);
        let seq = engine
            .eval::<u64>(&format!(
                r#"signal_agent("{target_id}", "act now", "steer")"#
            ))
            .unwrap();
        assert_eq!(seq, 1);

        let msgs = rx.drain();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].kind, MessageKind::Steer);
        match &ctx.event_store.events()[0] {
            SessionEvent::Custom { data, .. } => assert_eq!(data["kind"], "steer"),
            other => panic!("expected Sent audit event, got {other:?}"),
        }

        // Default stays update when the kind is omitted.
        let seq = engine
            .eval::<u64>(&format!(r#"signal_agent("{target_id}", "fyi")"#))
            .unwrap();
        assert_eq!(seq, 2);
        let msgs = rx.drain();
        assert_eq!(msgs[0].kind, MessageKind::Update);

        // An unknown kind is rejected, never coerced.
        let err = engine
            .eval::<u64>(&format!(r#"signal_agent("{target_id}", "x", "shout")"#))
            .expect_err("unknown kind");
        assert!(
            err.to_string().contains("unknown kind"),
            "the failure names the bad kind: {err}",
        );
    }

    /// W3.2 parity: a recipient the registry knows to be finished —
    /// terminal entry or reclaimed tombstone, addressed by path or UUID —
    /// is the honest already-finished script error, with no Sent record.
    #[tokio::test(flavor = "multi_thread")]
    async fn signal_agent_to_finished_recipient_is_honest_script_error() {
        let ctx = build_context();
        let guard = AgentRegistry::reserve(
            &ctx.registry,
            "/done-child".to_owned(),
            "worker".to_owned(),
            "claude".to_owned(),
            Some(ctx.agent_id),
            ctx.child_policy.grant_for_child(None).unwrap(),
            Some(&ctx.child_policy),
        )
        .unwrap();
        let child = guard.id();
        guard.confirm().unwrap();
        ctx.registry.write().mark_completed(child).unwrap();

        let engine = build_norn_engine(&ctx);
        // Terminal-but-unreclaimed, by path and by UUID.
        for identifier in ["/done-child".to_owned(), child.to_string()] {
            let err = engine
                .eval::<u64>(&format!(r#"signal_agent("{identifier}", "hi")"#))
                .expect_err("finished recipient");
            let message = err.to_string();
            assert!(
                message.contains("already finished") && message.contains("completed at"),
                "the failure states the recorded completion: {message}",
            );
        }
        // Reclaimed down to a tombstone: same honest error.
        assert!(ctx.registry.write().remove_terminal(child));
        let err = engine
            .eval::<u64>(&format!(r#"signal_agent("{child}", "hi")"#))
            .expect_err("tombstoned recipient");
        assert!(
            err.to_string().contains("already finished"),
            "tombstones keep the truth available: {err}",
        );
        assert!(
            ctx.event_store.events().is_empty(),
            "no Sent record for rejected sends",
        );
    }

    /// The sync script path cannot await backpressure: a full bounded
    /// channel is a typed script error (the message is not enqueued and
    /// no sequence number is consumed), never a silent drop.
    #[tokio::test(flavor = "multi_thread")]
    async fn signal_agent_full_channel_is_a_script_error() {
        let ctx = build_context();
        let target_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(1);
        ctx.router.register(target_id, tx);

        let engine = build_norn_engine(&ctx);
        engine
            .eval::<u64>(&format!(r#"signal_agent("{target_id}", "fits")"#))
            .unwrap();
        let err = engine
            .eval::<u64>(&format!(r#"signal_agent("{target_id}", "overflow")"#))
            .expect_err("capacity exhausted");
        assert!(
            err.to_string().contains("channel full"),
            "the failure names the full buffer: {err}",
        );
        assert_eq!(
            ctx.event_store.events().len(),
            1,
            "only the accepted send leaves a Sent record",
        );

        // Drain and retry: the failed send burned no sequence number.
        assert_eq!(rx.drain().len(), 1);
        let seq = engine
            .eval::<u64>(&format!(r#"signal_agent("{target_id}", "retry")"#))
            .unwrap();
        assert_eq!(seq, 2);
    }

    /// Attribution parity: a registered script host attributes by its
    /// registry path and role through the shared `sender_attribution`
    /// rule, and a reclaimed host falls back to its tombstone path.
    #[tokio::test(flavor = "multi_thread")]
    async fn signal_agent_attributes_registered_and_reclaimed_hosts() {
        let mut ctx = build_context();
        let guard = AgentRegistry::reserve(
            &ctx.registry,
            "/host".to_owned(),
            "orchestrator".to_owned(),
            "claude".to_owned(),
            None,
            ctx.child_policy.clone(),
            None,
        )
        .unwrap();
        let host = guard.id();
        guard.confirm().unwrap();
        ctx.agent_id = host;

        let target_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        ctx.router.register(target_id, tx);

        let engine = build_norn_engine(&ctx);
        engine
            .eval::<u64>(&format!(r#"signal_agent("{target_id}", "hello")"#))
            .unwrap();
        let msgs = rx.drain();
        assert_eq!(msgs[0].from, "/host");
        assert_eq!(msgs[0].role.as_deref(), Some("orchestrator"));

        // Reclaim the host: attribution falls back to the tombstone path
        // (no role — tombstones carry none).
        ctx.registry.write().mark_completed(host).unwrap();
        assert!(ctx.registry.write().remove_terminal(host));
        let engine = build_norn_engine(&ctx);
        engine
            .eval::<u64>(&format!(r#"signal_agent("{target_id}", "late note")"#))
            .unwrap();
        let msgs = rx.drain();
        assert_eq!(msgs[0].from, "/host", "tombstone path attribution");
        assert!(msgs[0].role.is_none(), "tombstones carry no role");
    }

    // -- W3.4: script hosts have real delegation budgets ----------------------

    /// A host whose own policy has `remaining_depth = 0` may not spawn:
    /// the script gets the typed, honest refusal naming the budget and
    /// nothing is registered.
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_agent_refused_when_host_depth_exhausted() {
        let mut ctx = build_context();
        ctx.child_policy.delegation.remaining_depth = 0;
        let engine = build_norn_engine(&ctx);

        let err = engine
            .eval::<crate::integration::rhai::AgentHandle>(
                r#"spawn_agent(#{ task: "t", model: "claude" })"#,
            )
            .expect_err("a zero-depth host must be refused");
        assert!(
            err.to_string().contains("delegation depth exhausted"),
            "the refusal names the budget: {err}",
        );
        assert!(ctx.registry.read().is_empty(), "nothing was registered");
    }

    /// A script-spawned child's registry entry carries the host's policy
    /// with the delegation depth decremented one level (the same
    /// derivation the spawn/fork tools use).
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_agent_stamps_decremented_grant() {
        let ctx = build_context();
        assert_eq!(ctx.child_policy.delegation.remaining_depth, 2);
        let engine = build_norn_engine(&ctx);
        let registry = Arc::clone(&ctx.registry);

        let handle = engine
            .eval::<crate::integration::rhai::AgentHandle>(
                r#"spawn_agent(#{ task: "t", model: "claude" })"#,
            )
            .expect("spawn succeeds");

        let entry = registry.read().get(handle.id()).expect("entry registered");
        assert_eq!(
            entry.policy.delegation.remaining_depth, 1,
            "the host's depth-2 policy decrements to 1 on the child",
        );
        assert!(
            entry.path.starts_with("/spawn/"),
            "an unregistered host has no path prefix: {}",
            entry.path,
        );
    }

    /// R5 parity on the script surface: the host policy's `loop_config`
    /// rides the inherit-with-decrement derivation into the child's grant
    /// (registry ground truth), so a script-spawned child runs under the
    /// host's granted loop-shaping knobs. (The `max_iterations` grant was
    /// removed — DECISIONS §0.6(c); the surviving `step_timeout_secs` /
    /// `linger_secs` knobs still ride the derivation unchanged.)
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_agent_grants_host_loop_config() {
        use crate::agent::child_policy::ChildLoopConfig;
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::usage::Usage;

        // One scripted turn: the child completes normally. The point under
        // test is that the host's granted loop_config reaches the child's
        // registry entry, not any iteration-cap behavior.
        let provider = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "done".to_owned(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ]]));
        let mut ctx = build_context_with_provider(Arc::<MockProvider>::clone(&provider));
        let granted = ChildLoopConfig {
            step_timeout_secs: Some(300),
            linger_secs: Some(30),
        };
        ctx.child_policy.loop_config = Some(granted);
        let engine = build_norn_engine(&ctx);
        let registry = Arc::clone(&ctx.registry);

        let handle = engine
            .eval::<crate::integration::rhai::AgentHandle>(
                r#"spawn_agent(#{ task: "t", model: "claude" })"#,
            )
            .expect("spawn succeeds");
        let child_id = handle.id();

        // The derivation carried the host's loop_config through unchanged.
        assert_eq!(
            registry
                .read()
                .get(child_id)
                .expect("entry registered")
                .policy
                .loop_config,
            Some(granted),
        );

        // The script child runs detached; wait for its terminal mark.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if registry
                .read()
                .get(child_id)
                .is_some_and(|entry| entry.status.is_terminal())
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "script child never reached a terminal status",
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// V2-R2, third launch site: a script child spawned under a
    /// PERSISTENT host gets a REAL write-through timeline under the
    /// root's `children/` dir — the index row carries `rel_path` +
    /// `parent_id`, the child's own run events land on disk, and the
    /// host's on-disk file carries the `ChildBranch` reservation plus
    /// the typed `subagent.started` / `subagent.completed` lifecycle
    /// audits (the rhai-fidelity hole).
    #[tokio::test(flavor = "multi_thread")]
    async fn rhai_spawn_under_persistent_host_persists_child_timeline() {
        use crate::provider::agent_event::{
            SUBAGENT_COMPLETED_EVENT_TYPE, SUBAGENT_STARTED_EVENT_TYPE,
        };
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::usage::Usage;
        use crate::session::manager::{CreateSessionOptions, SessionManager};
        use crate::session::persistence::io::read_session_events_for_entry;
        use crate::session::store::DurabilityPolicy;
        use crate::session::{SessionBinding, SessionBrancher};

        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(
                CreateSessionOptions {
                    model: "haiku".to_owned(),
                    working_dir: "/work".to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .expect("create host session");
        let root_id = opened.entry.id.clone();
        let binding = Arc::new(SessionBinding::persistent_root(
            Arc::new(SessionBrancher::new(
                manager.clone(),
                root_id.clone(),
                DurabilityPolicy::Flush,
            )),
            root_id.clone(),
            &[],
        ));

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "script child output".to_owned(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 3,
                    output_tokens: 2,
                    ..Usage::default()
                },
                response_id: None,
            },
        ]]));
        let mut ctx = build_context_with_provider(provider);
        ctx.event_store = Arc::new(opened.store);
        ctx.session = binding;
        let registry = Arc::clone(&ctx.registry);

        let engine = build_norn_engine(&ctx);
        let handle = engine
            .eval::<crate::integration::rhai::AgentHandle>(
                r#"spawn_agent(#{ task: "t", model: "haiku", role: "scout" })"#,
            )
            .expect("spawn_agent evaluates");
        let child_id = handle.id();

        // The script child runs detached; wait for its terminal mark.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if registry
                .read()
                .get(child_id)
                .is_some_and(|entry| entry.status.is_terminal())
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "script child never reached a terminal status",
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Index row: rel_path under the host root's children/ dir, keyed
        // by the SAME id as the child's registry entry.
        let row = manager
            .resolve(&child_id.to_string())
            .expect("script child session indexed");
        let rel = row.rel_path.as_deref().expect("child rows carry rel_path");
        assert!(
            rel.starts_with(&format!("{root_id}/children/scout-"))
                && std::path::Path::new(rel)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl")),
            "script-child file must live under the root's children/ dir: {rel}",
        );
        assert_eq!(row.parent_id.as_deref(), Some(root_id.as_str()));
        assert!(tmp.path().join(rel).exists(), "child timeline file exists");

        // The child's run events are ON DISK (Gap 1, rhai site).
        let child_read = read_session_events_for_entry(tmp.path(), &row).expect("child replays");
        assert!(
            child_read
                .events
                .iter()
                .any(|e| matches!(e, SessionEvent::ChildBranch { .. })),
            "child file carries its ChildBranch provenance header",
        );
        assert!(
            child_read.events.iter().any(|e| matches!(
                e,
                SessionEvent::AssistantMessage { content, .. }
                    if content.contains("script child output")
            )),
            "the script child's own run output must reach its on-disk timeline",
        );

        // Host side ON DISK: reservation + both typed lifecycle audits
        // (previously the rhai path emitted NO subagent events at all).
        let host_entry = manager.resolve(&root_id).expect("host entry");
        let host_read =
            read_session_events_for_entry(tmp.path(), &host_entry).expect("host replays");
        assert!(
            host_read.events.iter().any(|e| matches!(
                e,
                SessionEvent::ChildBranch { child_session_id: Some(c), .. }
                    if *c == child_id.to_string()
            )),
            "the host's file must carry the child's reservation",
        );
        let has_custom = |wanted: &str| {
            host_read.events.iter().any(|e| {
                matches!(
                    e,
                    SessionEvent::Custom { event_type, .. } if event_type == wanted
                )
            })
        };
        assert!(
            has_custom(SUBAGENT_STARTED_EVENT_TYPE),
            "subagent.started must land durably on the host's timeline",
        );
        assert!(
            has_custom(SUBAGENT_COMPLETED_EVENT_TYPE),
            "subagent.completed must land durably on the host's timeline",
        );
    }
}
