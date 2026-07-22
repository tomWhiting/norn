//! Rhai `spawn_agent` registration and launch pipeline.

use std::sync::Arc;

use chrono::Utc;
use rhai::{Engine, EvalAltResult, ImmutableString, Map};
use tokio::task::JoinHandle;

use super::super::context::{AgentHandle, NornRhaiContext, rhai_error};
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::r#loop::LoopContext;
use crate::r#loop::runner::{AgentStepRequest, ToolExecutor, run_agent_step};
use crate::provider::AgentEventSender;
use crate::provider::agent_event::{SubagentDescriptor, SubagentKind};
use crate::provider::usage::Usage;
use crate::session::events::ChildBranchKind;
use crate::session::{ChildBranchRequest, slugify_name_stem};
use crate::tool::context::ToolContext;
use crate::tools::agent::infra::SubAgentExecutor;
use crate::tools::agent::lifecycle::{LifecycleEmitter, SubagentCompletion};
use crate::tools::agent::spawn_outcome::{extract_outcome_summary, mark_terminal_in_registry};

pub(super) fn register(engine: &mut Engine, context: &NornRhaiContext) {
    let ctx = context.clone();
    engine.register_fn(
        "spawn_agent",
        move |config: Map| -> Result<AgentHandle, Box<EvalAltResult>> {
            spawn_agent(&ctx, &config)
        },
    );
}

