//! Capability resolution and the [`from_profile`] [`LoopContext`] builder.
//!
//! Capabilities are composable bundles of tools, system instructions, and
//! disallowed patterns. The three `resolved_*` helpers on [`Profile`] merge
//! the profile's own configuration with every capability's contribution:
//!
//! - [`Profile::resolved_tools`] — deduplicated tool names (first occurrence
//!   wins for ordering).
//! - [`Profile::resolved_instructions`] — concatenated system-instruction
//!   strings, in profile-then-capabilities order.
//! - [`Profile::resolved_disallowed`] — every capability's disallowed
//!   patterns, in declaration order, duplicates retained.
//!
//! [`from_profile`] takes the resolved values and produces a configured
//! [`LoopContext`] plus a [`ToolRegistry`] gated to the resolved tools.

use crate::integration::hooks::HookRegistry;
use crate::r#loop::loop_context::LoopContext;
use crate::rules::engine::RuleEngine;
use crate::tool::registry::ToolRegistry;

use super::types::Profile;

impl Profile {
    /// Merge `Profile.tools` with each capability's tools, preserving the
    /// order of first occurrence and deduplicating.
    ///
    /// Returns `None` when neither `self.tools` nor any capability
    /// contributes tools — callers should treat `None` as "all registered
    /// tools are available" (i.e. no gating). Returns `Some(list)` when at
    /// least one source declares tools, even if the merged list is empty
    /// (e.g. `tools = []` with no capability tools is a deliberate
    /// lockdown).
    #[must_use]
    pub fn resolved_tools(&self) -> Option<Vec<String>> {
        let has_explicit_tools = self.tools.is_some();
        let has_capability_tools = self.capabilities.iter().any(|c| !c.tools.is_empty());

        if !has_explicit_tools && !has_capability_tools {
            return None;
        }

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<String> = Vec::new();
        if let Some(profile_tools) = self.tools.as_ref() {
            for tool in profile_tools {
                if seen.insert(tool.clone()) {
                    out.push(tool.clone());
                }
            }
        }
        for cap in &self.capabilities {
            for tool in &cap.tools {
                if seen.insert(tool.clone()) {
                    out.push(tool.clone());
                }
            }
        }
        Some(out)
    }

    /// Concatenate `Profile.system_instructions` with each capability's
    /// system instructions in declaration order. Returns the full ordered
    /// list — callers that want a single string typically join with
    /// `"\n\n"`.
    #[must_use]
    pub fn resolved_instructions(&self) -> Vec<String> {
        let mut out: Vec<String> = self.system_instructions.clone();
        for cap in &self.capabilities {
            out.extend(cap.system_instructions.iter().cloned());
        }
        out
    }

    /// Collect every capability's disallowed patterns into a single vec.
    ///
    /// Order preserves the capabilities' declaration order; duplicates are
    /// retained because callers may attach severity or per-occurrence
    /// metadata downstream.
    #[must_use]
    pub fn resolved_disallowed(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for cap in &self.capabilities {
            out.extend(cap.disallowed_patterns.iter().cloned());
        }
        out
    }
}

