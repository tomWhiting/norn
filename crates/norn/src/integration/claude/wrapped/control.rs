//! Concurrent controls for the persistent Claude process without event ownership.

use std::process::ExitStatus;

use claude_runner::{InterruptOptions, InterruptReceipt, QueryCapabilities, QueryControl};

use super::NornWrappedClaudeError;

/// Cloneable concurrent control plane for a live
/// [`NornWrappedClaudeSession`](super::NornWrappedClaudeSession).
///
/// This handle does not consume SDK events or own final process reaping. It
/// can therefore interrupt, escalate, or observe a process from one task while
/// another task remains blocked in
/// [`NornWrappedClaudeSession::next_event`](super::NornWrappedClaudeSession::next_event).
#[derive(Clone)]
pub struct NornWrappedClaudeControl {
    pub(super) query: QueryControl,
}

impl NornWrappedClaudeControl {
    /// Return the operating-system process identifier.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.query.id()
    }

    /// Send a plain-text user message through the concurrent control plane.
    ///
    /// # Errors
    ///
    /// Returns a typed transport or shared-state error.
    pub fn send_user_message(&self, content: &str) -> Result<(), NornWrappedClaudeError> {
        self.query.send_user_message(content)?;
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

    /// Return the protocol capabilities observed on the init event.
    ///
    /// # Errors
    ///
    /// Returns an error if the shared query state was poisoned.
    pub fn capabilities(&self) -> Result<QueryCapabilities, NornWrappedClaudeError> {
        Ok(self.query.capabilities()?)
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

    /// Kill the Claude Code process immediately without consuming final
    /// process ownership.
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

    /// Wait for process exit without consuming the session's final reap owner.
    ///
    /// This remains independently selectable with a concurrent kill future.
    ///
    /// # Errors
    ///
    /// Returns a typed process or shared-state error.
    pub async fn wait_for_exit(&self) -> Result<ExitStatus, NornWrappedClaudeError> {
        Ok(self.query.wait_for_exit().await?)
    }
}
