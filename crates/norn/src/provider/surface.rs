//! Provider-resolved tool surface — the single resolution step between the
//! tool registry and every provider-facing projection.
//!
//! A tool registered in the [`ToolRegistry`] can be presented to a provider
//! in one of two ways: as a callable **function** definition, or replaced by
//! a **hosted** tool the provider executes natively (for example hosted web
//! search on providers whose [`ProviderCapabilities::hosted_web_search`] is
//! set). That decision used to be made independently in three places — the
//! provider request, the `tool_search` catalog, and the system-prompt tools
//! section — and they diverged: on hosted-capable providers the catalog and
//! prompt kept advertising `web_search` as a callable function even though
//! the wire had swapped it for the hosted tool.
//!
//! This module is now the only place that decision lives.
//! [`resolve_tool_presentation`] is the single capability predicate, keyed
//! purely on [`ProviderCapabilities`] (never on a provider's name, so a
//! future provider that sets `hosted_web_search` gets correct behaviour with
//! zero further changes), and all three consumers derive from it:
//!
//! 1. **Provider request definitions** — [`ResolvedToolSurface::resolve`] +
//!    [`ResolvedToolSurface::provider_definitions`], called per-request in
//!    `loop/runner.rs`, so the wire always reflects the live provider's
//!    capabilities.
//! 2. **Tool catalog** — [`reframe_catalog_entries`], applied by the
//!    `tool_search` tool at query time against the capabilities of the
//!    provider currently published on the tool context. Query time is the
//!    cheapest guaranteed-fresh point: the catalog snapshot itself can be
//!    installed before a provider is bound (the CLI does), and a provider
//!    rebind republishes the provider extension, so no cached surface can
//!    go stale.
//! 3. **System-prompt tools section** — [`reframe_prompt_entries`] at
//!    assembly (where the builder holds the provider), plus
//!    [`hosted_tools_prompt_section`], a per-iteration dynamic section the
//!    runner injects from the live provider so the prompt truth is
//!    recomputed at every provider call on every launch path.
//!
//! [`collect_function_definitions`] is the one registry → function-tool
//! projection (envelope-wrapped schemas) shared by builder assembly and the
//! spawn/fork child launch paths, so the inputs to the surface cannot drift
//! between parents and children.

use std::collections::HashSet;

use super::request::ToolDefinition;
use super::tools::{
    HostedToolDefinition, HostedWebSearchTool, ProviderCapabilities, ProviderToolDefinition,
};
use crate::system_prompt::builder::ToolPromptEntry;
use crate::tool::catalog::ToolCatalogEntry;
use crate::tool::registry::ToolRegistry;
use crate::tool::wrap_schema_with_envelope;
use crate::tools::web::WEB_SEARCH_TOOL_NAME;

/// How a single tool is presented to the provider.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolPresentation {
    /// Sent to the provider as a callable function definition.
    Function,
    /// Replaced by a provider-hosted tool: the provider executes it
    /// natively and the function definition is never sent. Boxed because
    /// hosted definitions carry full configuration structs while the
    /// common `Function` variant carries nothing.
    Hosted(Box<HostedToolDefinition>),
}

/// Resolve how the tool named `name` is presented under `capabilities`.
///
/// The single source of truth for hosted-versus-function presentation;
/// every projection in this module derives from it. Keyed only on the
/// capabilities struct — nothing provider-specific is named here.
#[must_use]
pub fn resolve_tool_presentation(
    name: &str,
    capabilities: ProviderCapabilities,
) -> ToolPresentation {
    if capabilities.hosted_web_search && name == WEB_SEARCH_TOOL_NAME {
        ToolPresentation::Hosted(Box::new(HostedToolDefinition::WebSearch(
            HostedWebSearchTool::default(),
        )))
    } else {
        ToolPresentation::Function
    }
}

/// A tool definition paired with its resolved provider presentation.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTool {
    /// The function-tool definition; for hosted tools this is the registry
    /// tool the hosted capability replaces.
    pub definition: ToolDefinition,
    /// How the provider sees this tool.
    pub presentation: ToolPresentation,
}

/// The resolved tool surface for one provider binding: every tool in input
/// order, each carrying its presentation.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedToolSurface {
    tools: Vec<ResolvedTool>,
}

