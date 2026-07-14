//! Immutable, atomically published tool generations.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::Value;

use super::catalog::{ToolCatalogEntry, ToolCatalogExtras};
use super::context::ToolContext;
use super::envelope::{ToolEnvelope, split_envelope_fields};
use super::registry::{ToolRegistry, dispatch_tool_with_outcome};
use super::scheduling::ToolEffectIndex;
use super::traits::Tool;
use crate::error::ToolError;
use crate::r#loop::config::{DispatchOutcome, ToolExecutionSnapshot, ToolExecutor};
use crate::provider::request::ToolDefinition;
use crate::provider::surface::function_definition_for_tool;
use crate::system_prompt::builder::ToolPromptEntry;
use crate::tools::tool_search::{TOOL_SEARCH_TOOL_NAME, ToolSearchTool};

/// One immutable executable and model-facing view of the available tools.
pub struct ToolGeneration {
    revision: u64,
    tools: BTreeMap<String, Arc<dyn Tool + Send + Sync>>,
    context: Arc<ToolContext>,
    effects: Arc<ToolEffectIndex>,
    definitions: Arc<[ToolDefinition]>,
    catalog: Arc<[ToolCatalogEntry]>,
    prompt_entries: Arc<[ToolPromptEntry]>,
    dynamic_prompt_entries: Arc<[ToolPromptEntry]>,
}

impl ToolGeneration {
    /// Snapshot the registry's currently available tools at `revision`.
    #[must_use]
    pub fn from_registry(registry: &ToolRegistry, revision: u64) -> Self {
        Self::assemble(
            registry.available_tool_arcs(),
            registry.context_arc(),
            revision,
        )
    }

    /// Replace every runtime-dynamic tool while preserving the stable surface.
    ///
    /// The new generation retains the exact context and stable tool instances
    /// from `previous`. All model-facing projections are then rebuilt together.
    ///
    /// # Errors
    ///
    /// Returns an error when a dynamic tool collides with a stable name or two
    /// dynamic tools resolve to the same provider-facing name.
    pub fn replacing_dynamic_tools(
        previous: &Self,
        dynamic_tools: Vec<Box<dyn Tool + Send + Sync>>,
        revision: u64,
    ) -> Result<Self, ToolGenerationBuildError> {
        let mut tools: BTreeMap<String, Arc<dyn Tool + Send + Sync>> = previous
            .tools
            .iter()
            .filter(|(_name, tool)| !tool.runtime_dynamic())
            .map(|(name, tool)| (name.clone(), Arc::clone(tool)))
            .collect();
        let mut dynamic_names = std::collections::BTreeSet::new();
        for tool in dynamic_tools {
            let name = tool.name().to_owned();
            if !dynamic_names.insert(name.clone()) {
                return Err(ToolGenerationBuildError::DuplicateDynamicName { name });
            }
            if tools.contains_key(&name) {
                return Err(ToolGenerationBuildError::StableNameCollision { name });
            }
            tools.insert(name, Arc::from(tool));
        }
        Ok(Self::assemble(
            tools,
            Arc::clone(&previous.context),
            revision,
        ))
    }

    /// Remove an explicit subset of runtime-dynamic tools from a generation.
    ///
    /// Every tool outside `removed` retains its exact implementation `Arc`.
    /// Stable tools are never removed, even if their names appear in the set.
    /// This preserves a narrowed provider view instead of reconstructing it
    /// from a broader runtime client pool.
    #[must_use]
    pub(crate) fn removing_dynamic_tools(
        previous: &Self,
        removed: &BTreeSet<String>,
        revision: u64,
    ) -> Self {
        let tools = previous
            .tools
            .iter()
            .filter(|(name, tool)| !(tool.runtime_dynamic() && removed.contains(*name)))
            .map(|(name, tool)| (name.clone(), Arc::clone(tool)))
            .collect();
        Self::assemble(tools, Arc::clone(&previous.context), revision)
    }

    /// Derive a child-context view while retaining source tool instances.
    pub(crate) fn child_view(
        source: &Self,
        dynamic_tools: Option<Vec<Box<dyn Tool + Send + Sync>>>,
        available: Option<&std::collections::BTreeSet<String>>,
        context: Arc<ToolContext>,
    ) -> Result<Self, ToolGenerationBuildError> {
        let mut tools: BTreeMap<String, Arc<dyn Tool + Send + Sync>> = source
            .tools
            .iter()
            .filter(|(_name, tool)| dynamic_tools.is_none() || !tool.runtime_dynamic())
            .map(|(name, tool)| (name.clone(), Arc::clone(tool)))
            .collect();
        if let Some(dynamic_tools) = dynamic_tools {
            let mut dynamic_names = std::collections::BTreeSet::new();
            for tool in dynamic_tools {
                let name = tool.name().to_owned();
                if !dynamic_names.insert(name.clone()) {
                    return Err(ToolGenerationBuildError::DuplicateDynamicName { name });
                }
                if tools.contains_key(&name) {
                    return Err(ToolGenerationBuildError::StableNameCollision { name });
                }
                tools.insert(name, Arc::from(tool));
            }
        }
        if let Some(available) = available {
            tools.retain(|name, _tool| available.contains(name));
        }
        Ok(Self::assemble(tools, context, source.revision))
    }

