//! Tool-registry construction for
//! [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build).
//!
//! Split out of `agent/assembly.rs` to keep it within the production-size
//! limit. Holds the ungated base tool-registry assembly (standard tools plus
//! the `cron` scheduling tool, with the bash drain-grace override applied)
//! and the skill-tool configuration resolution the `load_runtime_base` path
//! uses.

use std::sync::Arc;
use std::time::Duration;

use crate::tool::registry::ToolRegistry;
use crate::tool::traits::Tool;
use crate::tools::bash::BashTool;
use crate::tools::lsp::LspBackend;
use crate::tools::registry_builder::register_standard_tools;

/// Resolve the [`SkillToolConfig`](crate::tools::skill::SkillToolConfig)
/// from the merged settings' `tools.skill` section (D5).
///
/// An absent section — or an absent `shell_execution` key — defers to the
/// tool's own documented default (shell execution **enabled**); `false`
/// disables skill-authored shell expansion. This is the library-side
/// mirror of the CLI's `skill_tool_config_from_settings`, so the embedded
/// `load_runtime_base` path and the CLI resolve the skill tool identically.
pub(crate) fn skill_tool_config_from_settings(
    settings: &crate::config::NornSettings,
) -> crate::tools::skill::SkillToolConfig {
    let shell_execution = settings
        .tools
        .as_ref()
        .and_then(|tools| tools.skill.as_ref())
        .and_then(|skill| skill.shell_execution);
    match shell_execution {
        Some(shell_execution) => crate::tools::skill::SkillToolConfig { shell_execution },
        None => crate::tools::skill::SkillToolConfig::default(),
    }
}

/// Build the ungated tool registry: the standard set (with the bash drain
/// grace applied when overridden), plus the `cron` scheduling tool — this
/// is the `AgentBuilder` path, which always arms the schedule executor the
/// tool resolves, so registries assembled by a bare
/// `register_standard_tools` call carry no `cron` tool — plus the caller's
/// extra tools, minus the excluded names.
pub(crate) fn build_base_tool_registry(
    lsp_backend: Option<Arc<dyn LspBackend>>,
    extra_tools: Vec<Box<dyn Tool + Send + Sync>>,
    without_tools: &[String],
    bash_drain_grace: Option<Duration>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    register_standard_tools(&mut registry, lsp_backend);
    crate::tools::registry_builder::register_cron_tool(&mut registry);
    crate::tools::registry_builder::register_process_tool(&mut registry);
    // Replace the standard bash tool with one carrying the overridden drain
    // grace. Caller-registered replacements (extra tools named `bash`) are
    // registered afterwards and win, matching registry semantics.
    if let Some(grace) = bash_drain_grace
        && registry.remove("bash").is_some()
    {
        registry.register(Box::new(BashTool::new().with_drain_grace(grace)));
    }
    for tool in extra_tools {
        registry.register(tool);
    }
    for name in without_tools {
        registry.remove(name);
    }
    registry
}
