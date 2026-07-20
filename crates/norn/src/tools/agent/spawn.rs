//! `SpawnAgentTool` (NA-006) — launches a sub-agent asynchronously.
//!
//! Spawn reserves a child slot in the agent registry, builds a per-child
//! [`ToolContext`] carrying the child's own identity, resolves an optional
//! profile into the child's [`crate::r#loop::loop_context::LoopContext`],
//! filters the parent registry's tool definitions through the allow-list so
//! the child model can see its
//! tools, then launches the child via [`tokio::spawn`] and returns
//! immediately. When the child reaches a terminal status the spawn wrapper
//! marks the registry, delivers the result on the child-result channel,
//! and updates the status watch channel that backs reactive waits.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use super::delegation::{
    auto_child_path, effective_child_tools, grant_child_policy, install_child_result_channel,
    resolve_spawner_policy,
};
use super::handle::{AgentHandles, AgentWakeRegistry, ChildBranchMetadata};
#[cfg(test)]
use super::infra::SubAgentExecutor;
use super::infra::{AgentCancellation, AgentModel, infra_from};
use super::lifecycle::LifecycleEmitter;
use super::reclaim::{ReclaimHandshake, ReclaimOnResultDelivery};
use super::spawn_context::{build_child_context, resolve_profile_root};
use super::spawn_launch::{ChildLaunch, launch_child};
use super::variant_resolve::{SpawnIdentityArgs, resolve_parent_model, resolve_spawn};
#[cfg(test)]
use crate::agent::child_policy::ChildPolicy;
use crate::agent::child_policy::{ChildLoopConfig, CoordinationEnvelope};
use crate::agent::fork::ParentSystemInstruction;
use crate::agent::registry::AgentRegistry;
use crate::agent::result_channel::ChildResultSender;
use crate::agent::variants::VariantCatalog;
use crate::error::ToolError;
use crate::integration::hooks::HookRegistry;
#[cfg(test)]
use crate::r#loop::loop_context::LoopContext;
use crate::provider::agent_event::{AgentEventSender, SubagentDescriptor, SubagentKind};
use crate::session::action_log::ActionLog;
use crate::session::events::ChildBranchKind;
use crate::session::{ChildBranchRequest, slugify_name_stem};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
#[cfg(test)]
use crate::tool::registry::ToolRegistry;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

mod config;
mod execute;

#[cfg(test)]
use self::config::build_tool_definitions;
use self::config::{SpawnAgentArgs, build_child_loop_context};

/// Spawns a sub-agent that runs asynchronously on its own `tokio` task.
pub struct SpawnAgentTool;

impl SpawnAgentTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SpawnAgentTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Public tool name for the Norn spawn delegation tool.
pub const SPAWN_TOOL_NAME: &str = "spawn_agent";

#[async_trait]
impl Tool for SpawnAgentTool {
    fn name(&self) -> &'static str {
        SPAWN_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/spawn_agent.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/spawn_agent.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        super::spawn_schema::input_schema()
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Process
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        execute::execute(envelope, ctx).await
    }
}

#[cfg(test)]
#[path = "spawn/tests/mod.rs"]
mod tests;
