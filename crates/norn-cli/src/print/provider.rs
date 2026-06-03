//! Provider construction for print-mode execution (NC-003 R1 / R2).
//!
//! Bridges the parsed CLI surface ([`crate::cli::Cli::provider`] + the
//! provider-config overrides extracted by NC-004) onto the concrete
//! [`norn::provider::traits::Provider`] implementation that
//! `run_agent_step` consumes.
//!
//! Two backends are supported:
//!
//! - [`norn::provider::openai::OpenAiProvider`] — default, OAuth via
//!   codex-login. Async constructor.
//! - [`norn::integration::ClaudeRunnerAdapter`] — selected via
//!   `--provider claude-runner`. Synchronous constructor.
//!
//! The unknown-provider path is handled by clap's `value_enum` parsing
//! before the binary ever reaches this module, so the only invariant this
//! module enforces is that the variant matches one of the two concrete
//! backends.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use norn::error::ProviderError;
use norn::integration::{ClaudeRunnerAdapter, ClaudeRunnerConfig};
use norn::provider::auth::AuthSource;
use norn::provider::openai::OpenAiProvider;
use norn::provider::request::ProviderConfig;
use norn::provider::traits::Provider;

use crate::cli::ExitCode;
use crate::cli::ProviderKind;
use crate::config::ProviderConfigOverrides;

/// Brief-mandated default HTTP request timeout when `-c request_timeout`
/// is not supplied. Documented in NC-003 R1 acceptance criteria.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_mins(2);

/// Brief-mandated default HTTP retry budget when `-c max_retries` is not
/// supplied. Documented in NC-003 R1 acceptance criteria.
const DEFAULT_MAX_RETRIES: u32 = 2;

/// Concrete provider returned by [`build_provider`]. Holds the backend
/// behind an [`Arc`] so the caller can both hand a `&dyn Provider` to
/// `run_agent_step` and clone a shared `Arc<dyn Provider>` into the
/// [`norn::tools::agent::AgentToolInfra`] extension for sub-agent spawns.
pub enum BuiltProvider {
    /// OAuth-authenticated `OpenAI` provider.
    OpenAi(Arc<OpenAiProvider>),
    /// Claude Code CLI adapter.
    ClaudeRunner(Arc<ClaudeRunnerAdapter>),
}

impl BuiltProvider {
    /// Borrow as a [`Provider`] trait object for `run_agent_step`.
    #[must_use]
    pub fn as_dyn(&self) -> &dyn Provider {
        match self {
            Self::OpenAi(provider) => provider.as_ref(),
            Self::ClaudeRunner(provider) => provider.as_ref(),
        }
    }

    /// Clone the backend as a shared `Arc<dyn Provider>`.
    ///
    /// Used to populate [`norn::tools::agent::AgentToolInfra::provider`]
    /// so spawned and forked sub-agents share the parent's provider.
    #[must_use]
    pub fn as_arc(&self) -> Arc<dyn Provider> {
        match self {
            Self::OpenAi(provider) => Arc::clone(provider) as Arc<dyn Provider>,
            Self::ClaudeRunner(provider) => Arc::clone(provider) as Arc<dyn Provider>,
        }
    }
}

/// Errors produced while constructing the provider. Mapped to exit codes
/// by [`ProviderBuildError::exit_code`]: `AuthenticationFailed` → 3 (auth
/// error), everything else → 1 (agent error).
#[derive(Debug, thiserror::Error)]
pub enum ProviderBuildError {
    /// Authentication failure surfaced by the underlying provider.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// Any other provider-construction failure (connection, parse, etc.).
    #[error("provider error: {0}")]
    Provider(String),
}

impl ProviderBuildError {
    /// Terminal exit code for this error per CO5.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::Auth(_) => ExitCode::AuthError,
            Self::Provider(_) => ExitCode::AgentError,
        }
    }
}

impl From<ProviderError> for ProviderBuildError {
    fn from(err: ProviderError) -> Self {
        if matches!(err, ProviderError::AuthenticationFailed { .. }) {
            Self::Auth(err.to_string())
        } else {
            Self::Provider(err.to_string())
        }
    }
}

