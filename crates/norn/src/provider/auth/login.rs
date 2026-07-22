//! Public interactive OAuth login configuration and error classification.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::super::openai_oauth::{LoginError, LoginPromptPresenter, LoginStorageFailureKind};
use super::accounts;
use crate::error::{
    ConfigError, NornError, OAuthCredentialFailureKind, ProviderError, TransientKind,
};

/// Configuration for browser or device-code OAuth login.
#[derive(Clone, Default)]
pub struct LoginConfig {
    /// Optional absolute override for the Norn OAuth credential root. `None`
    /// resolves to `$NORN_HOME/auth` (default `~/.norn/auth`).
    /// Supplying a path declares it Norn-owned; it must not identify a foreign
    /// Codex credential directory.
    pub auth_root: Option<PathBuf>,
    /// Whether to use the device-code flow instead of browser PKCE.
    pub device_code: bool,
    /// Optional total deadline for the device authorization authority flow.
    /// `None` uses the documented 15-minute device-code lifetime.
    pub device_code_timeout: Option<Duration>,
    pub(super) prompt_presenter: Option<Arc<dyn LoginPromptPresenter>>,
}

impl std::fmt::Debug for LoginConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoginConfig")
            .field("auth_root", &self.auth_root)
            .field("device_code", &self.device_code)
            .field("device_code_timeout", &self.device_code_timeout)
            .field(
                "prompt_presenter",
                &self.prompt_presenter.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl LoginConfig {
    /// Adds the trusted terminal-only presenter for browser URLs or device codes.
    #[must_use]
    pub fn with_prompt_presenter(mut self, presenter: Arc<dyn LoginPromptPresenter>) -> Self {
        self.prompt_presenter = Some(presenter);
        self
    }

    /// Overrides the total device authorization deadline.
    #[must_use]
    pub fn with_device_code_timeout(mut self, timeout: Duration) -> Self {
        self.device_code_timeout = Some(timeout);
        self
    }
}

/// Runs browser PKCE or headless device-code login.
///
/// Credentials are durably saved to the selected Norn-owned slot before this
/// function returns success. Named accounts are published only after that save.
///
/// # Errors
///
/// Returns [`NornError::Config`] for an invalid auth root, unavailable prompt
/// presenter, or browser launcher failure. Transport, authority, and credential
/// lifecycle failures retain their structural provider error type.
pub async fn login(config: LoginConfig) -> Result<(), NornError> {
    accounts::login_account(config, None).await
}

pub(super) fn map_login_error(error: LoginError) -> NornError {
    match error {
        LoginError::DescriptorAdmission(error) => {
            NornError::Provider(ProviderError::DescriptorAdmission(error))
        }
        error @ (LoginError::Bind | LoginError::Server(_) | LoginError::Canceled) => {
            NornError::Provider(ProviderError::ConnectionFailed {
                reason: error.to_string(),
                kind: TransientKind::ConnectionReset,
            })
        }
        error @ (LoginError::Browser(_)
        | LoginError::Presentation
        | LoginError::DeviceCodeUnsupported
        | LoginError::DeviceCodeConfiguration) => NornError::Config(ConfigError::InvalidConfig {
            reason: error.to_string(),
        }),
        error @ (LoginError::MissingCode
        | LoginError::AuthorizationFailed
        | LoginError::TokenExchange(_)
        | LoginError::DeviceCodeAuthority { .. }
        | LoginError::DeviceCodeMalformed { .. }
        | LoginError::DeviceCodeExpired) => {
            NornError::Provider(ProviderError::AuthenticationFailed {
                reason: error.to_string(),
            })
        }
        LoginError::DeviceCodeTransport => NornError::Provider(ProviderError::ConnectionFailed {
            reason: LoginError::DeviceCodeTransport.to_string(),
            kind: TransientKind::ConnectionReset,
        }),
        LoginError::Storage { kind, reason } => match kind {
            LoginStorageFailureKind::Conflict => {
                NornError::Provider(ProviderError::OAuthCredentialFailure {
                    kind: OAuthCredentialFailureKind::Conflict,
                    reason,
                })
            }
            LoginStorageFailureKind::Undurable => {
                NornError::Provider(ProviderError::OAuthCredentialFailure {
                    kind: OAuthCredentialFailureKind::Undurable,
                    reason,
                })
            }
            LoginStorageFailureKind::Coordination => {
                NornError::Provider(ProviderError::ConnectionFailed {
                    reason,
                    kind: TransientKind::ConnectionReset,
                })
            }
        },
    }
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
