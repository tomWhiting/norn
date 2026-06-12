//! `ToolSearch` — BM25 keyword search over the tool catalog.
//!
//! The tool itself does not walk the [`crate::tool::ToolRegistry`]; that
//! would couple it to the registry's internals and risks cyclical lookups
//! (the search tool would be searching for itself). Instead, the
//! orchestrator publishes a list of [`ToolCatalogEntry`] records on the
//! tool context via the [`SharedToolCatalog`] extension and the search
//! ranks against that snapshot. Empty queries return alphabetically-sorted
//! results so the model gets a stable catalogue dump.
//!
//! The catalog types themselves live in [`crate::tool::catalog`].
//!
//! The published catalog snapshot is provider-blind; before ranking, this
//! tool projects it through the resolved tool surface
//! ([`reframe_catalog_entries`]) against the capabilities of the provider
//! currently published on the tool context, so hosted-replaced tools (e.g.
//! `web_search` on a hosted-search provider) are described as
//! provider-native capabilities rather than phantom callable functions.
//! Query time is the guaranteed-fresh resolution point: the snapshot may be
//! installed before any provider is bound, and a provider rebind republishes
//! the provider extension, so no cached surface can go stale.

use std::sync::Arc;

use async_trait::async_trait;
use bm25::{Document, Language, SearchEngineBuilder};
use serde::Deserialize;

use crate::error::ToolError;
use crate::internal::extraction::SharedProvider;
use crate::provider::surface::reframe_catalog_entries;
use crate::provider::tools::ProviderCapabilities;
use crate::tool::catalog::{SharedToolCatalog, ToolCatalogEntry};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Default result count when the caller does not specify `max_results`.
const DEFAULT_MAX_RESULTS: usize = 10;

/// Performs BM25 search over the catalog.
pub struct ToolSearchTool;

impl ToolSearchTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    max_results: Option<u32>,
}

/// Sort key for the empty-query catalogue dump: the *displayed* tool name
/// (the parent for subcommand entries) first, then the command value, so a
/// composite tool's subcommands group under it and the displayed list is
/// genuinely alphabetical.
fn display_sort_key(entry: &ToolCatalogEntry) -> (&str, &str) {
    (
        entry.parent_tool.as_deref().unwrap_or(entry.name.as_str()),
        entry.command_value.as_deref().unwrap_or_default(),
    )
}

fn alphabetical_results(entries: &[ToolCatalogEntry], limit: usize) -> Vec<serde_json::Value> {
    let mut sorted: Vec<&ToolCatalogEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| display_sort_key(a).cmp(&display_sort_key(b)));
    sorted
        .into_iter()
        .take(limit)
        .map(|e| format_result(e, 0.0))
        .collect()
}