/// Construct the provider selected by `--provider` (defaulting to `OpenAI`).
///
/// `model` is threaded into the Claude Runner config so the adapter's
/// fallback model matches the rest of the runtime. The `OpenAI` path
/// ignores `model` — it travels through `run_agent_step` per-request.
///
/// # Errors
///
/// Returns [`ProviderBuildError::Auth`] when OAuth bootstrap fails (the
/// codex home cannot be resolved or no credential is present) and
/// [`ProviderBuildError::Provider`] for any other underlying
/// [`ProviderError`].
pub async fn build_provider(
    kind: Option<ProviderKind>,
    overrides: &ProviderConfigOverrides,
    model: &str,
) -> Result<BuiltProvider, ProviderBuildError> {
    match kind.unwrap_or(ProviderKind::Openai) {
        ProviderKind::Openai => {
            let config = ProviderConfig {
                auth_source: AuthSource::OAuth { codex_home: None },
                base_url: overrides.base_url.clone(),
                timeout: overrides.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
                max_retries: overrides.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
                provider_options: overrides.provider_options.clone(),
                debug_dump_file: overrides.debug_dump_file.clone(),
                rate_limit: overrides.rate_limit,
            };
            let provider = OpenAiProvider::new(config).await?;
            Ok(BuiltProvider::OpenAi(Arc::new(provider)))
        }
        ProviderKind::ClaudeRunner => {
            let config = ClaudeRunnerConfig {
                runner_path: PathBuf::from("claude"),
                model: model.to_owned(),
                max_tokens: None,
            };
            Ok(BuiltProvider::ClaudeRunner(Arc::new(
                ClaudeRunnerAdapter::new(config),
            )))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_maps_to_exit_code_three() {
        let err = ProviderBuildError::Auth("expired".to_owned());
        assert_eq!(err.exit_code(), ExitCode::AuthError);
    }

    #[test]
    fn provider_error_maps_to_exit_code_one() {
        let err = ProviderBuildError::Provider("connection refused".to_owned());
        assert_eq!(err.exit_code(), ExitCode::AgentError);
    }

    #[test]
    fn authentication_failed_provider_error_converts_to_auth_variant() {
        let err: ProviderBuildError = ProviderError::AuthenticationFailed {
            reason: "token expired".to_owned(),
        }
        .into();
        assert!(matches!(err, ProviderBuildError::Auth(_)));
        assert_eq!(err.exit_code(), ExitCode::AuthError);
    }

    #[test]
    fn connection_failed_provider_error_converts_to_provider_variant() {
        let err: ProviderBuildError = ProviderError::ConnectionFailed {
            reason: "refused".to_owned(),
        }
        .into();
        assert!(matches!(err, ProviderBuildError::Provider(_)));
        assert_eq!(err.exit_code(), ExitCode::AgentError);
    }

    #[test]
    fn overrides_flow_through_to_provider_config_fields() {
        let overrides = ProviderConfigOverrides {
            base_url: Some("http://localhost:8080".to_owned()),
            request_timeout: Some(Duration::from_secs(30)),
            max_retries: Some(5),
            provider_options: Some(serde_json::json!({"key": "val"})),
            debug_dump_dir: None,
            debug_dump_file: None,
            rate_limit: None,
        };
        let config = ProviderConfig {
            auth_source: AuthSource::OAuth { codex_home: None },
            timeout: overrides.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
            max_retries: overrides.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
            base_url: overrides.base_url,
            provider_options: overrides.provider_options,
            debug_dump_file: None,
            rate_limit: overrides.rate_limit,
        };
        assert_eq!(config.base_url, Some("http://localhost:8080".to_owned()));
        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(config.max_retries, 5);
        assert!(config.provider_options.is_some());
    }

    #[test]
    fn default_overrides_use_brief_mandated_defaults() {
        let overrides = ProviderConfigOverrides::default();
        let timeout = overrides.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT);
        let retries = overrides.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);
        assert_eq!(timeout, Duration::from_mins(2));
        assert_eq!(retries, 2);
    }

    #[tokio::test]
    async fn claude_runner_construction_is_synchronous_and_succeeds() {
        // ClaudeRunnerAdapter::new is infallible — verify build_provider
        // wraps it correctly and returns a usable &dyn Provider.
        let overrides = ProviderConfigOverrides::default();
        let built = build_provider(Some(ProviderKind::ClaudeRunner), &overrides, "sonnet")
            .await
            .expect("claude-runner construction is infallible");
        match built {
            BuiltProvider::ClaudeRunner(_) => {}
            BuiltProvider::OpenAi(_) => panic!("expected ClaudeRunner variant"),
        }
        // Borrowing as &dyn Provider must compile.
        let _: &dyn Provider = built.as_dyn();
    }
}