impl ResolvedToolSurface {
    /// Resolve `tools` against `capabilities`.
    ///
    /// Pure function of its inputs — no caching, so callers that resolve
    /// at the point of use (the runner resolves per provider request) can
    /// never observe a stale surface across a provider change.
    #[must_use]
    pub fn resolve(tools: &[ToolDefinition], capabilities: ProviderCapabilities) -> Self {
        let tools = tools
            .iter()
            .cloned()
            .map(|definition| {
                let presentation = resolve_tool_presentation(&definition.name, capabilities);
                ResolvedTool {
                    definition,
                    presentation,
                }
            })
            .collect();
        Self { tools }
    }

    /// Every resolved tool, in input order.
    #[must_use]
    pub fn tools(&self) -> &[ResolvedTool] {
        &self.tools
    }

    /// Projection 1: the provider request definitions — function tools are
    /// sent as functions, hosted tools as their provider-native definition.
    #[must_use]
    pub fn provider_definitions(&self) -> Vec<ProviderToolDefinition> {
        self.tools
            .iter()
            .map(|tool| match &tool.presentation {
                ToolPresentation::Function => {
                    ProviderToolDefinition::Function(tool.definition.clone())
                }
                ToolPresentation::Hosted(hosted) => {
                    ProviderToolDefinition::Hosted(hosted.as_ref().clone())
                }
            })
            .collect()
    }
}

/// Provider-truth description for a hosted tool.
///
/// Replaces the registry tool's function-mode description everywhere the
/// surface is described to the model (tool catalog, system-prompt tools
/// section, per-iteration surface note), so the model learns from
/// `tool_search` and the prompt that the capability is provider-native —
/// not a phantom callable function.
#[must_use]
pub fn hosted_surface_description(tool_name: &str, hosted: &HostedToolDefinition) -> String {
    match hosted {
        HostedToolDefinition::WebSearch(_) => format!(
            "Provided natively by the current model provider: web search runs \
             server-side and its results arrive directly in the response. \
             `{tool_name}` is not a callable function tool in this session — do \
             not emit `{tool_name}` function calls; use the provider's built-in \
             web-search capability instead."
        ),
    }
}

/// Provider-truth usage guidance for a hosted tool, complementing
/// [`hosted_surface_description`] in the system-prompt tools section.
#[must_use]
pub fn hosted_surface_usage(hosted: &HostedToolDefinition) -> String {
    match hosted {
        HostedToolDefinition::WebSearch(_) => {
            "Use the native capability to find external information, documentation, \
             or answers not available in the local codebase. To retrieve the full \
             content of a specific URL found via search, use the `web_fetch` \
             function tool."
                .to_owned()
        }
    }
}

/// Projection 2: the tool catalog as it must read under `capabilities`.
///
/// Hosted-replaced entries are **kept** (the model should still discover
/// the capability through `tool_search`) but reframed: their description
/// becomes the provider truth and their function-call field hints are
/// dropped, because there is no function call to construct. Entries for
/// function-presented tools pass through unchanged. Subcommand entries are
/// keyed by their parent tool's name.
#[must_use]
pub fn reframe_catalog_entries(
    entries: &[ToolCatalogEntry],
    capabilities: ProviderCapabilities,
) -> Vec<ToolCatalogEntry> {
    entries
        .iter()
        .map(|entry| {
            let effective_name = entry.parent_tool.as_deref().unwrap_or(entry.name.as_str());
            match resolve_tool_presentation(effective_name, capabilities) {
                ToolPresentation::Function => entry.clone(),
                ToolPresentation::Hosted(hosted) => {
                    let mut reframed = entry.clone();
                    reframed.description = hosted_surface_description(effective_name, &hosted);
                    reframed.fields = Vec::new();
                    reframed
                }
            }
        })
        .collect()
}

/// Projection 3: the system-prompt tool entries as they must read under
/// `capabilities`. Hosted-replaced tools stay listed but carry the provider
/// truth instead of their function-mode description and usage guidance.
#[must_use]
pub fn reframe_prompt_entries(
    entries: Vec<ToolPromptEntry>,
    capabilities: ProviderCapabilities,
) -> Vec<ToolPromptEntry> {
    entries
        .into_iter()
        .map(|mut entry| {
            if let ToolPresentation::Hosted(hosted) =
                resolve_tool_presentation(&entry.name, capabilities)
            {
                entry.description = hosted_surface_description(&entry.name, &hosted);
                entry.usage_guidance = Some(hosted_surface_usage(&hosted));
            }
            entry
        })
        .collect()
}

