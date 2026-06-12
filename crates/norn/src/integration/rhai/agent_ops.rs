//! Handle-returning agent operations: `spawn_agent` and `send_message`.
//! Each function bridges Rhai's sync callsite into the
//! Tokio runtime via stored `runtime` handles — `spawn_agent` keeps the
//! loop running in the background.

use std::sync::Arc;

use rhai::{Dynamic, Engine, EvalAltResult, ImmutableString, Map};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::context::{AgentHandle, NornRhaiContext, dynamic_to_json, rhai_error};
use crate::agent::registry::AgentRegistry;
use crate::error::NornError;
use crate::r#loop::LoopContext;
use crate::r#loop::runner::{
    AgentLoopConfig, AgentStepRequest, AgentStepResult, ToolExecutor, run_agent_step,
};
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
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
            move |to: AgentHandle, content: Dynamic| -> Result<(), Box<EvalAltResult>> {
                let json = dynamic_to_json(&content)?;
                ctx.mailbox.send(ctx.agent_id, to.0, json, false);
                Ok(())
            },
        );
    }

    // send_message (String recipient — resolves path or UUID)
    {
        let ctx = context.clone();
        engine.register_fn(
            "send_message",
            move |to: ImmutableString, content: Dynamic| -> Result<(), Box<EvalAltResult>> {
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
                let json = dynamic_to_json(&content)?;
                ctx.mailbox.send(ctx.agent_id, id, json, false);
                Ok(())
            },
        );
    }
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
    use crate::agent::mailbox::Mailbox;
    use crate::agent::registry::AgentRegistry;
    use crate::provider::mock::MockProvider;
    use crate::provider::traits::Provider;
    use crate::session::store::EventStore;
    use crate::tool::registry::ToolRegistry;

    fn build_context() -> NornRhaiContext {
        build_context_with_provider(Arc::new(MockProvider::new(Vec::new())))
    }

    fn build_context_with_provider(provider: Arc<dyn Provider>) -> NornRhaiContext {
        let registry = AgentRegistry::shared();
        let mailbox = Arc::new(Mailbox::new());
        let agent_id = Uuid::new_v4();
        NornRhaiContext {
            registry,
            mailbox,
            provider,
            agent_id,
            runtime: tokio::runtime::Handle::current(),
            event_store: Arc::new(EventStore::new()),
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            working_dir: crate::tool::context::SharedWorkingDir::default(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn send_message_delivers_via_mailbox() {
        let ctx = build_context();
        let mailbox = ctx.mailbox.clone();
        let target_id = Uuid::new_v4();

        let engine = build_norn_engine(&ctx);
        let script = format!(
            r#"send_message("{}", #{{ kind: "hello", text: "hi" }})"#,
            target_id
        );
        engine.eval::<()>(&script).unwrap();

        let msgs = mailbox.recv(target_id);
        assert_eq!(msgs.len(), 1, "exactly one message queued");
        assert_eq!(msgs[0].content["kind"], "hello");
        assert_eq!(msgs[0].content["text"], "hi");
    }
}
