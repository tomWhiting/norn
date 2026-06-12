//! Handle-returning agent operations: `spawn_agent` and `send_message`.
//! Each function bridges Rhai's sync callsite into the
//! Tokio runtime via stored `runtime` handles — `spawn_agent` keeps the
//! loop running in the background.

use std::sync::Arc;

use chrono::Utc;
use rhai::{Dynamic, Engine, EvalAltResult, ImmutableString, Map};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::context::{AgentHandle, NornRhaiContext, dynamic_to_json, rhai_error};
use crate::agent::registry::AgentRegistry;
use crate::error::NornError;
use crate::r#loop::LoopContext;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::r#loop::runner::{
    AgentLoopConfig, AgentStepRequest, AgentStepResult, ToolExecutor, run_agent_step,
};
use crate::provider::agent_event::AgentMessageLifecycle;
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tools::agent::append_message_audit;
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

    // send_message (AgentHandle recipient)
    {
        let ctx = context.clone();
        engine.register_fn(
            "send_message",
            move |to: AgentHandle, content: Dynamic| -> Result<u64, Box<EvalAltResult>> {
                send_message(&ctx, to.0, &content)
            },
        );
    }

    // send_message (String recipient — resolves path or UUID)
    {
        let ctx = context.clone();
        engine.register_fn(
            "send_message",
            move |to: ImmutableString, content: Dynamic| -> Result<u64, Box<EvalAltResult>> {
                let id = if let Ok(parsed) = Uuid::parse_str(to.as_str()) {
                    parsed
                } else {
                    let reg = ctx.registry.read();
                    reg.get_by_path(to.as_str()).map(|e| e.id).ok_or_else(|| {
                        Box::new(rhai_error(format!(
                            "send_message: unknown recipient '{to}'"
                        )))
                    })?
                };
                send_message(&ctx, id, &content)
            },
        );
    }
}

/// Deliver `content` to `to_id` through the shared
/// [`MessageRouter`](crate::agent::message_router::MessageRouter),
/// returning the router-minted per-recipient sequence number.
///
/// Rhai is a synchronous embedding, so delivery uses the router's
/// non-blocking path: a recipient with no live inbound route, a closed
/// channel, or a full buffer is a typed script error — never a silent
/// queue into storage nothing drains (the failure mode the deleted
/// `Mailbox` had). Attribution comes from registry ground truth; a
/// scripting context whose `agent_id` is not registered attributes as
/// `root` (script hosts are root-level orchestrators). Every accepted
/// send appends an `agent_message.sent` audit event to the host's event
/// store.
///
/// Script sends always carry [`MessageKind::Update`] (FYI batching;
/// they never interrupt the recipient mid-stream and never wake a
/// lingering recipient), preserving the pre-router script semantics.
/// A script-side kind parameter, the tombstone attribution fallback,
/// and registry terminal-state checks arrive with the `send_message`
/// tool in W3.2 — until then a send to a terminal-but-not-yet-dropped
/// recipient is detectable in the audit trail as `Sent` without a
/// paired `Delivered`.
fn send_message(
    ctx: &NornRhaiContext,
    to_id: Uuid,
    content: &Dynamic,
) -> Result<u64, Box<EvalAltResult>> {
    let json = dynamic_to_json(content)?;
    let body = match json {
        serde_json::Value::String(s) => s,
        other => serde_json::to_string(&other).map_err(|e| {
            Box::new(rhai_error(format!(
                "send_message: could not serialize content: {e}"
            )))
        })?,
    };
    let (from_label, from_role, to_label) = {
        let reg = ctx.registry.read();
        let (label, role) = reg.get(ctx.agent_id).map_or_else(
            || ("root".to_owned(), None),
            |entry| (entry.path, Some(entry.role)),
        );
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
        kind: MessageKind::Update,
        seq: None,
        timestamp: sent_at,
    };
    let seq = ctx
        .router
        .try_deliver(to_id, msg)
        .map_err(|e| Box::new(rhai_error(format!("send_message: {e}"))))?;
    append_message_audit(
        &ctx.event_store,
        &AgentMessageLifecycle::Sent {
            message_id,
            from_id: ctx.agent_id,
            from: from_label,
            to_id,
            to: to_label,
            kind: MessageKind::Update,
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
    let path =
        map_get_string(config, "path").unwrap_or_else(|| format!("/spawn/{}", Uuid::new_v4()));
    let tools = map_get_string_vec(config, "tools");

    let registry = ctx.tool_registry.as_ref().ok_or_else(|| {
        Box::new(rhai_error(
            "spawn_agent: NornRhaiContext.tool_registry is None; orchestrator must \
             supply a ToolRegistry so the sub-agent has tools available",
        ))
    })?;

    let guard =
        AgentRegistry::reserve(&ctx.registry, path, role, model.clone(), Some(ctx.agent_id))
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
            let child_context = registry_for_executor
                .shared_context()
                .unwrap_or_else(|| Arc::new(ToolContext::empty()));
            let executor = SubAgentExecutor::new(registry_for_executor, tools, child_context);
            let child_store = EventStore::new();
            let mut loop_ctx = LoopContext::new("You are a sub-agent. Complete the task and stop.");
            let outcome = run_agent_step(AgentStepRequest {
                provider: provider.as_ref(),
                executor: &executor,
                store: &child_store,
                user_prompt: &task_for_async,
                tools: &[],
                output_schema: None,
                model: &model_for_task,
                config: &AgentLoopConfig::default(),
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
            match &outcome {
                Ok(_) => {
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
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn send_message_delivers_via_router() {
        let ctx = build_context();
        let router = ctx.router.clone();
        let target_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        router.register(target_id, tx);

        let engine = build_norn_engine(&ctx);
        let script = format!(
            r#"send_message("{}", #{{ kind: "hello", text: "hi" }})"#,
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
    async fn send_message_to_unrouted_recipient_is_a_script_error() {
        let ctx = build_context();
        let target_id = Uuid::new_v4();

        let engine = build_norn_engine(&ctx);
        let script = format!(r#"send_message("{target_id}", "hello")"#);
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
}