    fn assemble(
        mut tools: BTreeMap<String, Arc<dyn Tool + Send + Sync>>,
        context: Arc<ToolContext>,
        revision: u64,
    ) -> Self {
        let mut catalog: Vec<ToolCatalogEntry> = tools
            .values()
            .flat_map(|tool| tool.catalog_entries())
            .collect();
        if let Some(extras) = context.get_extension::<ToolCatalogExtras>() {
            catalog.extend(extras.0.iter().cloned());
        }
        let catalog: Arc<[ToolCatalogEntry]> = Arc::from(catalog);

        if tools.contains_key(TOOL_SEARCH_TOOL_NAME) {
            let search: Arc<dyn Tool + Send + Sync> =
                Arc::new(ToolSearchTool::with_bound_catalog(Arc::clone(&catalog)));
            tools.insert(TOOL_SEARCH_TOOL_NAME.to_owned(), search);
        }

        let effects = Arc::new(ToolEffectIndex::new());
        for (name, tool) in &tools {
            effects.insert(name.clone(), Arc::clone(tool));
        }

        let definitions: Arc<[ToolDefinition]> = Arc::from(
            tools
                .values()
                .map(|tool| function_definition_for_tool(tool.as_ref()))
                .collect::<Vec<_>>(),
        );
        let prompt_entries: Vec<ToolPromptEntry> = tools
            .values()
            .map(|tool| prompt_entry(tool.as_ref()))
            .collect();
        let dynamic_prompt_entries: Arc<[ToolPromptEntry]> = Arc::from(
            tools
                .values()
                .filter(|tool| tool.runtime_dynamic())
                .map(|tool| prompt_entry(tool.as_ref()))
                .collect::<Vec<_>>(),
        );

        Self {
            revision,
            tools,
            context,
            effects,
            definitions,
            catalog,
            prompt_entries: Arc::from(prompt_entries),
            dynamic_prompt_entries,
        }
    }

    /// Monotonic identifier for this generation.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Provider function definitions in deterministic name order.
    #[must_use]
    pub fn definitions(&self) -> Arc<[ToolDefinition]> {
        Arc::clone(&self.definitions)
    }

    /// Provider-blind discovery catalog for exactly this generation.
    #[must_use]
    pub fn catalog(&self) -> Arc<[ToolCatalogEntry]> {
        Arc::clone(&self.catalog)
    }

    /// Prompt metadata for every tool in deterministic name order.
    #[must_use]
    pub fn prompt_entries(&self) -> Arc<[ToolPromptEntry]> {
        Arc::clone(&self.prompt_entries)
    }

    /// Prompt metadata for runtime-dynamic tools only.
    #[must_use]
    pub fn dynamic_prompt_entries(&self) -> Arc<[ToolPromptEntry]> {
        Arc::clone(&self.dynamic_prompt_entries)
    }

    /// Pin this generation as one provider-request execution lease.
    pub(crate) fn execution_lease(generation: &Arc<Self>) -> ToolExecutionSnapshot {
        ToolExecutionSnapshot {
            revision: generation.revision,
            executor: Arc::clone(generation) as Arc<dyn ToolExecutor>,
            definitions: Arc::clone(&generation.definitions),
            dynamic_prompt_entries: Arc::clone(&generation.dynamic_prompt_entries),
        }
    }

    /// Context shared with the source registry and every generation.
    #[must_use]
    pub fn context(&self) -> Arc<ToolContext> {
        Arc::clone(&self.context)
    }

    /// Scheduling metadata pinned to this generation's implementations.
    #[must_use]
    pub fn effect_index(&self) -> Arc<ToolEffectIndex> {
        Arc::clone(&self.effects)
    }

    /// Available names in deterministic order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    fn prepare_dispatch(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<(&(dyn Tool + Send + Sync), ToolEnvelope), ToolError> {
        let tool =
            self.tools
                .get(name)
                .map(AsRef::as_ref)
                .ok_or_else(|| ToolError::ToolNotFound {
                    name: name.to_owned(),
                })?;
        let split = split_envelope_fields(arguments);
        if let Some(description) = split.description {
            tracing::debug!(
                tool = name,
                call_id,
                %description,
                generation = self.revision,
                "tool_use_description on generation dispatch",
            );
        }
        Ok((
            tool,
            ToolEnvelope {
                tool_call_id: call_id.to_owned(),
                tool_name: name.to_owned(),
                model_args: split.tool_args,
                metadata: split.metadata,
            },
        ))
    }
}