/// Build a configured [`LoopContext`] and gate `registry` to the profile's
/// resolved tool list.
///
/// The returned `LoopContext` carries:
///
/// - the merged resolved instructions joined with `"\n\n"` as the single
///   base system section,
/// - the profile's [`crate::provider::request::ReasoningEffort`] threaded
///   through to every provider request,
/// - the optional [`RuleEngine`] and [`HookRegistry`] supplied by the
///   caller,
/// - the profile's prompt commands and a fresh cache.
///
/// `registry` is taken by value and returned with
/// [`ToolRegistry::set_available`] applied so callers can chain
/// construction without re-borrowing.
#[must_use]
pub fn from_profile(
    profile: &Profile,
    mut registry: ToolRegistry,
    rules: Option<RuleEngine>,
    hooks: Option<std::sync::Arc<HookRegistry>>,
) -> (LoopContext, ToolRegistry) {
    let instructions = profile.resolved_instructions();
    let base = instructions.join("\n\n");
    let mut loop_context = LoopContext::new(base);
    loop_context
        .reasoning_effort
        .clone_from(&profile.reasoning_effort);
    loop_context
        .reasoning_summary
        .clone_from(&profile.reasoning_summary);
    loop_context.service_tier = profile.service_tier;
    loop_context.rules = rules;
    loop_context.hooks = hooks;
    loop_context
        .prompt_commands
        .clone_from(&profile.prompt_commands);

    if let Some(tools) = profile.resolved_tools() {
        registry.set_available(tools);
    }

    (loop_context, registry)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::uninlined_format_args,
    clippy::unnecessary_literal_bound
)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::profile::loader::parse_capability;
    use crate::profile::types::{Capability, PromptCommand};
    use crate::provider::request::ReasoningEffort;

    #[test]
    fn capability_composition_merges_tools_and_instructions() {
        let profile = Profile {
            name: "p".to_owned(),
            model: "m".to_owned(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            tools: Some(vec!["read".to_owned(), "edit".to_owned()]),
            system_instructions: vec!["base instruction".to_owned()],
            capabilities: vec![
                Capability {
                    name: "editing".to_owned(),
                    tools: vec!["edit".to_owned(), "write".to_owned()],
                    system_instructions: vec!["editing instruction".to_owned()],
                    disallowed_patterns: vec!["pattern-a".to_owned()],
                },
                Capability {
                    name: "shell".to_owned(),
                    tools: vec!["write".to_owned(), "bash".to_owned()],
                    system_instructions: vec!["shell instruction".to_owned()],
                    disallowed_patterns: vec!["pattern-b".to_owned()],
                },
            ],
            settings: HashMap::new(),
            prompt_commands: Vec::new(),
        };

        let tools = profile.resolved_tools();
        assert_eq!(
            tools,
            Some(vec![
                "read".to_owned(),
                "edit".to_owned(),
                "write".to_owned(),
                "bash".to_owned(),
            ]),
            "duplicates must be removed while preserving first-occurrence order",
        );

        let instructions = profile.resolved_instructions();
        assert_eq!(
            instructions,
            vec![
                "base instruction".to_owned(),
                "editing instruction".to_owned(),
                "shell instruction".to_owned(),
            ],
        );

        let disallowed = profile.resolved_disallowed();
        assert_eq!(
            disallowed,
            vec!["pattern-a".to_owned(), "pattern-b".to_owned()],
        );
    }

    #[test]
    fn resolved_tools_none_when_no_tools_configured() {
        let profile = Profile {
            name: "p".to_owned(),
            model: "m".to_owned(),
            ..Profile::default()
        };
        assert!(
            profile.resolved_tools().is_none(),
            "no tools and no capability tools must return None (no gating)",
        );
    }

    #[test]
    fn resolved_tools_some_empty_when_explicit_empty_list() {
        let profile = Profile {
            name: "p".to_owned(),
            model: "m".to_owned(),
            tools: Some(Vec::new()),
            ..Profile::default()
        };
        assert_eq!(
            profile.resolved_tools(),
            Some(Vec::new()),
            "explicit empty tools list must return Some(empty) for deliberate lockdown",
        );
    }

    /// Stub Tool used to verify [`from_profile`] applies the registry gate
    /// without inventing or depending on real tool behaviour.
    struct StubTool {
        tool_name: String,
    }

    #[async_trait::async_trait]
    impl crate::tool::traits::Tool for StubTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> crate::tool::scheduling::ToolEffect {
            crate::tool::scheduling::ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &crate::tool::envelope::ToolEnvelope,
            _ctx: &crate::tool::context::ToolContext,
        ) -> Result<crate::tool::traits::ToolOutput, crate::error::ToolError> {
            Ok(crate::tool::traits::ToolOutput::success(serde_json::json!(
                null
            )))
        }
    }

    /// N-020 R3: `from_profile` builds a `LoopContext` carrying the merged
    /// resolved instructions as the base system section, threads
    /// `reasoning_effort`, and gates the registry so only the profile's
    /// resolved tools are reachable.
    #[test]
    fn from_profile_restricts_registry_to_listed_tools() {
        let profile = Profile {
            name: "p".to_owned(),
            model: "m".to_owned(),
            reasoning_effort: Some(ReasoningEffort::Medium),
            reasoning_summary: None,
            service_tier: None,
            tools: Some(vec!["read".to_owned(), "write".to_owned()]),
            system_instructions: vec!["be careful".to_owned()],
            capabilities: vec![Capability {
                name: "cap".to_owned(),
                tools: Vec::new(),
                system_instructions: vec!["also this".to_owned()],
                disallowed_patterns: Vec::new(),
            }],
            settings: HashMap::new(),
            prompt_commands: Vec::new(),
        };

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(StubTool {
            tool_name: "read".to_owned(),
        }));
        registry.register(Box::new(StubTool {
            tool_name: "write".to_owned(),
        }));
        registry.register(Box::new(StubTool {
            tool_name: "bash".to_owned(),
        }));

        let (loop_ctx, registry) = from_profile(&profile, registry, None, None);

        assert!(registry.get("read").is_some(), "read must be available");
        assert!(registry.get("write").is_some(), "write must be available");
        assert!(
            registry.get("bash").is_none(),
            "bash must be gated out by from_profile",
        );

        assert_eq!(
            loop_ctx.reasoning_effort,
            Some(ReasoningEffort::Medium),
            "from_profile must thread reasoning_effort",
        );
        let base = &loop_ctx.system_sections[0];
        assert!(base.contains("be careful"));
        assert!(base.contains("also this"));
    }

    #[test]
    fn from_profile_no_gating_when_tools_not_configured() {
        let profile = Profile {
            name: "default".to_owned(),
            model: "m".to_owned(),
            ..Profile::default()
        };

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(StubTool {
            tool_name: "read".to_owned(),
        }));
        registry.register(Box::new(StubTool {
            tool_name: "bash".to_owned(),
        }));
        registry.register(Box::new(StubTool {
            tool_name: "edit".to_owned(),
        }));

        let (_loop_ctx, registry) = from_profile(&profile, registry, None, None);

        assert!(
            registry.get("read").is_some(),
            "all tools must be available when profile has no tool config",
        );
        assert!(
            registry.get("bash").is_some(),
            "all tools must be available when profile has no tool config",
        );
        assert!(
            registry.get("edit").is_some(),
            "all tools must be available when profile has no tool config",
        );
        assert_eq!(registry.len(), 3);
    }

    #[test]
    fn from_profile_threads_prompt_commands() {
        let profile = Profile {
            name: "p".to_owned(),
            model: "m".to_owned(),
            prompt_commands: vec![PromptCommand {
                name: "cwd".to_owned(),
                command: "echo x".to_owned(),
                cache_ttl: None,
            }],
            ..Profile::default()
        };
        let (loop_ctx, _registry) = from_profile(&profile, ToolRegistry::new(), None, None);
        assert_eq!(loop_ctx.prompt_commands.len(), 1);
        assert_eq!(loop_ctx.prompt_commands[0].name, "cwd");
    }

    /// Two capabilities parsed from markdown via the loader compose into a
    /// profile and resolve correctly through the existing capability
    /// merge logic — verifying loader and resolver fit together.
    #[test]
    fn from_profile_resolves_markdown_parsed_capabilities() {
        let editing_md = "---\nname: editing\ntools: edit, write\ndisallowedTools: rm -rf\n---\nPrefer minimal diffs.\n";
        let shell_md =
            "---\nname: shell\ntools: bash\ndisallowedTools: sudo\n---\nUse bash sparingly.\n";

        let editing =
            parse_capability(editing_md, &PathBuf::from("editing.md")).expect("editing parses");
        let shell = parse_capability(shell_md, &PathBuf::from("shell.md")).expect("shell parses");

        let profile = Profile {
            name: "composed".to_owned(),
            model: "gpt-5".to_owned(),
            tools: Some(vec!["read".to_owned(), "edit".to_owned()]),
            system_instructions: vec!["You are a composed agent.".to_owned()],
            capabilities: vec![editing, shell],
            ..Profile::default()
        };

        let tools = profile.resolved_tools().unwrap();
        assert_eq!(
            tools,
            vec![
                "read".to_owned(),
                "edit".to_owned(),
                "write".to_owned(),
                "bash".to_owned(),
            ],
            "markdown-parsed capability tools must merge with profile tools, deduplicated"
        );

        let instructions = profile.resolved_instructions();
        assert_eq!(
            instructions,
            vec![
                "You are a composed agent.".to_owned(),
                "Prefer minimal diffs.".to_owned(),
                "Use bash sparingly.".to_owned(),
            ]
        );

        let disallowed = profile.resolved_disallowed();
        assert_eq!(
            disallowed,
            vec!["rm -rf".to_owned(), "sudo".to_owned()],
            "disallowed patterns from markdown-parsed capabilities must flow through resolved_disallowed"
        );
    }
}
