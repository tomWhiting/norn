//! Fork loop-runtime assembly after durable child setup has completed.

use std::sync::Arc;

use uuid::Uuid;

use super::args::ForkArgs;
use super::prompt;
use crate::agent::child_policy::ChildPolicy;
use crate::agent::fork::{ForkIdentity, ParentPromptPlan};
use crate::agent::result_channel::ChildAgentResult;
use crate::error::ToolError;
use crate::integration::hooks::HookRegistry;
use crate::r#loop::loop_context::LoopContext;
use crate::session::action_log::ActionLog;
use crate::tool::context::ToolContext;
use crate::tool::registry::ToolRegistry;
use crate::tools::agent::delegation::effective_child_tools;
use crate::tools::agent::infra::{AgentModel, AgentToolInfra};

/// Inputs needed to assemble one fork's loop context and visible tool policy.
pub(super) struct ForkRuntimeInputs<'a> {
    pub(super) parent_context: &'a ToolContext,
    pub(super) parent_infra: &'a AgentToolInfra,
    pub(super) child_context: &'a ToolContext,
    pub(super) fork_id: Uuid,
    pub(super) path_address: &'a str,
    pub(super) fork_policy: &'a ChildPolicy,
    pub(super) child_result_rx: Option<tokio::sync::mpsc::Receiver<ChildAgentResult>>,
    pub(super) args: &'a ForkArgs,
    pub(super) parent_registry: &'a ToolRegistry,
}

/// Assembled runtime values consumed by the launch boundary.
pub(super) struct ForkRuntime {
    pub(super) loop_context: LoopContext,
    pub(super) requirement_names: Vec<String>,
    pub(super) user_task: String,
    pub(super) allow_list: Option<Vec<String>>,
    pub(super) hooks: Option<Arc<HookRegistry>>,
}

/// Assemble the fork's source-aware prompt, loop context, and tool policy.
pub(super) fn assemble(inputs: ForkRuntimeInputs<'_>) -> Result<ForkRuntime, ToolError> {
    let requirement_names = inputs
        .args
        .requirements
        .iter()
        .map(|requirement| crate::agent::fork::slugify_requirement_name(&requirement.name))
        .collect();
    let parent_agent_id = inputs.parent_infra.agent_id.to_string();
    let prompt::ForkPrompt {
        mut parent_plan,
        active_plan,
        user_task,
    } = prompt::assemble(
        inputs.parent_context,
        &ForkIdentity {
            parent_agent_id: &parent_agent_id,
            path_address: inputs.path_address,
            granted: inputs.fork_policy,
        },
        &inputs.args.request,
        &inputs.args.requirements,
    );

    // The compatibility string remains available to adapters that cannot
    // consume the typed plan. Source-aware providers use the installed plan.
    let compatibility_base = active_plan.flattened_content();
    let mut loop_context = LoopContext::with_working_dir(
        compatibility_base,
        inputs.child_context.shared_working_dir(),
    );
    loop_context.install_stable_prompt_plan(active_plan);
    loop_context.agent_id = Some(inputs.fork_id);
    loop_context.pending_agent_messages = Some(Arc::clone(&inputs.parent_infra.pending_messages));

    // Forks inherit the parent's live reasoning effort, validated against the
    // fork model. Unsupported inherited pairings degrade to None in the shared
    // arming helper rather than failing the fork.
    loop_context.reasoning_effort = crate::agent::arming::arm_child_reasoning_effort(
        inputs
            .parent_context
            .get_extension::<AgentModel>()
            .and_then(|live| live.reasoning_effort),
        &crate::agent::arming::ChildEffortSource::Inherited { child: "fork" },
        &inputs.args.model,
    )
    .map_err(|error| ToolError::ExecutionFailed {
        reason: format!("fork: {error}"),
    })?;

    let hooks = inputs.parent_context.get_extension::<HookRegistry>();
    loop_context.hooks = hooks.as_ref().map(Arc::clone);
    loop_context.action_log = inputs.child_context.get_extension::<ActionLog>();
    loop_context.child_result_rx = inputs.child_result_rx;
    loop_context.environment = Some(crate::system_prompt::environment::EnvironmentConfig {
        session_id: None,
        model: inputs.args.model.clone(),
    });

    // A leaf never sees delegation tools, and a no-messaging child never sees
    // signal_agent. The definitions and executor receive the same allow-list.
    let allow_list =
        effective_child_tools(inputs.parent_registry, None, inputs.fork_policy, "fork");

    if let Some(catalog) = inputs
        .parent_context
        .get_extension::<crate::skill::SkillCatalog>()
    {
        crate::agent::arming::install_child_skill_listing(
            &mut loop_context,
            &catalog,
            crate::agent::arming::child_skill_tool_available(
                inputs.parent_registry,
                allow_list.as_deref(),
            ),
        );
    }

    // Descendants inherit the source-aware plan without this fork's identity.
    if let Some(installed) = loop_context.stable_prompt_plan() {
        parent_plan.clone_from(installed);
        parent_plan.remove(crate::system_prompt::PromptSource::ForkAgentPolicy);
    }
    inputs.child_context.insert_extension(Arc::new(AgentModel {
        model: inputs.args.model.clone(),
        reasoning_effort: loop_context.reasoning_effort,
    }));
    inputs
        .child_context
        .insert_extension(Arc::new(ParentPromptPlan::new(parent_plan)));

    Ok(ForkRuntime {
        loop_context,
        requirement_names,
        user_task,
        allow_list,
        hooks,
    })
}