/// Invalid composition of a live tool generation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ToolGenerationBuildError {
    /// A runtime tool attempted to replace a stable tool.
    #[error("runtime tool '{name}' collides with a stable tool")]
    StableNameCollision {
        /// Colliding provider-facing name.
        name: String,
    },
    /// Multiple runtime tools resolved to one provider-facing name.
    #[error("multiple runtime tools resolve to provider name '{name}'")]
    DuplicateDynamicName {
        /// Duplicated provider-facing name.
        name: String,
    },
}

#[async_trait]
impl ToolExecutor for ToolGeneration {
    async fn execute(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        self.execute_with_outcome(name, call_id, arguments)
            .await
            .map(|outcome| outcome.content)
    }

    async fn execute_with_outcome(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<DispatchOutcome, ToolError> {
        let (tool, envelope) = self.prepare_dispatch(name, call_id, arguments)?;
        dispatch_tool_with_outcome(tool, &envelope, self.context.as_ref()).await
    }

    fn shared_context(&self) -> Option<Arc<ToolContext>> {
        Some(Arc::clone(&self.context))
    }

    fn effect_index(&self) -> Option<Arc<ToolEffectIndex>> {
        Some(Arc::clone(&self.effects))
    }
}

/// Rejected publication that would violate store generation invariants.
#[derive(Debug, thiserror::Error)]
pub enum ToolGenerationPublishError {
    /// The proposed revision does not advance the current revision.
    #[error("tool generation revision {proposed} must be newer than current revision {current}")]
    StaleRevision {
        /// Currently published revision.
        current: u64,
        /// Rejected revision.
        proposed: u64,
    },
    /// The proposed generation belongs to a different runtime context.
    #[error("tool generation publication cannot replace the shared tool context")]
    ContextChanged,
}

/// Atomically publishes immutable tool generations to request leases.
pub struct ToolGenerationStore {
    current: RwLock<Arc<ToolGeneration>>,
}

impl ToolGenerationStore {
    /// Create a store holding `initial`.
    #[must_use]
    pub fn new(initial: Arc<ToolGeneration>) -> Self {
        Self {
            current: RwLock::new(initial),
        }
    }

    /// Create the initial revision from an assembled registry.
    #[must_use]
    pub(crate) fn from_registry(registry: &ToolRegistry) -> Self {
        Self::new(Arc::new(ToolGeneration::from_registry(registry, 0)))
    }

    /// Capture the current generation with one read-lock acquisition.
    #[must_use]
    pub fn snapshot(&self) -> Arc<ToolGeneration> {
        Arc::clone(&self.current.read())
    }

    /// Atomically publish a strictly newer generation.
    ///
    /// # Errors
    ///
    /// Returns [`ToolGenerationPublishError`] when `next` would repeat or
    /// move backwards from the current revision.
    pub fn publish(
        &self,
        next: Arc<ToolGeneration>,
    ) -> Result<Arc<ToolGeneration>, ToolGenerationPublishError> {
        let mut current = self.current.write();
        if next.revision <= current.revision {
            return Err(ToolGenerationPublishError::StaleRevision {
                current: current.revision,
                proposed: next.revision,
            });
        }
        if !Arc::ptr_eq(&next.context, &current.context) {
            return Err(ToolGenerationPublishError::ContextChanged);
        }
        Ok(std::mem::replace(&mut current, next))
    }

    fn execution_lease(&self) -> ToolExecutionSnapshot {
        ToolGeneration::execution_lease(&self.snapshot())
    }
}

#[async_trait]
impl ToolExecutor for ToolGenerationStore {
    async fn execute(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        self.snapshot().execute(name, call_id, arguments).await
    }

    async fn execute_with_outcome(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<DispatchOutcome, ToolError> {
        self.snapshot()
            .execute_with_outcome(name, call_id, arguments)
            .await
    }

    fn shared_context(&self) -> Option<Arc<ToolContext>> {
        Some(self.snapshot().context())
    }

    fn effect_index(&self) -> Option<Arc<ToolEffectIndex>> {
        // A caller that bypasses `execution_snapshot` could otherwise read
        // one generation's index and dispatch through another after publish.
        // No index means conservative `Unknown` scheduling on that fallback.
        None
    }

    fn execution_snapshot(&self) -> Option<ToolExecutionSnapshot> {
        Some(self.execution_lease())
    }
}

fn prompt_entry(tool: &(dyn Tool + Send + Sync)) -> ToolPromptEntry {
    ToolPromptEntry {
        name: tool.name().to_owned(),
        category: tool.category(),
        description: tool.description().to_owned(),
        usage_guidance: tool.usage_guidance().map(str::to_owned),
    }
}

#[cfg(test)]
#[path = "generation_tests.rs"]
mod tests;