/// Per-iteration dynamic system section stating which tools the current
/// provider hosts natively.
///
/// The runner injects this from the **live** provider's capabilities at
/// every iteration, so the prompt truth is recomputed at exactly the same
/// cadence as the wire resolution — a provider rebind between iterations
/// (or a launch path whose static prompt was assembled before the provider
/// was bound, like the CLI's) can never leave a stale function-style
/// framing standing. Returns `None` when nothing is hosted: function mode
/// is precisely what the static prompt and the tools' own guidance already
/// describe, so no correction is needed.
#[must_use]
pub fn hosted_tools_prompt_section(
    tools: &[ToolDefinition],
    capabilities: ProviderCapabilities,
) -> Option<String> {
    let lines: Vec<String> = tools
        .iter()
        .filter_map(
            |tool| match resolve_tool_presentation(&tool.name, capabilities) {
                ToolPresentation::Hosted(hosted) => Some(format!(
                    "- **{}**: {}",
                    tool.name,
                    hosted_surface_description(&tool.name, &hosted)
                )),
                ToolPresentation::Function => None,
            },
        )
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "# Provider Tool Surface\n\nThe current model provider hosts the \
         following tools natively. They are not callable functions in this \
         session — this supersedes any function-style description of them \
         elsewhere in the prompt:\n\n{}",
        lines.join("\n")
    ))
}

