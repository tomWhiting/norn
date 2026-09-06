//! One owning persistent Claude SDK query, event receiver and descriptor permit.

use std::collections::BTreeMap;
use std::process::ExitStatus;
use std::sync::Arc;

use claude_runner::control::{
    ElicitationResult, McpPermissionModeOverride, PermissionResult, SDKInitializeOptions,
    UserDialogResult, UserMessage,
};
use claude_runner::query::{
    ContextUsage, ExperimentalUsage, McpPermissionModeOverrideResult, McpServerConfig,
    McpServerStatus, McpSetServersResult,
};
use claude_runner::{
    ClaudeEvent, InitializationResult, InterruptOptions, InterruptReceipt, Query,
    QueryCapabilities, ThinkingDisplayUpdate,
};

use crate::resource::{DescriptorGovernor, DescriptorPermit, TWO_PIPE_SPAWN_PEAK};

use super::events::claude_session_id_of;
use super::{NornWrappedClaudeConfig, NornWrappedClaudeControl, NornWrappedClaudeError};

const SDK_QUERY_RETAINED_DESCRIPTORS: u32 = 2;

/// One persistent, bidirectional Norn-wrapped Claude Code Agent SDK session.
///
/// The session owns the Claude subprocess, correlated control dispatcher, and
/// Norn descriptor admission for the two retained stdin/stdout pipe handles.
pub struct NornWrappedClaudeSession {
    query: Query,
    descriptor_permit: DescriptorPermit,
    claude_session_id: Option<String>,
}

impl NornWrappedClaudeSession {
    pub(super) async fn spawn(
        config: &NornWrappedClaudeConfig,
        options: SDKInitializeOptions,
    ) -> Result<Self, NornWrappedClaudeError> {
        let governor = DescriptorGovernor::global()?;
        Self::spawn_with_governor(config, options, governor).await
    }

    pub(super) async fn spawn_with_governor(
        config: &NornWrappedClaudeConfig,
        mut options: SDKInitializeOptions,
        governor: Arc<DescriptorGovernor>,
    ) -> Result<Self, NornWrappedClaudeError> {
        tokio::runtime::Handle::try_current()
            .map_err(NornWrappedClaudeError::RuntimeUnavailable)?;
        let command = config.build_sdk_command()?;
        let mut spawn_permit = governor.try_acquire(TWO_PIPE_SPAWN_PEAK)?;
        let query = Query::spawn(&command)?;
        let descriptor_permit = spawn_permit.split(SDK_QUERY_RETAINED_DESCRIPTORS).ok_or(
            NornWrappedClaudeError::DescriptorAdmissionInvariant {
                retained: SDK_QUERY_RETAINED_DESCRIPTORS,
                peak: TWO_PIPE_SPAWN_PEAK,
            },
        )?;
        drop(spawn_permit);

        let session = Self {
            query,
            descriptor_permit,
            claude_session_id: None,
        };
        options.system_prompt = Some(vec![config.system_prompt.clone()]);
        session.query.initialize(options).await?;
        Ok(session)
    }

