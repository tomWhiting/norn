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
use crate::error::NornError;
use crate::r#loop::LoopContext;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::r#loop::runner::{AgentStepRequest, AgentStepResult, ToolExecutor, run_agent_step};
use crate::provider::agent_event::AgentMessageLifecycle;
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tools::agent::append_message_audit;
use crate::tools::agent::coord::sender_attribution;
use crate::tools::agent::infra::SubAgentExecutor;

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
    );
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
    guard
        .confirm()
        .map_err(|e| Box::new(rhai_error(format!("spawn_agent: confirm: {e}"))))?;

    let provider = Arc::clone(&ctx.provider);
    let registry_for_executor = Arc::clone(registry);
    let agent_registry = Arc::clone(&ctx.registry);
    let model_for_task = model;
    let task_for_async = task;

    let _join_handle: JoinHandle<Result<AgentStepResult, NornError>> =
        ctx.runtime.spawn(async move {
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
            // identity and budget.
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
            let child_store = EventStore::new();
            let mut loop_ctx = LoopContext::new("You are a sub-agent. Complete the task and stop.");
            // R5: the child's loop config is the granted ChildLoopConfig
            // applied onto AgentLoopConfig::default(); an absent grant is
            // byte-for-byte the default — the pre-R5 behavior. Same
            // resolution as the spawn/fork launch wrappers.
            let child_config =
                crate::agent::child_policy::ChildLoopConfig::resolve(child_loop_config);
            let outcome = run_agent_step(AgentStepRequest {
                provider: provider.as_ref(),
                executor: &executor,
                store: &child_store,
                user_prompt: &task_for_async,
                tools: &[],
                output_schema: None,
                model: &model_for_task,
                config: &child_config,
                event_tx: None,
                inbound: None,
                loop_context: &mut loop_ctx,
                cancel: None,
            })
            .await;

            // Terminal transitions share the single-owner invariant with the
            // spawn/fork wrappers: this task is the sole owner of the child's
            // terminal sequence, so a failed transition means another actor
            // mutated the entry and is logged as the violation it is.
            //
            // Status classification mirrors the spawn/fork wrappers
            // (`extract_outcome_summary`): only a genuine `Completed` step
            // is a registry `Completed` — every stopped variant
            // (MaxIterationsReached / TimedOut / Truncated / Cancelled /
            // SchemaUnreachable) is a Failed run. Pre-R5 the stopped
            // variants were structurally unreachable here (script children
            // always ran uncapped defaults); a host-granted `loop_config`
            // makes them real, and a capped-out child reported `Completed`
            // would be a silent failure in registry ground truth (REVIEW
            // R5 HIGH-1).
            match &outcome {
                Ok(AgentStepResult::Completed { .. }) => {
                    let mut reg = agent_registry.write();
                    let transition = reg
                        .mark_completing(real_id)
                        .and_then(|()| reg.mark_completed(real_id));
                    if let Err(e) = transition {
                        crate::tools::agent::reclaim::log_terminal_transition_violation(
                            &reg,
                            real_id,
                            "rhai spawn_agent",
                            &e,
                        );
                    }
                }
                Ok(stopped) => {
                    let mut reg = agent_registry.write();
                    if let Err(e) = reg.mark_failed(real_id) {
                        tracing::error!(
                            child_id = %real_id,
                            stopped = ?std::mem::discriminant(stopped),
                            "rhai spawn_agent: child stopped without completing and \
                             its terminal mark was stolen",
                        );
                        crate::tools::agent::reclaim::log_terminal_transition_violation(
                            &reg,
                            real_id,
                            "rhai spawn_agent",
                            &e,
                        );
                    }
                }
                Err(run_error) => {
                    let mut reg = agent_registry.write();
                    if let Err(e) = reg.mark_failed(real_id) {
                        tracing::error!(child_id = %real_id, run_error = %run_error,
                            "rhai spawn_agent: child run failed and its terminal mark was stolen");
                        crate::tools::agent::reclaim::log_terminal_transition_violation(
                            &reg,
                            real_id,
                            "rhai spawn_agent",
                            &e,
                        );
                    }
                }
            }

            outcome
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
    /// rides the inherit-with-decrement derivation into the child's
    /// grant (registry ground truth) and actually binds on the script
    /// child's run — a granted `max_iterations = 1` stops the child
    /// after exactly one provider call where the scripted conversation
    /// would otherwise take two.
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_agent_grants_and_enforces_host_loop_config() {
        use crate::agent::child_policy::ChildLoopConfig;
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::usage::Usage;

        // Two scripted turns: a tool call (which would force a second
        // provider round-trip) then text. The granted cap of 1 means the
        // second turn must never be requested.
        let provider = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc1".to_owned(),
                    name: Some("nonexistent".to_owned()),
                    arguments_delta: "{}".to_owned(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                ProviderEvent::Done {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                    response_id: None,
                },
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "never reached".to_owned(),
                },
                ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                },
            ],
        ]));
        let mut ctx = build_context_with_provider(Arc::<MockProvider>::clone(&provider));
        let granted = ChildLoopConfig {
            max_iterations: Some(1),
            step_timeout_secs: None,
            linger_secs: None,
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
        assert_eq!(
            provider.call_count(),
            1,
            "the granted max_iterations = 1 must stop the child after one provider call",
        );
        // REVIEW R5 HIGH-1: a capped-out script child is a FAILED run in
        // registry ground truth — `MaxIterationsReached` must never be
        // recorded as `Completed` (the agents tool, status surfaces, and
        // close decisions all read this status).
        assert_eq!(
            registry
                .read()
                .get(child_id)
                .expect("terminal entry observable")
                .status,
            crate::agent::registry::AgentStatus::Failed,
            "a child stopped by its iteration cap completed nothing",
        );
    }
}
