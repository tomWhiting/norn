//! Spawn argument parsing and child loop-context construction.

use std::path::Path;

use serde::Deserialize;

use super::super::spawn_context::validate_model_selected_profile;
use crate::agent::child_policy::ChildPolicy;
use crate::agent::variants::VariantPromptOrigin;
use crate::error::ToolError;
use crate::r#loop::loop_context::LoopContext;
use crate::profile::from_profile;
use crate::profile::loader::{ProfileOrigin, resolve_workspace_profile_at_launch_root};
use crate::system_prompt::child::{ChildPromptFragment, build_child_prompt_plan};
use crate::tool::registry::ToolRegistry;

// deny_unknown_fields: a typo'd key (e.g. `child_polciy`) must fail
// loudly, not silently hand the child a default grant where the caller
// intended a narrowing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpawnAgentArgs {
    pub(super) task: String,
    /// Optional model id. Resolution: this argument, else the variant's
    /// model, else the parent's own model (unless the variant requires
    /// an explicit model — a typed error then).
    #[serde(default)]
    pub(super) model: Option<String>,
    /// Optional role label. Resolution: this argument, else the variant
    /// name; with neither the spawn is a typed error.
    #[serde(default)]
    pub(super) role: Option<String>,
    /// Optional named agent variant (built-in or configured
    /// `variants.<name>` settings). Mutually exclusive with `profile`.
    #[serde(default)]
    pub(super) variant: Option<String>,
    #[serde(default)]
    pub(super) profile: Option<String>,
    #[serde(default)]
    pub(super) tools: Option<Vec<String>>,
    /// Optional MCP server subset for this child. Omit to inherit; an empty
    /// list removes MCP tools while preserving the other selected tools.
    #[serde(default)]
    pub(super) mcp_servers: Option<Vec<String>>,
    #[serde(default)]
    pub(super) path: Option<String>,
    /// Optional JSON Schema the child's final output must validate
    /// against. Schema is an explicit per-spawn decision: a child never
    /// implicitly inherits the parent's output schema — omitting this
    /// field means the child produces free-form output.
    #[serde(default)]
    pub(super) output_schema: Option<serde_json::Value>,
    /// Optional per-spawn [`ChildPolicy`] narrowing (DECISION R2),
    /// mirroring the Rust type 1:1 at the JSON layer. Omitted → the
    /// child inherits the caller's own granted policy with the
    /// delegation depth decremented one level. Supplied → must be a
    /// strict narrowing of the caller's own grant; widening is a typed
    /// failure naming the caller's budget.
    #[serde(default)]
    pub(super) child_policy: Option<ChildPolicy>,
}

/// Build the child's [`LoopContext`] and the profile-derived tool list.
///
/// The child's stable prompt always starts with compiled child policy. A
/// variant prompt retains its field-level built-in/configured provenance; a
/// profile retains whether discovery resolved it from a trusted user path or
/// the workspace. The launch task is deliberately not accepted by this API:
/// it is supplied exactly once as the run's User prompt.
///
/// When `profile_name` is `Some`, the named profile is resolved through the
/// scanner over the parent working directory's standard profile tiers; its
/// reasoning config flows into the returned [`LoopContext`] via
/// [`from_profile`]. Model-selected profiles that declare prompt commands are
/// rejected before child construction because they cannot grant ambient
/// process authority.
/// The gated [`ToolRegistry`] `from_profile` produces is discarded — the
/// child shares the parent's registry — but the profile's resolved tool
/// list is returned so the caller can use it as the per-child
/// allow-list. With neither, a minimal context carries only compiled child
/// policy.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] when a named profile cannot be
/// resolved — spawn never silently falls back to a default profile.
pub(super) fn build_child_loop_context(
    variant_prompt: Option<(&str, VariantPromptOrigin)>,
    profile_name: Option<&str>,
    working_dir: &Path,
) -> Result<(LoopContext, Option<Vec<String>>), ToolError> {
    if let Some((prompt, origin)) = variant_prompt {
        let fragment = match origin {
            VariantPromptOrigin::Builtin => ChildPromptFragment::BuiltinVariant(prompt),
            VariantPromptOrigin::Configured => ChildPromptFragment::ConfiguredVariant(prompt),
        };
        return Ok((loop_context_with_plan(Some(fragment)), None));
    }
    if let Some(name) = profile_name {
        let resolved =
            resolve_workspace_profile_at_launch_root(name, working_dir).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("spawn_agent: selected profile could not be resolved: {e}"),
                }
            })?;
        let profile = resolved.profile;
        validate_model_selected_profile(&profile)?;
        let resolved_tools = profile.resolved_tools();
        let instructions = profile.resolved_instructions().join("\n\n");
        let fragment = match resolved.origin {
            ProfileOrigin::User => ChildPromptFragment::OperatorProfile(&instructions),
            ProfileOrigin::WorkingDirectory => ChildPromptFragment::WorkspaceProfile(&instructions),
        };
        let (mut loop_ctx, _gated) = from_profile(&profile, ToolRegistry::new(), None, None);
        loop_ctx.install_stable_prompt_plan(build_child_prompt_plan(Some(fragment)));
        Ok((loop_ctx, resolved_tools))
    } else {
        Ok((loop_context_with_plan(None), None))
    }
}

fn loop_context_with_plan(fragment: Option<ChildPromptFragment<'_>>) -> LoopContext {
    let mut context = LoopContext::new("");
    context.install_stable_prompt_plan(build_child_prompt_plan(fragment));
    context
}

/// Build the [`ToolDefinition`] slice the child model sees.
///
/// Delegates to the shared registry → function-definition projection in
/// [`crate::provider::surface`] — the same projection `AgentBuilder`
/// assembly uses — filtered through `allow_list` (the same list that gates
/// the child's [`crate::tools::agent::infra::SubAgentExecutor`]). When
/// `allow_list` is `None` every
/// available parent tool is included. The child's agent loop then resolves
/// these definitions against the live provider's capabilities per request,
/// exactly like the parent's loop, so hosted-tool replacement applies
/// identically to children.
#[cfg(test)]
pub(super) fn build_tool_definitions(
    registry: &ToolRegistry,
    allow_list: Option<&[String]>,
) -> Vec<crate::provider::request::ToolDefinition> {
    match allow_list {
        Some(allow_list) => {
            crate::provider::surface::collect_registered_function_definitions(registry, allow_list)
        }
        None => crate::provider::surface::collect_function_definitions(registry, None),
    }
}