/// Project the registry's currently-available tools into function-tool
/// definitions (envelope-wrapped schemas), optionally filtered through an
/// allow-list.
///
/// This is the single registry → function-definition projection feeding the
/// resolved surface: builder assembly uses it with no allow-list, and the
/// spawn/fork child launch paths use it with the per-child allow-list, so a
/// child's inputs to the surface can never drift from its parent's.
#[must_use]
pub fn collect_function_definitions(
    registry: &ToolRegistry,
    allow_list: Option<&[String]>,
) -> Vec<ToolDefinition> {
    let allow_set: Option<HashSet<&str>> =
        allow_list.map(|names| names.iter().map(String::as_str).collect());
    registry
        .names()
        .filter(|name| allow_set.as_ref().is_none_or(|set| set.contains(name)))
        .filter_map(|name| {
            registry.get(name).map(|tool| ToolDefinition {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: wrap_schema_with_envelope(tool.input_schema()),
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::tool::traits::ToolCategory;

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            description: "tool".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn hosted_caps() -> ProviderCapabilities {
        ProviderCapabilities {
            hosted_web_search: true,
            ..ProviderCapabilities::default()
        }
    }

    #[test]
    fn web_search_presents_as_function_without_capability() {
        assert_eq!(
            resolve_tool_presentation(WEB_SEARCH_TOOL_NAME, ProviderCapabilities::default()),
            ToolPresentation::Function,
        );
    }

    #[test]
    fn web_search_presents_as_hosted_with_capability() {
        assert!(matches!(
            resolve_tool_presentation(WEB_SEARCH_TOOL_NAME, hosted_caps()),
            ToolPresentation::Hosted(hosted)
                if matches!(hosted.as_ref(), HostedToolDefinition::WebSearch(_)),
        ));
    }

    #[test]
    fn other_tools_always_present_as_functions() {
        assert_eq!(
            resolve_tool_presentation("read", hosted_caps()),
            ToolPresentation::Function,
        );
    }

    #[test]
    fn provider_definitions_keep_web_search_as_function_without_capability() {
        let surface = ResolvedToolSurface::resolve(
            &[tool(WEB_SEARCH_TOOL_NAME)],
            ProviderCapabilities::default(),
        );
        assert!(matches!(
            surface.provider_definitions().as_slice(),
            [ProviderToolDefinition::Function(function)]
                if function.name == WEB_SEARCH_TOOL_NAME
        ));
    }

    #[test]
    fn provider_definitions_swap_web_search_when_hosted_preserving_order() {
        let surface = ResolvedToolSurface::resolve(
            &[tool("read_file"), tool(WEB_SEARCH_TOOL_NAME), tool("bash")],
            ProviderCapabilities::openai_responses(),
        );
        assert!(matches!(
            surface.provider_definitions().as_slice(),
            [
                ProviderToolDefinition::Function(first),
                ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(_)),
                ProviderToolDefinition::Function(last),
            ] if first.name == "read_file" && last.name == "bash"
        ));
    }

    #[test]
    fn resolved_surface_retains_replaced_definition_for_hosted_tools() {
        let surface = ResolvedToolSurface::resolve(&[tool(WEB_SEARCH_TOOL_NAME)], hosted_caps());
        let resolved = &surface.tools()[0];
        assert_eq!(resolved.definition.name, WEB_SEARCH_TOOL_NAME);
        assert!(matches!(
            &resolved.presentation,
            ToolPresentation::Hosted(hosted)
                if matches!(hosted.as_ref(), HostedToolDefinition::WebSearch(_)),
        ));
    }

    #[test]
    fn catalog_reframing_flips_with_capabilities() {
        let entries = vec![
            ToolCatalogEntry::from_tool_schema(
                WEB_SEARCH_TOOL_NAME,
                "Search the public web.",
                &serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string", "description": "q" } },
                    "required": ["query"]
                }),
            ),
            ToolCatalogEntry::tool("read", "Read a file."),
        ];

        let function_view = reframe_catalog_entries(&entries, ProviderCapabilities::default());
        assert_eq!(function_view[0].description, "Search the public web.");
        assert_eq!(function_view[0].fields.len(), 1);
        assert_eq!(function_view[1].description, "Read a file.");

        let hosted_view = reframe_catalog_entries(&entries, hosted_caps());
        assert!(
            hosted_view[0]
                .description
                .contains("not a callable function"),
            "hosted catalog entry must carry the provider truth: {}",
            hosted_view[0].description,
        );
        assert!(
            hosted_view[0].fields.is_empty(),
            "hosted entries carry no function-call field hints",
        );
        assert_eq!(
            hosted_view[0].name, WEB_SEARCH_TOOL_NAME,
            "the entry is kept, not removed",
        );
        assert_eq!(hosted_view[1].description, "Read a file.");
    }

    #[test]
    fn prompt_reframing_flips_with_capabilities() {
        let entries = || {
            vec![
                ToolPromptEntry {
                    name: WEB_SEARCH_TOOL_NAME.to_owned(),
                    category: ToolCategory::Web,
                    description: "Search the public web.".to_owned(),
                    usage_guidance: Some("Use for external info.".to_owned()),
                },
                ToolPromptEntry {
                    name: "read".to_owned(),
                    category: ToolCategory::FileSystem,
                    description: "Read a file.".to_owned(),
                    usage_guidance: None,
                },
            ]
        };

        let function_view = reframe_prompt_entries(entries(), ProviderCapabilities::default());
        assert_eq!(function_view[0].description, "Search the public web.");

        let hosted_view = reframe_prompt_entries(entries(), hosted_caps());
        assert!(
            hosted_view[0]
                .description
                .contains("not a callable function"),
        );
        assert!(
            hosted_view[0]
                .usage_guidance
                .as_deref()
                .unwrap_or_default()
                .contains("native capability"),
        );
        assert_eq!(hosted_view[1].description, "Read a file.");
    }

    #[test]
    fn hosted_section_present_only_when_something_is_hosted() {
        let tools = [tool("read"), tool(WEB_SEARCH_TOOL_NAME)];
        assert!(hosted_tools_prompt_section(&tools, ProviderCapabilities::default()).is_none());

        let section =
            hosted_tools_prompt_section(&tools, hosted_caps()).expect("hosted section present");
        assert!(section.contains("# Provider Tool Surface"));
        assert!(section.contains(WEB_SEARCH_TOOL_NAME));
        assert!(!section.contains("- **read**"));

        let no_web = [tool("read"), tool("bash")];
        assert!(
            hosted_tools_prompt_section(&no_web, hosted_caps()).is_none(),
            "no hosted-replaceable tool registered means no section",
        );
    }
}
