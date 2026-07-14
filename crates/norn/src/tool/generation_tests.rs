use std::error::Error;
use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{ToolGeneration, ToolGenerationStore};
use crate::error::ToolError;
use crate::r#loop::config::ToolExecutor;
use crate::tool::catalog::{SharedToolCatalog, ToolCatalogEntry, ToolCatalogExtras};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::registry::ToolRegistry;
use crate::tool::scheduling::{ToolEffect, ToolEffectIndex};
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::tool_search::ToolSearchTool;

type TestResult = Result<(), Box<dyn Error>>;

struct StubTool {
    name: String,
    effect: ToolEffect,
    dynamic: bool,
}

impl StubTool {
    fn new(name: &str, effect: ToolEffect, dynamic: bool) -> Self {
        Self {
            name: name.to_owned(),
            effect,
            dynamic,
        }
    }
}

#[async_trait]
impl Tool for StubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &'static str {
        "generation test tool"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        self.effect
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::General
    }

    fn runtime_dynamic(&self) -> bool {
        self.dynamic
    }

    async fn execute(
        &self,
        _envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::success(json!({ "tool": self.name })))
    }
}

fn missing(message: &str) -> io::Error {
    io::Error::other(message.to_owned())
}

fn registry_with_tools(tools: &[(&str, ToolEffect, bool)]) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for (name, effect, dynamic) in tools {
        registry.register(Box::new(StubTool::new(name, *effect, *dynamic)));
    }
    registry
}

#[test]
fn generation_projects_one_deterministic_available_view() {
    let mut registry = registry_with_tools(&[
        ("zulu", ToolEffect::Write, false),
        ("alpha", ToolEffect::ReadOnly, false),
        ("mcp_live", ToolEffect::Unknown, true),
    ]);
    registry.set_available(vec![
        "mcp_live".to_owned(),
        "zulu".to_owned(),
        "alpha".to_owned(),
    ]);
    registry.set_disallowed(vec!["zulu".to_owned()]);
    registry
        .context_arc()
        .insert_extension(Arc::new(ToolCatalogExtras(vec![ToolCatalogEntry::tool(
            "external_hint",
            "Extra catalog metadata",
        )])));

    let generation = ToolGeneration::from_registry(&registry, 7);
    let names: Vec<&str> = generation.names().collect();
    let definitions = generation.definitions();
    let definition_names: Vec<&str> = definitions.iter().map(|tool| tool.name.as_str()).collect();
    let prompt_entries = generation.prompt_entries();
    let prompt_names: Vec<&str> = prompt_entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    let dynamic_entries = generation.dynamic_prompt_entries();
    let dynamic_names: Vec<&str> = dynamic_entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();

    assert_eq!(generation.revision(), 7);
    assert_eq!(names, ["alpha", "mcp_live"]);
    assert_eq!(definition_names, names);
    assert_eq!(prompt_names, names);
    assert_eq!(dynamic_names, ["mcp_live"]);
    assert!(
        generation
            .catalog()
            .iter()
            .any(|entry| entry.name == "external_hint")
    );
    assert!(Arc::ptr_eq(&generation.context(), &registry.context_arc()));
}

#[tokio::test]
async fn bound_tool_search_cannot_observe_newer_context_catalog() -> TestResult {
    let mut registry = registry_with_tools(&[("old_tool", ToolEffect::ReadOnly, false)]);
    registry.register(Box::new(ToolSearchTool::new()));
    let generation = ToolGeneration::from_registry(&registry, 1);

    generation
        .context()
        .insert_extension(Arc::new(SharedToolCatalog(Arc::new(vec![
            ToolCatalogEntry::tool("new_tool", "Only in the newer shared catalog"),
        ]))));

    let output = generation
        .execute(
            "tool_search",
            "search-call",
            json!({ "query": "", "max_results": 500 }),
        )
        .await?;
    let results = output
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("tool_search output must contain a results array"))?;
    let names: Vec<&str> = results
        .iter()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .collect();

    assert!(names.contains(&"old_tool"));
    assert!(!names.contains(&"new_tool"));
    Ok(())
}

#[tokio::test]
async fn published_generation_does_not_change_existing_request_lease() -> TestResult {
    let mut registry = registry_with_tools(&[("old_tool", ToolEffect::ReadOnly, false)]);
    let first = Arc::new(ToolGeneration::from_registry(&registry, 1));
    let store = ToolGenerationStore::new(Arc::clone(&first));
    let old_lease = store
        .execution_snapshot()
        .ok_or_else(|| missing("generation store must provide an execution snapshot"))?;

    registry.remove("old_tool");
    registry.register(Box::new(StubTool::new(
        "new_tool",
        ToolEffect::Write,
        false,
    )));
    let second = Arc::new(ToolGeneration::from_registry(&registry, 2));
    let replaced = store.publish(Arc::clone(&second))?;

    assert!(Arc::ptr_eq(&first, &replaced));
    assert_eq!(old_lease.revision, 1);
    assert_eq!(store.snapshot().revision(), 2);
    assert_eq!(
        old_lease
            .executor
            .execute("old_tool", "old-call", json!({}))
            .await?["tool"],
        "old_tool",
    );
    assert_eq!(
        store.execute("new_tool", "new-call", json!({})).await?["tool"],
        "new_tool",
    );
    assert!(
        store
            .execute("old_tool", "stale-call", json!({}))
            .await
            .is_err()
    );
    assert!(store.publish(second).is_err());
    Ok(())
}

#[test]
fn store_rejects_a_generation_from_another_context() {
    let first_registry = registry_with_tools(&[("first", ToolEffect::ReadOnly, false)]);
    let other_registry = registry_with_tools(&[("other", ToolEffect::ReadOnly, false)]);
    let first = Arc::new(ToolGeneration::from_registry(&first_registry, 1));
    let other = Arc::new(ToolGeneration::from_registry(&other_registry, 2));
    let store = ToolGenerationStore::new(Arc::clone(&first));

    assert!(store.publish(other).is_err());
    assert!(Arc::ptr_eq(&store.snapshot(), &first));
    assert!(ToolExecutor::effect_index(&store).is_none());
}

#[test]
fn effect_indexes_are_pinned_to_their_generation() {
    let mut registry = registry_with_tools(&[("switch", ToolEffect::ReadOnly, false)]);
    let first = ToolGeneration::from_registry(&registry, 1);
    registry.register(Box::new(StubTool::new(
        "switch",
        ToolEffect::RemoteMutation,
        false,
    )));
    let second = ToolGeneration::from_registry(&registry, 2);
    let args = json!({});

    assert_eq!(
        ToolEffectIndex::effect_for(&first.effect_index(), "switch", &args),
        ToolEffect::ReadOnly,
    );
    assert_eq!(
        ToolEffectIndex::effect_for(&second.effect_index(), "switch", &args),
        ToolEffect::RemoteMutation,
    );
}
