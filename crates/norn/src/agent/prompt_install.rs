//! System-prompt installation for
//! [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build).
//!
//! Splits the provider-aware system-prompt phase out of `agent/assembly.rs`
//! to keep each file within the production-size limit. The prompt's tools
//! section is resolved against the bound provider's capabilities
//! ([`reframe_prompt_entries`]), so a hosted-replaced tool (e.g.
//! `web_search` on a hosted-search provider) is described as
//! provider-native rather than as a phantom callable function.

use crate::r#loop::loop_context::LoopContext;
use crate::provider::surface::reframe_prompt_entries;
use crate::provider::tools::ProviderCapabilities;
use crate::system_prompt::builder::{
    ExecutionMode, SystemPromptInputs, ToolPromptEntry, build_system_prompt,
};
use crate::tool::registry::ToolRegistry;

/// Inputs for [`install_system_prompt`] beyond the loop context itself.
pub(crate) struct SystemPromptInstall<'a> {
    /// The gated tool registry whose tools the prompt lists.
    pub(crate) registry: &'a ToolRegistry,
    /// Interactive or headless execution.
    pub(crate) mode: ExecutionMode,
    /// Whether an output schema is configured for the final response.
    pub(crate) has_output_schema: bool,
    /// Caller-supplied replacement for the profile instructions.
    pub(crate) system_prompt_override: Option<String>,
    /// Caller-supplied fragment appended after the instructions.
    pub(crate) append_system_prompt: Option<String>,
    /// Whether auto-compaction is enabled on the effective config.
    pub(crate) has_auto_compact: bool,
    /// Capabilities of the provider this agent is being bound to. The
    /// prompt's tools section is reframed through the resolved tool
    /// surface so a hosted-replaced tool (e.g. `web_search` on a
    /// hosted-search provider) is described as provider-native, never as
    /// a phantom callable function. Recomputed on every build — including
    /// session resumes, which re-enter this assembly with the (possibly
    /// different) provider being bound.
    pub(crate) capabilities: ProviderCapabilities,
}

/// Build the Norn base system prompt from the gated registry and layer it
/// over the profile instructions (or the caller's `system_prompt` override)
/// into `loop_context.system_sections[0]`.
///
/// Runtime-dynamic tools are deliberately omitted from this cache-stable
/// prefix. The request-boundary tool lease publishes their generation-bound
/// guidance through the managed developer tail instead.
pub(crate) fn install_system_prompt(
    loop_context: &mut LoopContext,
    install: SystemPromptInstall<'_>,
) {
    let inputs = SystemPromptInputs {
        mode: install.mode,
        tools: reframe_prompt_entries(
            collect_tool_prompt_entries(install.registry),
            install.capabilities,
        ),
        has_output_schema: install.has_output_schema,
        event_schema_descriptions: Vec::new(),
        has_rules_engine: loop_context.rules.is_some(),
        has_auto_compact: install.has_auto_compact,
    };
    let base_prompt = build_system_prompt(&inputs);

    let profile_prefix = std::mem::take(&mut loop_context.system_sections);
    let mut instructions = install
        .system_prompt_override
        .unwrap_or_else(|| profile_prefix.into_iter().next().unwrap_or_default());
    if let Some(append) = install.append_system_prompt
        && !append.is_empty()
    {
        append_prompt(&mut instructions, &append);
    }

    loop_context.base_prefix = if instructions.is_empty() {
        base_prompt
    } else {
        format!("{base_prompt}\n\n{instructions}")
    };
    loop_context.rebuild_base_section();
}

fn append_prompt(prompt: &mut String, fragment: &str) {
    if prompt.is_empty() {
        *prompt = fragment.to_string();
    } else {
        prompt.push_str("\n\n");
        prompt.push_str(fragment);
    }
}

/// Tool metadata for the system prompt builder.
fn collect_tool_prompt_entries(registry: &ToolRegistry) -> Vec<ToolPromptEntry> {
    let names: Vec<String> = registry.names().map(str::to_owned).collect();
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        if let Some(tool) = registry.get(&name) {
            if tool.runtime_dynamic() {
                continue;
            }
            entries.push(ToolPromptEntry {
                name: tool.name().to_owned(),
                category: tool.category(),
                description: tool.description().to_owned(),
                usage_guidance: tool.usage_guidance().map(str::to_owned),
            });
        }
    }
    entries
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unnecessary_literal_bound
)]
mod tests {
    use async_trait::async_trait;

    use super::{ExecutionMode, SystemPromptInputs, ToolRegistry, collect_tool_prompt_entries};
    use crate::error::ToolError;
    use crate::system_prompt::builder::build_system_prompt;
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::scheduling::ToolEffect;
    use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

    struct StubTool {
        tool_name: String,
    }

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            &self.tool_name
        }

        fn description(&self) -> &str {
            "stub tool"
        }

        fn category(&self) -> ToolCategory {
            ToolCategory::FileSystem
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }

        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::success(serde_json::json!(null)))
        }
    }

    fn registry_with(order: &[&str]) -> ToolRegistry {
        let mut reg = ToolRegistry::new();
        for name in order {
            reg.register(Box::new(StubTool {
                tool_name: (*name).to_string(),
            }));
        }
        reg
    }

    fn render(reg: &ToolRegistry) -> String {
        build_system_prompt(&SystemPromptInputs {
            mode: ExecutionMode::Interactive,
            tools: collect_tool_prompt_entries(reg),
            has_output_schema: false,
            event_schema_descriptions: Vec::new(),
            has_rules_engine: false,
            has_auto_compact: false,
        })
    }

    /// The `# Tools` section — and the entries feeding it — must be
    /// byte-for-byte identical across two independently-built registries,
    /// regardless of each `HashMap`'s per-instance iteration order. Twelve
    /// tools are registered (>10, enough to make a HashMap-order regression
    /// flaky) in two different insertion orders; a stable render proves the
    /// listing no longer rides on nondeterministic map order, which would
    /// otherwise break provider prompt caching.
    #[test]
    fn tool_prompt_listing_is_deterministically_ordered() {
        let forward = [
            "read",
            "write",
            "edit",
            "bash",
            "grep",
            "glob",
            "web_fetch",
            "web_search",
            "task",
            "todo",
            "lsp",
            "diagnostics",
        ];
        let mut reverse = forward;
        reverse.reverse();

        let reg_a = registry_with(&forward);
        let reg_b = registry_with(&reverse);

        let names_a: Vec<String> = collect_tool_prompt_entries(&reg_a)
            .into_iter()
            .map(|e| e.name)
            .collect();
        let names_b: Vec<String> = collect_tool_prompt_entries(&reg_b)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(
            names_a, names_b,
            "entry order must not depend on insertion or HashMap order",
        );

        let mut sorted = names_a.clone();
        sorted.sort();
        assert_eq!(
            names_a, sorted,
            "prompt entries must be lexicographically ordered",
        );

        assert_eq!(
            render(&reg_a),
            render(&reg_b),
            "rendered system prompt must be byte-identical across runs",
        );
    }
}
