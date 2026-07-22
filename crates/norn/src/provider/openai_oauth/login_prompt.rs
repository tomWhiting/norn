//! Explicit presentation boundary for interactive OAuth instructions.

use std::time::Duration;

/// One transient OAuth instruction that may be written directly to a terminal.
///
/// Authorization URLs and device codes intentionally have no `Display`
/// implementation. Callers must opt into the terminal presentation boundary
/// through [`LoginPromptPresenter`].
pub enum LoginPrompt<'a> {
    /// Browser PKCE authorization target for the local callback flow.
    Browser {
        /// HTTPS authorization target containing one attempt's state.
        authorization_url: &'a str,
    },
    /// Headless device-code verification target and one-time code.
    DeviceCode {
        /// HTTPS page that accepts the one-time code.
        verification_url: &'a str,
        /// One-time code issued for this login attempt.
        user_code: &'a str,
        /// Maximum time Norn will wait for authorization.
        expires_after: Duration,
    },
}

impl std::fmt::Debug for LoginPrompt<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Browser { .. } => formatter
                .debug_struct("BrowserLoginPrompt")
                .field("authorization_url", &"[REDACTED]")
                .finish(),
            Self::DeviceCode { expires_after, .. } => formatter
                .debug_struct("DeviceCodeLoginPrompt")
                .field("verification_url", &"[REDACTED]")
                .field("user_code", &"[REDACTED]")
                .field("expires_after", expires_after)
                .finish(),
        }
    }
}

/// Non-disclosing failure to present interactive login instructions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("login instructions could not be written to the terminal")]
pub struct LoginPromptError;

impl LoginPromptError {
    /// Creates a presentation failure without retaining terminal I/O details.
    #[must_use]
    pub const fn terminal_output_unavailable() -> Self {
        Self
    }
}

/// Trusted sink for transient OAuth instructions.
///
/// Implementations should write only to an interactive terminal. They must not
/// route prompts through tracing, session events, debug dumps, or error values.
pub trait LoginPromptPresenter: Send + Sync {
    /// Presents one browser or device-code instruction.
    ///
    /// # Errors
    ///
    /// Returns a non-disclosing failure when the terminal cannot be written.
    fn present(&self, prompt: LoginPrompt<'_>) -> Result<(), LoginPromptError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_debug_redacts_urls_and_codes() {
        let browser = LoginPrompt::Browser {
            authorization_url: "https://auth.example/secret-state",
        };
        let device = LoginPrompt::DeviceCode {
            verification_url: "https://auth.example/device-secret",
            user_code: "CODE-SECRET",
            expires_after: Duration::from_secs(900),
        };

        let rendered = format!("{browser:?} {device:?}");
        assert!(!rendered.contains("secret-state"));
        assert!(!rendered.contains("device-secret"));
        assert!(!rendered.contains("CODE-SECRET"));
        assert!(rendered.contains("[REDACTED]"));
    }
}