fn format_result(entry: &ToolCatalogEntry, score: f32) -> serde_json::Value {
    let mut value = serde_json::json!({
        "name": entry.parent_tool.as_deref().unwrap_or(entry.name.as_str()),
        "description": entry.description,
        "score": score,
    });
    if let Some(parent_tool) = &entry.parent_tool {
        value["parent_tool"] = serde_json::Value::String(parent_tool.clone());
    }
    if let Some(command_value) = &entry.command_value {
        value["command_value"] = serde_json::Value::String(command_value.clone());
    }
    if !entry.fields.is_empty() {
        value["fields"] = serde_json::json!(entry.fields);
    }
    value
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &'static str {
        "tool_search"
    }

    fn description(&self) -> &'static str {
        include_str!("guidance/tool_search.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Discovery
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("guidance/tool_search.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords to search for in tool names and descriptions. Empty string returns all tools."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of results to return. Defaults to 10."
                }
            },
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SearchArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;
        let catalog: Arc<SharedToolCatalog> = ctx.require_extension::<SharedToolCatalog>()?;

        let limit = args
            .max_results
            .map_or(DEFAULT_MAX_RESULTS, |n| {
                usize::try_from(n).unwrap_or(usize::MAX)
            })
            .max(1);

        // Resolve the provider-blind snapshot against the live provider's
        // capabilities. No provider published means no provider is bound,
        // and with no provider there is no hosted surface — the default
        // (all-false) capabilities describe exactly that.
        let capabilities = ctx
            .get_extension::<SharedProvider>()
            .map_or_else(ProviderCapabilities::default, |provider| {
                provider.0.capabilities()
            });
        let entries = reframe_catalog_entries(catalog.0.as_ref(), capabilities);
        if entries.is_empty() {
            return Ok(ToolOutput::success(
                serde_json::json!({ "results": Vec::<serde_json::Value>::new() }),
            ));
        }

        let query = args.query.trim();
        let results: Vec<serde_json::Value> = if query.is_empty() {
            alphabetical_results(&entries, limit)
        } else {
            let documents: Vec<Document<usize>> = entries
                .iter()
                .enumerate()
                .map(|(idx, entry)| Document::new(idx, entry.searchable_text()))
                .collect();
            let engine =
                SearchEngineBuilder::<usize>::with_documents(Language::English, documents).build();
            engine
                .search(query, limit)
                .into_iter()
                .filter_map(|result| {
                    let id = result.document.id;
                    entries
                        .get(id)
                        .map(|entry| format_result(entry, result.score))
                })
                .collect()
        };

        Ok(ToolOutput::success(
            serde_json::json!({ "results": results }),
        ))
    }
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
    use serde_json::json;

    use super::*;
    use crate::tool::catalog::ToolFieldHint;
    use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};

    fn entries() -> Vec<ToolCatalogEntry> {
        vec![
            ToolCatalogEntry::tool("read", "Read a file from disk with line numbers"),
            ToolCatalogEntry::tool(
                "write",
                "Write contents to a file enforcing read-before-overwrite",
            ),
            ToolCatalogEntry::tool(
                "edit",
                "Edit an existing file by string replacement with AST validation",
            ),
            ToolCatalogEntry::tool("bash", "Execute a shell command and stream output"),
            ToolCatalogEntry::tool(
                "web_search",
                "Search the public web for a query and return results",
            ),
        ]
    }

    fn build_ctx() -> ToolContext {
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(entries()))));
        ctx
    }

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "tool_search".to_string(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn ranks_relevant_tool_first() {
        let tool = ToolSearchTool::new();
        let ctx = build_ctx();
        let out = tool
            .execute(&envelope_for(json!({"query": "shell command"})), &ctx)
            .await
            .unwrap();
        let results = out.content["results"].as_array().unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0]["name"], "bash");
        assert!(results[0]["score"].as_f64().unwrap_or(0.0) > 0.0);
    }

    #[tokio::test]
    async fn respects_max_results() {
        let tool = ToolSearchTool::new();
        let ctx = build_ctx();
        let out = tool
            .execute(
                &envelope_for(json!({"query": "file", "max_results": 2})),
                &ctx,
            )
            .await
            .unwrap();
        let results = out.content["results"].as_array().unwrap();
        assert!(results.len() <= 2);
    }

    #[tokio::test]
    async fn subcommand_match_returns_parent_and_command_value() {
        let tool = ToolSearchTool::new();
        let mut catalog = entries();
        catalog.push(ToolCatalogEntry::tool(
            "meridian_messaging",
            "Direct messages, channels, and notifications.",
        ));
        catalog.push(ToolCatalogEntry::subcommand_no_fields(
            "meridian_messaging",
            "send",
            "Send a DM to one or more recipients.",
        ));
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(catalog))));

        let out = tool
            .execute(&envelope_for(json!({"query": "send DM"})), &ctx)
            .await
            .unwrap();
        let results = out.content["results"].as_array().unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0]["name"], "meridian_messaging");
        assert_eq!(results[0]["parent_tool"], "meridian_messaging");
        assert_eq!(results[0]["command_value"], "send");
        assert_eq!(
            results[0]["description"],
            "Send a DM to one or more recipients."
        );
    }

    #[tokio::test]
    async fn normal_tool_results_omit_subcommand_fields() {
        let tool = ToolSearchTool::new();
        let mut catalog = entries();
        catalog.push(ToolCatalogEntry::subcommand_no_fields(
            "meridian_messaging",
            "read",
            "Read a specific message or conversation by ID.",
        ));
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(catalog))));

        let out = tool
            .execute(&envelope_for(json!({"query": "read file"})), &ctx)
            .await
            .unwrap();
        let first = &out.content["results"].as_array().unwrap()[0];
        assert_eq!(first["name"], "read");
        assert!(first.get("parent_tool").is_none());
        assert!(first.get("command_value").is_none());
        assert!(first.get("fields").is_none());
    }

    #[tokio::test]
    async fn parameterless_subcommand_results_omit_fields() {
        let tool = ToolSearchTool::new();
        let mut catalog = entries();
        catalog.push(ToolCatalogEntry::subcommand_no_fields(
            "meridian_messaging",
            "read_message",
            "Read a specific message or conversation by ID.",
        ));
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(catalog))));

        let out = tool
            .execute(
                &envelope_for(json!({"query": "specific message conversation"})),
                &ctx,
            )
            .await
            .unwrap();
        let results = out.content["results"].as_array().unwrap();
        let subcommand = results
            .iter()
            .find(|result| result["command_value"] == "read_message")
            .expect("read_message subcommand result");
        assert!(subcommand.get("fields").is_none());
    }

    #[tokio::test]
    async fn subcommand_results_include_fields_when_present() {
        let tool = ToolSearchTool::new();
        let mut catalog = entries();
        catalog.push(ToolCatalogEntry::subcommand(
            "meridian_messaging",
            "send",
            "Send a DM to one or more recipients.",
            vec![
                ToolFieldHint {
                    name: "to".to_string(),
                    type_hint: "array".to_string(),
                    required: true,
                    description: "Recipient member handles.".to_string(),
                    enum_values: Vec::new(),
                },
                ToolFieldHint {
                    name: "priority".to_string(),
                    type_hint: "string".to_string(),
                    required: false,
                    description: "Delivery priority.".to_string(),
                    enum_values: vec!["normal".to_string(), "urgent".to_string()],
                },
            ],
        ));
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(catalog))));

        let out = tool
            .execute(&envelope_for(json!({"query": "send DM recipients"})), &ctx)
            .await
            .unwrap();
        let results = out.content["results"].as_array().unwrap();
        let subcommand = results
            .iter()
            .find(|result| result["command_value"] == "send")
            .expect("send subcommand result");
        assert_eq!(
            subcommand["fields"],
            json!([
                {
                    "name": "to",
                    "type_hint": "array",
                    "required": true,
                    "description": "Recipient member handles.",
                    "enum_values": []
                },
                {
                    "name": "priority",
                    "type_hint": "string",
                    "required": false,
                    "description": "Delivery priority.",
                    "enum_values": ["normal", "urgent"]
                }
            ])
        );
    }

    #[tokio::test]
    async fn empty_query_returns_alphabetical() {
        let tool = ToolSearchTool::new();
        let ctx = build_ctx();
        let out = tool
            .execute(&envelope_for(json!({"query": ""})), &ctx)
            .await
            .unwrap();
        let results = out.content["results"].as_array().unwrap();
        assert!(!results.is_empty());
        let names: Vec<&str> = results
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    fn install_provider(ctx: &ToolContext, hosted_web_search: bool) {
        let capabilities = ProviderCapabilities {
            hosted_web_search,
            ..ProviderCapabilities::default()
        };
        ctx.insert_extension(Arc::new(SharedProvider(Arc::new(
            crate::provider::mock::MockProvider::with_capabilities(Vec::new(), capabilities),
        ))));
    }

    /// Find the catalog-dump result for `name` via an empty query.
    async fn dump_entry(ctx: &ToolContext, name: &str) -> serde_json::Value {
        let tool = ToolSearchTool::new();
        let out = tool
            .execute(&envelope_for(json!({"query": "", "max_results": 500})), ctx)
            .await
            .unwrap();
        out.content["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|result| result["name"] == name)
            .unwrap_or_else(|| panic!("{name} entry present in catalog dump"))
            .clone()
    }

    #[tokio::test]
    async fn hosted_capability_reframes_web_search_entry_at_query_time() {
        let ctx = build_ctx();
        install_provider(&ctx, true);
        let entry = dump_entry(&ctx, "web_search").await;
        let description = entry["description"].as_str().unwrap();
        assert!(
            description.contains("not a callable function"),
            "hosted provider must reframe web_search as provider-native: {description}",
        );
        assert!(
            entry.get("fields").is_none(),
            "hosted entries carry no function-call field hints",
        );
    }

    #[tokio::test]
    async fn function_capability_keeps_web_search_function_entry() {
        let ctx = build_ctx();
        install_provider(&ctx, false);
        let entry = dump_entry(&ctx, "web_search").await;
        assert_eq!(
            entry["description"],
            "Search the public web for a query and return results",
        );
    }

    #[tokio::test]
    async fn absent_provider_extension_keeps_function_entries() {
        // No provider bound means no hosted surface — function framing.
        let ctx = build_ctx();
        let entry = dump_entry(&ctx, "web_search").await;
        assert_eq!(
            entry["description"],
            "Search the public web for a query and return results",
        );
    }

    #[tokio::test]
    async fn missing_catalog_returns_missing_extension() {
        let tool = ToolSearchTool::new();
        let ctx = ToolContext::empty();
        let err = tool
            .execute(&envelope_for(json!({"query": "x"})), &ctx)
            .await
            .expect_err("no catalog");
        match err {
            ToolError::MissingExtension { extension } => {
                assert!(
                    extension.contains("SharedToolCatalog"),
                    "error must name the missing extension type: {extension}",
                );
            }
            other => panic!("expected MissingExtension, got {other:?}"),
        }
    }
}
