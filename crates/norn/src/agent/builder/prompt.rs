//! Effective prompt and compaction assembly for [`AgentBuilder`](super::AgentBuilder).

use crate::agent::prompt_install::{SystemPromptInstall, install_system_prompt};
use crate::agent_loop::config::AgentLoopConfig;
use crate::error::NornError;
use crate::provider::ProviderCapabilities;
use crate::system_prompt::PromptSource;
use crate::system_prompt::builder::ExecutionMode;
use crate::tool::registry::ToolRegistry;

/// Builder-owned prompt inputs consumed by the prompt assembly phase.
pub(super) struct PromptInstallParts {
    pub(super) execution_mode: ExecutionMode,
    pub(super) system_prompt: Option<String>,
    pub(super) append_system_prompt: Option<String>,
    pub(super) profile_source: PromptSource,
    pub(super) capabilities: ProviderCapabilities,
}

/// Arm the effective context window and install the provider-bound prompt.
pub(super) fn install_effective_prompt(
    loop_context: &mut crate::agent_loop::loop_context::LoopContext,
    config: &mut AgentLoopConfig,
    model: &str,
    registry: &ToolRegistry,
    parts: PromptInstallParts,
) -> Result<Option<u64>, NornError> {
    // The same effective config drives compaction, prompt guidance, and
    // tool-output budgeting. Catalog filling never replaces an explicit
    // context window, and unsupported windows fail before execution.
    crate::agent::arming::arm_auto_compaction(loop_context, config, model);
    crate::agent::arming::validate_context_window(config, model).map_err(NornError::Config)?;

    // The runtime disables compaction when the reserve reaches the window,
    // so the prompt must not promise compaction for that shape.
    let has_auto_compact = match (
        config.context_window_limit,
        config.auto_compact_reserve_tokens,
    ) {
        (Some(limit), Some(reserve)) if reserve >= limit => {
            tracing::warn!(
                reserve_tokens = reserve,
                context_window_limit = limit,
                "auto_compact_reserve_tokens is at or above context_window; \
                     the runtime trigger disables in this configuration, so the \
                     system prompt will not claim auto-compaction is active",
            );
            false
        }
        (Some(_), Some(_)) => true,
        _ => false,
    };

    // Hosted-replaced tools are described as provider-native. Rebuilding
    // an existing session recomputes this surface for the rebound provider.
    install_system_prompt(
        loop_context,
        SystemPromptInstall {
            registry,
            mode: parts.execution_mode,
            has_output_schema: config.output_schema.is_some(),
            system_prompt_override: parts.system_prompt,
            append_system_prompt: parts.append_system_prompt,
            profile_source: parts.profile_source,
            has_auto_compact,
            capabilities: parts.capabilities,
        },
    );

    Ok(config.context_window_limit)
}