fn spawn_agent(ctx: &NornRhaiContext, config: &Map) -> Result<AgentHandle, Box<EvalAltResult>> {
    use crate::tools::agent::variant_resolve::{
        lookup_variant, resolve_child_model, resolve_parent_model,
    };

    let task = map_get_string(config, "task")
        .ok_or_else(|| Box::new(rhai_error("spawn_agent: missing 'task'")))?;
    let host_shared = ctx
        .tool_registry
        .as_ref()
        .and_then(|registry| registry.shared_context());
    let catalog = host_shared
        .as_ref()
        .and_then(|shared| shared.get_extension::<crate::agent::variants::VariantCatalog>());
    let variant = match map_get_string(config, "variant") {
        Some(name) => Some(
            lookup_variant(catalog.as_deref(), &name, "spawn_agent")
                .map_err(|e| Box::new(rhai_error(e.to_string())))?
                .clone(),
        ),
        None => None,
    };

    let model = match (map_get_string(config, "model"), variant.as_ref()) {
        (explicit, Some(variant)) => {
            resolve_child_model(explicit, Some(variant), "spawn_agent", || {
                resolve_parent_model(
                    &ctx.registry,
                    ctx.agent_id,
                    host_shared.as_deref(),
                    "spawn_agent",
                )
            })
            .map_err(|e| Box::new(rhai_error(e.to_string())))?
        }
        (Some(explicit), None) => explicit,
        (None, None) => return Err(Box::new(rhai_error("spawn_agent: missing 'model'"))),
    };
    let role = map_get_string(config, "role")
        .or_else(|| variant.as_ref().map(|variant| variant.name.clone()))
        .unwrap_or_else(|| "subagent".to_owned());
    let child_effort = crate::tools::agent::variant_resolve::resolve_child_reasoning_effort(
        &crate::tools::agent::variant_resolve::ChildEffortInputs {
            variant_effort: variant.as_ref().and_then(|value| value.reasoning_effort),
            variant_name: variant.as_ref().map(|value| value.name.as_str()),
            profile_effort: None,
            profile_name: None,
            parent_live_effort: host_shared
                .as_ref()
                .and_then(|shared| shared.get_extension::<crate::tools::agent::AgentModel>())
                .and_then(|live| live.reasoning_effort),
            child_model: &model,
            child_role: &role,
            surface: "spawn_agent",
        },
    )
    .map_err(|e| Box::new(rhai_error(e.to_string())))?;
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
    let descriptor = SubagentDescriptor {
        kind: SubagentKind::Spawn,
        role: role.clone(),
        model: model.clone(),
        profile: variant.as_ref().map(|value| value.name.clone()),
    };
    let name_stem = slugify_name_stem(&role, "spawn");

    let child_grant = ctx
        .child_policy
        .grant_for_child(None)
        .map_err(|e| Box::new(rhai_error(format!("spawn_agent: {e}"))))?;
    let mut child_config =
        crate::agent::child_policy::ChildLoopConfig::resolve(child_grant.loop_config);
    crate::agent::arming::arm_child_window(&mut child_config, &model)
        .map_err(|e| Box::new(rhai_error(format!("spawn_agent: {e}"))))?;
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
    guard
        .confirm()
        .map_err(|e| Box::new(rhai_error(format!("spawn_agent: confirm: {e}"))))?;

    let provider = Arc::clone(&ctx.provider);
    let registry_for_executor = Arc::clone(registry);
    let agent_registry = Arc::clone(&ctx.registry);
    let variant_prompt = variant
        .as_ref()
        .and_then(|value| value.prompt.clone().zip(value.prompt_origin));
    let prompt_fragment = variant_prompt
        .as_ref()
        .map(|(prompt, origin)| match origin {
            crate::agent::variants::VariantPromptOrigin::Builtin => {
                crate::system_prompt::child::ChildPromptFragment::BuiltinVariant(prompt)
            }
            crate::agent::variants::VariantPromptOrigin::Configured => {
                crate::system_prompt::child::ChildPromptFragment::ConfiguredVariant(prompt)
            }
        });
    let child_prompt_plan = crate::system_prompt::child::build_child_prompt_plan(prompt_fragment);

    let child_tool_ctx = {
        let mut child_ctx = ToolContext::with_working_dir(
            crate::tool::context::SharedWorkingDir::new(ctx.working_dir.get()),
        );
        if let Some(host) = host_shared.as_deref() {
            if let Some(root) = host.workspace_root() {
                child_ctx.confine_to_workspace(root.to_path_buf());
                let exempt = host.read_exempt_roots().to_vec();
                if !exempt.is_empty() {
                    child_ctx.set_read_exempt_roots(exempt);
                }
            }
            crate::tools::agent::spawn_context::forward_shared_extensions(host, &mut child_ctx);
        }
        child_ctx.insert_extension(Arc::new(crate::tools::agent::AgentModel {
            model: model.clone(),
            reasoning_effort: child_effort,
        }));
        child_ctx.insert_extension(Arc::new(crate::agent::fork::ParentPromptPlan::new(
            child_prompt_plan.clone(),
        )));
        Arc::new(child_ctx)
    };
    let model_for_task = model;

    let _join_handle: JoinHandle<()> = ctx.runtime.spawn(async move {
        let executor = SubAgentExecutor::new(registry_for_executor, tools, child_tool_ctx);
        let mut loop_ctx = LoopContext::new("");
        loop_ctx.install_stable_prompt_plan(child_prompt_plan);
        if let Some(effort) = child_effort {
            loop_ctx.reasoning_effort = Some(effort);
        }
        crate::agent::arming::arm_auto_compaction(
            &mut loop_ctx,
            &mut child_config,
            &model_for_task,
        );
        let outcome = run_agent_step(AgentStepRequest {
            provider: provider.as_ref(),
            executor: &executor,
            store: child_store.as_ref(),
            user_prompt: &task,
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

        let summary = extract_outcome_summary(outcome, Usage::default());
        let subtree_usage = summary.usage.clone() + summary.children_usage.clone();
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
    map.get(key).and_then(|value| {
        if let Some(string) = value.clone().try_cast::<String>() {
            Some(string)
        } else {
            value
                .clone()
                .try_cast::<ImmutableString>()
                .map(|string| string.to_string())
        }
    })
}

fn map_get_string_vec(map: &Map, key: &str) -> Option<Vec<String>> {
    map.get(key).and_then(|value| {
        value.clone().try_cast::<rhai::Array>().map(|entries| {
            entries
                .into_iter()
                .filter_map(|entry| {
                    if let Some(string) = entry.clone().try_cast::<String>() {
                        Some(string)
                    } else {
                        entry
                            .try_cast::<ImmutableString>()
                            .map(|string| string.to_string())
                    }
                })
                .collect()
        })
    })
}
