//! Configuration owner for explicit one-shot and persistent Claude execution modes.

use std::sync::Arc;

use claude_runner::{ClaudeCommand, ClaudeEvent};

use crate::error::IntegrationError;
use crate::session::events::SessionEvent;

use super::super::adapter::spawn_and_collect;
use super::events::capture_session_events;
use super::{NornWrappedClaudeConfig, NornWrappedClaudeError, NornWrappedClaudeSession};

/// Configuration owner for Norn-wrapped Claude Code execution.
///
/// [`spawn_session`](Self::spawn_session) creates a persistent SDK session;
/// [`run`](Self::run) executes one prompt and returns its captured events.
pub struct NornWrappedClaudeCode {
    pub(super) config: NornWrappedClaudeConfig,
    claude_session_id: Arc<parking_lot::Mutex<Option<String>>>,
}

impl NornWrappedClaudeCode {
    /// Construct a wrapper from configuration.
    #[must_use]
    pub fn new(config: NornWrappedClaudeConfig) -> Self {
        Self {
            config,
            claude_session_id: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Return the last Claude session identifier observed by a one-shot call.
    #[must_use]
    pub fn claude_session_id(&self) -> Option<String> {
        self.claude_session_id.lock().clone()
    }

    /// Spawn and initialize a persistent bidirectional Agent SDK session.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration, admission, process, or protocol error.
    pub async fn spawn_session(&self) -> Result<NornWrappedClaudeSession, NornWrappedClaudeError> {
        self.spawn_session_with_options(claude_runner::control::SDKInitializeOptions::default())
            .await
    }

    /// Spawn a persistent session with explicit SDK initialization options.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration, admission, process, or protocol error.
    pub async fn spawn_session_with_options(
        &self,
        options: claude_runner::control::SDKInitializeOptions,
    ) -> Result<NornWrappedClaudeSession, NornWrappedClaudeError> {
        NornWrappedClaudeSession::spawn(&self.config, options).await
    }

    /// Run one prompt through the one-shot print-mode surface.
    ///
    /// Persistent conversations, rich input, correlated controls, and safe interruption
    /// are available through [`spawn_session`](Self::spawn_session).
    ///
    /// # Errors
    ///
    /// Returns [`IntegrationError::ClaudeRunnerError`] for invalid wrapper
    /// configuration or process/stream failures.
    pub fn run(&self, prompt: &str) -> Result<Vec<SessionEvent>, IntegrationError> {
        let command = self
            .build_command(prompt)
            .map_err(|error| integration_error_from_wrapped(&error))?;
        let claude_events = spawn_and_collect(&command)?;
        validate_terminal_result(&claude_events)?;
        self.capture_session_events(&claude_events)
    }

    /// Map Claude events into Norn session events and record the latest Claude
    /// session identifier.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a tool payload cannot be represented honestly.
    pub fn capture_session_events(
        &self,
        events: &[ClaudeEvent],
    ) -> Result<Vec<SessionEvent>, IntegrationError> {
        capture_session_events(events, &self.claude_session_id)
    }

    pub(crate) fn build_command(
        &self,
        prompt: &str,
    ) -> Result<ClaudeCommand, NornWrappedClaudeError> {
        self.config.build_oneshot_command(prompt)
    }
}

fn integration_error_from_wrapped(error: &NornWrappedClaudeError) -> IntegrationError {
    IntegrationError::ClaudeRunnerError {
        reason: error.to_string(),
    }
}

pub(super) fn validate_terminal_result(events: &[ClaudeEvent]) -> Result<(), IntegrationError> {
    let mut saw_result = false;
    for event in events {
        let ClaudeEvent::Result {
            subtype,
            is_error,
            error,
            ..
        } = event
        else {
            continue;
        };
        saw_result = true;
        let affirmative_success =
            subtype.as_deref() == Some("success") && is_error != &Some(true) && error.is_none();
        if !affirmative_success {
            let reason = error
                .as_deref()
                .or(subtype.as_deref())
                .unwrap_or("Claude Code terminal result omitted an affirmative success status");
            return Err(IntegrationError::ClaudeRunnerError {
                reason: reason.to_owned(),
            });
        }
    }

    if saw_result {
        Ok(())
    } else {
        Err(IntegrationError::ClaudeRunnerError {
            reason: "Claude Code stream ended without a terminal result".to_owned(),
        })
    }
}
