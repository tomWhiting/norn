//! `ToolSearch` — BM25 keyword search over the tool catalog.
//!
//! The tool itself does not walk the [`crate::tool::ToolRegistry`]; that
//! would couple it to the registry's internals and risks cyclical lookups
//! (the search tool would be searching for itself). Instead, the
//! orchestrator publishes a list of [`ToolCatalogEntry`] records on the
//! tool context via the [`SharedToolCatalog`] extension and the search
//! ranks against that snapshot. Empty queries return alphabetically-sorted
//! results so the model gets a stable catalogue dump.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use bm25::{Document, Language, SearchEngineBuilder};
use serde::Deserialize;

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Field-level hints for constructing a cataloged subcommand call.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ToolFieldHint {
    /// JSON field name accepted by the subcommand.
    pub name: String,
    /// Coarse field type hint such as `string`, `integer`, `boolean`, or `array`.
    pub type_hint: String,
    /// Whether the field must be supplied for this command.
    pub required: bool,
    /// Human-facing field description.
    pub description: String,
    /// Allowed values when the field is constrained to an enum-like set.
    pub enum_values: Vec<String>,
}

/// A single searchable tool or subcommand description.
#[derive(Clone, Debug)]
pub struct ToolCatalogEntry {
    /// Tool or subcommand name.
    pub name: String,
    /// Human-facing description.
    pub description: String,
    /// Parent composite tool name when this entry describes a subcommand.
    pub parent_tool: Option<String>,
    /// Concrete command value to pass to the parent composite tool.
    pub command_value: Option<String>,
    /// Field-level hints for constructing this entry when it is a subcommand.
    pub fields: Vec<ToolFieldHint>,
}

impl ToolCatalogEntry {
    /// Construct a top-level tool catalog entry.
    #[must_use]
    pub fn tool(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parent_tool: None,
            command_value: None,
            fields: Vec::new(),
        }
    }

    /// Construct a subcommand entry for a composite parent tool.
    #[must_use]
    pub fn subcommand(
        parent_tool: impl Into<String>,
        command_value: impl Into<String>,
        description: impl Into<String>,
        fields: Vec<ToolFieldHint>,
    ) -> Self {
        let parent_tool = parent_tool.into();
        let command_value = command_value.into();
        Self {
            name: command_value.clone(),
            description: description.into(),
            parent_tool: Some(parent_tool),
            command_value: Some(command_value),
            fields,
        }
    }

    /// Construct a parameterless subcommand entry for a composite parent tool.
    #[must_use]
    pub fn subcommand_no_fields(
        parent_tool: impl Into<String>,
        command_value: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::subcommand(parent_tool, command_value, description, Vec::new())
    }

    fn searchable_text(&self) -> String {
        let parent = self.parent_tool.as_deref().unwrap_or_default();
        let command = self.command_value.as_deref().unwrap_or_default();
        format!("{} {parent} {command} {}", self.name, self.description)
    }
}

/// Additional catalog entries supplied by an embedding runtime before the
/// final [`SharedToolCatalog`] snapshot is published.
pub struct ToolCatalogExtras(pub Vec<ToolCatalogEntry>);

/// Shared catalog handle.
///
/// Wrapping `Vec<ToolCatalogEntry>` in a named type lets the extension
/// map key on this type rather than the bare `Vec`, which keeps multiple
/// catalogues from accidentally clashing.
pub struct SharedToolCatalog(pub Arc<Vec<ToolCatalogEntry>>);

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

fn alphabetical_results(entries: &[ToolCatalogEntry], limit: usize) -> Vec<serde_json::Value> {
    let mut sorted: Vec<&ToolCatalogEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
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
        let started = Instant::now();
        let args: SearchArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;
        let catalog: Arc<SharedToolCatalog> =
            ctx.get_extension::<SharedToolCatalog>()
                .ok_or_else(|| ToolError::ExecutionFailed {
                    reason: "tool catalog not configured in tool context".to_string(),
                })?;

        let limit = args
            .max_results
            .map_or(DEFAULT_MAX_RESULTS, |n| {
                usize::try_from(n).unwrap_or(usize::MAX)
            })
            .max(1);

        let entries = catalog.0.as_ref();
        if entries.is_empty() {
            return Ok(ToolOutput {
                content: serde_json::json!({ "results": Vec::<serde_json::Value>::new() }),
                is_error: false,
                duration: started.elapsed(),
            });
        }

        let query = args.query.trim();
        let results: Vec<serde_json::Value> = if query.is_empty() {
            alphabetical_results(entries, limit)
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

        Ok(ToolOutput {
            content: serde_json::json!({ "results": results }),
            is_error: false,
            duration: started.elapsed(),
        })
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
            .execute(&envelope_for(json!({"query": "specific message conversation"})), &ctx)
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

    #[tokio::test]
    async fn missing_catalog_returns_execution_failed() {
        let tool = ToolSearchTool::new();
        let ctx = ToolContext::empty();
        let err = tool
            .execute(&envelope_for(json!({"query": "x"})), &ctx)
            .await
            .expect_err("no catalog");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }
}
