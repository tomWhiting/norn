//! Default initialization for [`AgentBuilder`](super::AgentBuilder).

use std::sync::Arc;

use crate::agent::assembly::AgentConfigPresence;
use crate::agent::mcp::McpAttachment;
use crate::agent_loop::config::AgentLoopConfig;
use crate::provider::traits::Provider;
use crate::system_prompt::builder::ExecutionMode;

use super::AgentBuilder;

impl AgentBuilder {
    /// Start a builder for the given provider. Every other field is optional.
    #[must_use]
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            provider,
            profile: None,
            profile_origin: None,
            profile_name: None,
            model: None,
            system_prompt: None,
            append_system_prompt: None,
            reasoning_effort: None,
            service_tier: None,
            capabilities: Vec::new(),
            working_dir: None,
            workspace_root: None,
            bash_drain_grace: None,
            allowed_tools: None,
            extra_tools: Vec::new(),
            without_tools: Vec::new(),
            lsp_backend: None,
            lsp_workspace: None,
            execution_mode: ExecutionMode::Headless,
            agent_config: AgentLoopConfig::default(),
            agent_config_present: AgentConfigPresence::default(),
            retry_policy: None,
            session: None,
            session_request: None,
            event_channel_capacity: None,
            cancel: None,
            inbound_capacity: None,
            inbound: None,
            inbound_tx: None,
            agent_id: None,
            hooks: None,
            rules: None,
            diagnostics: None,
            diagnostic_infra: None,
            additional_post_checks: Vec::new(),
            agent_registry: None,
            child_policy: None,
            child_result_capacity: None,
            extensions: Vec::new(),
            load_runtime_base: false,
            task_group_slug: None,
            event_schemas: None,
            variables: None,
            variable_pairs: Vec::new(),
            disallowed_tools: Vec::new(),
            mcp: McpAttachment::default(),
            terminal_reclamation: true,
            register_root: None,
        }
    }
}
