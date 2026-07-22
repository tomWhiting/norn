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
use crate::system_prompt::PromptSource;
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
    /// Provenance of the resolved profile instructions.
    pub(crate) profile_source: PromptSource,
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

/// Build the Norn base system prompt from the gated registry and install the
/// source-aware stable prompt plan.
///
/// `system_sections[0]` remains a flattened compatibility view for existing
/// introspection and child-inheritance surfaces; provider request assembly
/// uses the typed plan so those fragments are not promoted to System.
///
/// Runtime-dynamic tools are deliberately omitted from this cache-stable
/// prefix. Their generation-bound descriptions remain solely in the live tool
/// definitions selected by the request-boundary lease; duplicating them into
/// prompt prose would create a second, differently trusted authority channel.
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

    let profile_instructions = std::mem::take(&mut loop_context.system_sections)
        .into_iter()
        .next()
        .unwrap_or_default();
    let mut plan = loop_context.stable_prompt_plan.take().unwrap_or_default();
    plan.set(PromptSource::ProductPolicy, base_prompt.clone());

    let mut compatibility_prefix = base_prompt;
    if let Some(mut replacement) = install.system_prompt_override {
        if let Some(append) = install.append_system_prompt
            && !append.is_empty()
        {
            append_prompt(&mut replacement, &append);
        }
        plan.set(PromptSource::OperatorOverride, replacement.clone());
        append_prompt(&mut compatibility_prefix, &replacement);
    } else {
        plan.set(install.profile_source, profile_instructions.clone());
        if !profile_instructions.is_empty() {
            append_prompt(&mut compatibility_prefix, &profile_instructions);
        }
        if let Some(append) = install.append_system_prompt
            && !append.is_empty()
        {
            plan.set(PromptSource::OperatorOverride, append.clone());
            append_prompt(&mut compatibility_prefix, &append);
        }
    }
    loop_context.base_prefix = compatibility_prefix;
    loop_context.install_stable_prompt_plan(plan);
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
    use std::fs;

    use async_trait::async_trait;

    use super::{
        ExecutionMode, SystemPromptInputs, SystemPromptInstall, ToolRegistry,
        collect_tool_prompt_entries, install_system_prompt,
    };
    use crate::context::{ContextFile, ContextLoader};
    use crate::error::ToolError;
    use crate::r#loop::loop_context::LoopContext;
    use crate::provider::tools::ProviderCapabilities;
    use crate::skill::SkillCatalog;
    use crate::system_prompt::builder::build_system_prompt;
    use crate::system_prompt::{PromptAuthority, PromptFragment, PromptSource};
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

    #[test]
    fn install_preserves_root_fragment_sources_and_authorities() {
        let root = tempfile::tempdir().expect("temporary skill root");
        let operator_skills = root.path().join("operator-skills");
        let workspace = root.path().join("workspace");
        let workspace_skills = workspace.join(".norn/skills");
        fs::create_dir_all(operator_skills.join("trusted")).expect("operator skill directory");
        fs::create_dir_all(workspace_skills.join("repository")).expect("workspace skill directory");
        fs::write(
            operator_skills.join("trusted/SKILL.md"),
            "---\ndescription: OPERATOR_SKILL_SENTINEL\n---\nbody\n",
        )
        .expect("operator skill");
        fs::write(
            workspace_skills.join("repository/SKILL.md"),
            "---\ndescription: WORKSPACE_SKILL_SENTINEL\n---\nbody\n",
        )
        .expect("workspace skill");
        let workspace_root = workspace.canonicalize().expect("canonical workspace root");
        let catalog = SkillCatalog::scan_with_workspace(
            &[workspace_skills, operator_skills],
            &workspace_root,
        );
        let registry = ToolRegistry::new();
        let mut context = LoopContext::new("workspace profile");
        context.context_loader = Some(ContextLoader {
            user: Some(ContextFile {
                path: "/user/NORN.md".into(),
                content: "user context".to_owned(),
                mtime: None,
            }),
            project: Some(ContextFile {
                path: "/repo/NORN.md".into(),
                content: "project context".to_owned(),
                mtime: None,
            }),
            cwd: "/repo".into(),
        });
        crate::agent::arming::apply_skill_listing(&mut context, &catalog, true);

        install_system_prompt(
            &mut context,
            SystemPromptInstall {
                registry: &registry,
                mode: ExecutionMode::Headless,
                has_output_schema: false,
                system_prompt_override: None,
                append_system_prompt: None,
                profile_source: PromptSource::WorkspaceProfile,
                has_auto_compact: false,
                capabilities: ProviderCapabilities::openai_responses(),
            },
        );

        let plan = context
            .stable_prompt_plan()
            .expect("root prompt installation must publish a typed plan");
        let sources = plan
            .fragments()
            .iter()
            .map(PromptFragment::source)
            .collect::<Vec<_>>();
        assert_eq!(
            sources,
            [
                PromptSource::ProductPolicy,
                PromptSource::SkillCatalogPolicy,
                PromptSource::UserContextFile,
                PromptSource::OperatorSkillCatalog,
                PromptSource::WorkspaceProfile,
                PromptSource::ProjectContextFile,
                PromptSource::WorkspaceSkillCatalog,
            ]
        );
        let authorities = plan
            .fragments()
            .iter()
            .map(PromptFragment::authority)
            .collect::<Vec<_>>();
        assert_eq!(
            authorities,
            [
                PromptAuthority::System,
                PromptAuthority::System,
                PromptAuthority::Developer,
                PromptAuthority::Developer,
                PromptAuthority::User,
                PromptAuthority::User,
                PromptAuthority::User,
            ]
        );
        let developer_text = plan
            .fragments()
            .iter()
            .filter(|fragment| fragment.authority() == PromptAuthority::Developer)
            .map(PromptFragment::content)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(developer_text.contains("OPERATOR_SKILL_SENTINEL"));
        assert!(!developer_text.contains("WORKSPACE_SKILL_SENTINEL"));
        let workspace_fragment = plan
            .fragments()
            .iter()
            .find(|fragment| fragment.source() == PromptSource::WorkspaceSkillCatalog)
            .expect("workspace catalog fragment must be retained");
        assert_eq!(workspace_fragment.authority(), PromptAuthority::User);
        assert!(
            workspace_fragment
                .content()
                .contains("WORKSPACE_SKILL_SENTINEL")
        );
        let operator_skill_position = context
            .base_suffix
            .find("OPERATOR_SKILL_SENTINEL")
            .expect("operator skill remains in compatibility view");
        let workspace_skill_position = context
            .base_suffix
            .find("WORKSPACE_SKILL_SENTINEL")
            .expect("workspace skill remains in compatibility view");
        assert!(
            operator_skill_position < workspace_skill_position,
            "flattened compatibility order must match typed source order",
        );

        crate::agent::arming::apply_skill_listing(&mut context, &catalog, false);
        assert!(context.base_suffix.is_empty());
        let gated_plan = context
            .stable_prompt_plan()
            .expect("root typed plan remains installed after gating");
        assert!(gated_plan.fragments().iter().all(|fragment| !matches!(
            fragment.source(),
            PromptSource::SkillCatalogPolicy
                | PromptSource::OperatorSkillCatalog
                | PromptSource::WorkspaceSkillCatalog
        )));
        assert!(!context.base_system_instruction().contains("SKILL_SENTINEL"));
    }

    #[test]
    fn operator_override_replaces_profile_without_losing_append_authority() {
        let registry = ToolRegistry::new();
        let mut context = LoopContext::new("workspace profile");

        install_system_prompt(
            &mut context,
            SystemPromptInstall {
                registry: &registry,
                mode: ExecutionMode::Headless,
                has_output_schema: false,
                system_prompt_override: Some("operator replacement".to_owned()),
                append_system_prompt: Some("operator append".to_owned()),
                profile_source: PromptSource::WorkspaceProfile,
                has_auto_compact: false,
                capabilities: ProviderCapabilities::openai_responses(),
            },
        );

        let plan = context
            .stable_prompt_plan()
            .expect("root prompt installation must publish a typed plan");
        assert!(
            plan.fragments()
                .iter()
                .all(|fragment| fragment.source() != PromptSource::WorkspaceProfile)
        );
        let override_fragment = plan
            .fragments()
            .iter()
            .find(|fragment| fragment.source() == PromptSource::OperatorOverride)
            .expect("operator override must be present");
        assert_eq!(override_fragment.authority(), PromptAuthority::Developer);
        assert_eq!(
            override_fragment.content(),
            "operator replacement\n\noperator append"
        );
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