    /// Return the operating-system process identifier.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.query.id()
    }

    /// Return the first Claude session identifier observed on the event stream.
    #[must_use]
    pub fn claude_session_id(&self) -> Option<&str> {
        self.claude_session_id.as_deref()
    }

    /// Return the retained descriptor admission weight.
    #[must_use]
    pub fn retained_descriptor_count(&self) -> usize {
        self.descriptor_permit.weight()
    }

    /// Clone a control plane that remains usable while this session is
    /// mutably polling its event stream.
    #[must_use]
    pub fn control_handle(&self) -> NornWrappedClaudeControl {
        NornWrappedClaudeControl {
            query: self.query.control_handle(),
        }
    }

    /// Send a plain-text user message without ending the session.
    ///
    /// # Errors
    ///
    /// Returns a typed query transport or state error.
    pub fn send_user_message(&self, content: &str) -> Result<(), NornWrappedClaudeError> {
        self.query.send_user_message(content)?;
        Ok(())
    }

    /// Send a typed rich user message without ending the session.
    ///
    /// # Errors
    ///
    /// Returns a typed serialization, transport, or state error.
    pub fn send_typed_user_message(
        &self,
        message: &UserMessage,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.send_typed_user_message(message)?;
        Ok(())
    }

    /// Read the next ordinary SDK event in wire order.
    ///
    /// Correlated control responses remain routed to the method that issued
    /// them. `Ok(None)` means the process event stream reached EOF.
    ///
    /// # Errors
    ///
    /// Returns a typed event decode or stream transport error.
    pub async fn next_event(&mut self) -> Result<Option<ClaudeEvent>, NornWrappedClaudeError> {
        match self.query.next_event().await {
            Some(Ok(event)) => {
                if self.claude_session_id.is_none() {
                    self.claude_session_id = claude_session_id_of(&event).map(str::to_owned);
                }
                Ok(Some(event))
            }
            Some(Err(error)) => Err(error.into()),
            None => Ok(None),
        }
    }

    /// Repeat the SDK initialize handshake and replay pending interactions.
    ///
    /// # Errors
    ///
    /// Returns a typed control, protocol, or transport error.
    pub async fn reinitialize(&self) -> Result<InitializationResult, NornWrappedClaudeError> {
        Ok(self.query.reinitialize().await?)
    }

    /// Return the latest successful SDK initialization result.
    ///
    /// # Errors
    ///
    /// Returns an error if the query state was poisoned.
    pub fn initialization_result(
        &self,
    ) -> Result<Option<InitializationResult>, NornWrappedClaudeError> {
        Ok(self.query.initialization_result()?)
    }

    /// Return the protocol capabilities observed on the init event.
    ///
    /// # Errors
    ///
    /// Returns an error if the query state was poisoned.
    pub fn capabilities(&self) -> Result<QueryCapabilities, NornWrappedClaudeError> {
        Ok(self.query.capabilities()?)
    }

    /// Answer one pending Claude tool-permission request.
    ///
    /// # Errors
    ///
    /// Returns a typed lifecycle, serialization, or transport error.
    pub fn respond_permission(
        &self,
        request_id: &str,
        result: PermissionResult,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.respond_permission(request_id, result)?;
        Ok(())
    }

    /// Answer one pending host-rendered user dialog.
    ///
    /// # Errors
    ///
    /// Returns a typed lifecycle, serialization, or transport error.
    pub fn respond_user_dialog(
        &self,
        request_id: &str,
        result: UserDialogResult,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.respond_user_dialog(request_id, result)?;
        Ok(())
    }

    /// Answer one pending MCP elicitation.
    ///
    /// # Errors
    ///
    /// Returns a typed lifecycle, serialization, or transport error.
    pub fn respond_elicitation(
        &self,
        request_id: &str,
        result: ElicitationResult,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.respond_elicitation(request_id, result)?;
        Ok(())
    }

    /// Interrupt the active turn while retaining queued messages.
    ///
    /// # Errors
    ///
    /// Returns a typed control, capability, protocol, or transport error.
    pub async fn interrupt(&self) -> Result<Option<InterruptReceipt>, NornWrappedClaudeError> {
        Ok(self.query.interrupt().await?)
    }

    /// Interrupt the active turn with explicit queued-message behavior.
    ///
    /// # Errors
    ///
    /// Returns a typed control, capability, protocol, or transport error.
    pub async fn interrupt_with_options(
        &self,
        options: InterruptOptions,
    ) -> Result<Option<InterruptReceipt>, NornWrappedClaudeError> {
        Ok(self.query.interrupt_with_options(options).await?)
    }

    /// Atomically interrupt the turn and cancel queued messages.
    ///
    /// # Errors
    ///
    /// Returns a typed control, capability, protocol, or transport error.
    pub async fn interrupt_and_cancel_queued(
        &self,
    ) -> Result<InterruptReceipt, NornWrappedClaudeError> {
        Ok(self.query.interrupt_and_cancel_queued().await?)
    }

    /// Fetch the current context-window usage breakdown.
    ///
    /// # Errors
    ///
    /// Returns a typed control, response-shape, or transport error.
    pub async fn get_context_usage(&self) -> Result<ContextUsage, NornWrappedClaudeError> {
        Ok(self.query.get_context_usage().await?)
    }

    /// Fetch the upstream experimental session and plan-limit usage payload.
    ///
    /// # Errors
    ///
    /// Returns a typed control, response-shape, or transport error.
    pub async fn usage_experimental_may_change(
        &self,
    ) -> Result<ExperimentalUsage, NornWrappedClaudeError> {
        Ok(self.query.usage_experimental_may_change().await?)
    }

    /// Change the permission mode for subsequent tool execution.
    ///
    /// # Errors
    ///
    /// Returns a typed control or transport error.
    pub async fn set_permission_mode(
        &self,
        mode: claude_runner::PermissionMode,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.set_permission_mode(mode).await?;
        Ok(())
    }

    /// Change the model for subsequent turns.
    ///
    /// # Errors
    ///
    /// Returns a typed control or transport error.
    pub async fn set_model(&self, model: impl Into<String>) -> Result<(), NornWrappedClaudeError> {
        self.query.set_model(model).await?;
        Ok(())
    }

    /// Restore the session's initial model selection.
    ///
    /// # Errors
    ///
    /// Returns a typed control or transport error.
    pub async fn reset_model(&self) -> Result<(), NornWrappedClaudeError> {
        self.query.reset_model().await?;
        Ok(())
    }

    /// Set or clear the maximum thinking-token count and display mode.
    ///
    /// # Errors
    ///
    /// Returns a typed control or transport error.
    pub async fn set_max_thinking_tokens(
        &self,
        max_thinking_tokens: Option<u64>,
        thinking_display: ThinkingDisplayUpdate,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query
            .set_max_thinking_tokens(max_thinking_tokens, thinking_display)
            .await?;
        Ok(())
    }

    /// Shallow-merge in-memory Claude Code flag settings.
    ///
    /// # Errors
    ///
    /// Returns a typed control, serialization, or transport error.
    pub async fn apply_flag_settings(
        &self,
        settings: BTreeMap<String, serde_json::Value>,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.apply_flag_settings(settings).await?;
        Ok(())
    }

    /// Change the output style for subsequent turns.
    ///
    /// # Errors
    ///
    /// Returns a typed control or transport error.
    pub async fn set_output_style(
        &self,
        output_style: impl Into<String>,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.set_output_style(output_style).await?;
        Ok(())
    }

    /// Return current status for every configured MCP server.
    ///
    /// # Errors
    ///
    /// Returns a typed control, response-shape, or transport error.
    pub async fn mcp_server_status(&self) -> Result<Vec<McpServerStatus>, NornWrappedClaudeError> {
        Ok(self.query.mcp_server_status().await?)
    }

    /// Reconnect one MCP server by exact configured name.
    ///
    /// # Errors
    ///
    /// Returns a typed control or transport error.
    pub async fn reconnect_mcp_server(
        &self,
        server_name: impl Into<String>,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.reconnect_mcp_server(server_name).await?;
        Ok(())
    }

    /// Enable or disable one MCP server.
    ///
    /// # Errors
    ///
    /// Returns a typed control or transport error.
    pub async fn toggle_mcp_server(
        &self,
        server_name: impl Into<String>,
        enabled: bool,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.toggle_mcp_server(server_name, enabled).await?;
        Ok(())
    }

    /// Replace the dynamically managed MCP server set.
    ///
    /// # Errors
    ///
    /// Returns a typed control, response-shape, serialization, or transport
    /// error.
    pub async fn set_mcp_servers(
        &self,
        servers: BTreeMap<String, McpServerConfig>,
    ) -> Result<McpSetServersResult, NornWrappedClaudeError> {
        Ok(self.query.set_mcp_servers(servers).await?)
    }

    /// Set or clear one MCP server's tighten-only permission override.
    ///
    /// # Errors
    ///
    /// Returns a typed control, response-shape, or transport error.
    pub async fn set_mcp_permission_mode_override(
        &self,
        server_name: impl Into<String>,
        mode: Option<McpPermissionModeOverride>,
    ) -> Result<McpPermissionModeOverrideResult, NornWrappedClaudeError> {
        Ok(self
            .query
            .set_mcp_permission_mode_override(server_name, mode)
            .await?)
    }

    /// Stop a running Claude task.
    ///
    /// # Errors
    ///
    /// Returns a typed control or transport error.
    pub async fn stop_task(
        &self,
        task_id: impl Into<String>,
    ) -> Result<(), NornWrappedClaudeError> {
        self.query.stop_task(task_id).await?;
        Ok(())
    }

    /// Move foreground tasks into the background.
    ///
    /// # Errors
    ///
    /// Returns a typed control, response-shape, or transport error.
    pub async fn background_tasks(
        &self,
        tool_use_id: Option<&str>,
    ) -> Result<bool, NornWrappedClaudeError> {
        Ok(self.query.background_tasks(tool_use_id).await?)
    }

    /// Cancel one UUID-addressed queued asynchronous message.
    ///
    /// # Errors
    ///
    /// Returns a typed control, response-shape, or transport error.
    pub async fn cancel_async_message(
        &self,
        message_uuid: impl Into<String>,
    ) -> Result<bool, NornWrappedClaudeError> {
        Ok(self.query.cancel_async_message(message_uuid).await?)
    }

    /// Close stdin and allow Claude Code to drain naturally.
    ///
    /// # Errors
    ///
    /// Returns a typed shared-state error.
    pub fn close_stdin(&self) -> Result<(), NornWrappedClaudeError> {
        self.query.close_stdin()?;
        Ok(())
    }

    /// Kill the Claude Code process immediately.
    ///
    /// Call [`wait`](Self::wait) afterward to reap the process.
    ///
    /// # Errors
    ///
    /// Returns a typed process or shared-state error.
    pub fn kill(&self) -> Result<(), NornWrappedClaudeError> {
        self.query.kill()?;
        Ok(())
    }

    /// Try to observe the child exit status without blocking.
    ///
    /// # Errors
    ///
    /// Returns a typed process or shared-state error.
    pub fn try_wait(&self) -> Result<Option<ExitStatus>, NornWrappedClaudeError> {
        Ok(self.query.try_wait()?)
    }

    /// Forcefully close the process transport.
    ///
    /// Call [`wait`](Self::wait) afterward to reap the process.
    ///
    /// # Errors
    ///
    /// Returns a typed process or shared-state error.
    pub fn close(&self) -> Result<(), NornWrappedClaudeError> {
        self.query.close()?;
        Ok(())
    }

    /// Close stdin, wait without an arbitrary timeout, and reap the process.
    ///
    /// Consume [`next_event`](Self::next_event) until EOF first when trailing
    /// SDK events must be preserved.
    ///
    /// # Errors
    ///
    /// Returns a typed process, task-join, or shared-state error.
    pub async fn wait(self) -> Result<ExitStatus, NornWrappedClaudeError> {
        let Self {
            query,
            descriptor_permit,
            claude_session_id: _,
        } = self;
        let status = query.wait().await?;
        drop(descriptor_permit);
        Ok(status)
    }
}
