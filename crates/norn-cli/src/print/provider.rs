//! Provider construction for print-mode execution (NC-003 R1 / R2).
//!
//! Bridges the parsed CLI surface ([`crate::cli::Cli::provider`] + the
//! provider-config overrides extracted by NC-004) onto the concrete
//! [`norn::provider::traits::Provider`] implementation that
//! `run_agent_step` consumes.
//!
//! Three backends are supported:
//!
//! - [`norn::provider::openai::OpenAiProvider`] — default, OAuth via
//!   `OpenAI` `ChatGPT` auth. Async constructor.
//! - [`norn::provider::openai_compatible::OpenAiCompatibleProvider`] —
//!   API-key authenticated Chat Completions-compatible HTTP endpoint.
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

use norn::error::{ErrorClass, ProviderError};
use norn::integration::{ClaudeRunnerAdapter, ClaudeRunnerConfig};
use norn::provider::auth::AuthSource;
use norn::provider::openai::OpenAiProvider;
use norn::provider::openai_compatible::OpenAiCompatibleProvider;
use norn::provider::request::{ProviderConfig, SecretString};
use norn::provider::traits::Provider;

use crate::cli::ExitCode;
use crate::cli::ProviderKind;
use crate::config::ProviderConfigOverrides;
use crate::print::provider_trace;

/// Default HTTP request timeout when neither `settings.provider.timeout`
/// nor `-c request_timeout` supplies a value.
///
/// Owner-approved explicit default (2 minutes, approved 2026-06-11).
/// Override surfaces, lowest to highest precedence:
/// `settings.provider.timeout` → `-c request_timeout=<duration>`.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_mins(2);

/// Default HTTP retry budget when neither `settings.provider.max_retries`
/// nor `-c max_retries` supplies a value.
///
/// Owner-approved explicit default (2 retries, approved 2026-06-11).
/// Override surfaces, lowest to highest precedence:
/// `settings.provider.max_retries` → `-c max_retries=<u32>`.
const DEFAULT_MAX_RETRIES: u32 = 2;

/// Default Claude Runner binary when `settings.provider.runner_path` is
/// unset: the documented existing behaviour of resolving `claude` on
/// `PATH`. Kept explicit so the settings override and the fallback are
/// visible in one place.
const DEFAULT_RUNNER_PATH: &str = "claude";
const DEFAULT_OPENAI_COMPAT_API_KEY_ENV: &str = "NORN_OPENAI_COMPAT_API_KEY";

/// Concrete provider returned by [`build_provider`]. Holds the backend
/// behind an [`Arc`] so the caller can both hand a `&dyn Provider` to
/// `run_agent_step` and clone a shared `Arc<dyn Provider>` into the
/// [`norn::tools::agent::AgentToolInfra`] extension for sub-agent spawns.
pub enum BuiltProvider {
    /// OAuth-authenticated `OpenAI` provider.
    OpenAi(Arc<OpenAiProvider>),
    /// API-key authenticated OpenAI-compatible Chat Completions provider.
    OpenAiCompatible(Arc<OpenAiCompatibleProvider>),
    /// Claude Code CLI adapter.
    ClaudeRunner(Arc<ClaudeRunnerAdapter>),
}

impl BuiltProvider {
    /// Borrow as a [`Provider`] trait object for `run_agent_step`.
    #[must_use]
    pub fn as_dyn(&self) -> &dyn Provider {
        match self {
            Self::OpenAi(provider) => provider.as_ref(),
            Self::OpenAiCompatible(provider) => provider.as_ref(),
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
            Self::OpenAiCompatible(provider) => Arc::clone(provider) as Arc<dyn Provider>,
            Self::ClaudeRunner(provider) => Arc::clone(provider) as Arc<dyn Provider>,
        }
    }
}

/// Errors produced while constructing the provider. Mapped to exit codes
/// by [`ProviderBuildError::exit_code`]: errors classified as authentication
/// failures map to 3, everything else maps to 1.
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
        if err.class() == ErrorClass::Auth {
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
/// Norn credential root cannot be resolved or no credential is present) and
/// [`ProviderBuildError::Provider`] for any other underlying
/// [`ProviderError`].
pub async fn build_provider(
    kind: ProviderKind,
    overrides: &ProviderConfigOverrides,
    model: &str,
) -> Result<BuiltProvider, ProviderBuildError> {
    let provider_build_started = provider_trace::provider_build_start(kind, model);
    match kind {
        ProviderKind::Openai => {
            let auth_source = match overrides.api_key_env.as_deref() {
                Some(api_key_env) => AuthSource::ApiKey {
                    key: SecretString::new(read_required_api_key(
                        api_key_env,
                        "--api-shape openai-responses",
                    )?),
                },
                None => AuthSource::OAuth { auth_root: None },
            };
            provider_trace::openai_auth_source_resolved(provider_build_started, &auth_source);
            let config = ProviderConfig {
                auth_source,
                base_url: overrides.base_url.clone(),
                timeout: overrides.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
                max_retries: overrides.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
                provider_options: overrides.provider_options.clone(),
                debug_dump_file: overrides.debug_dump_file.clone(),
                rate_limit: overrides.rate_limit,
                rate_limit_interval: overrides.rate_limit_interval,
                retry_backoff: overrides.retry_backoff,
                retry_after_ceiling: overrides.retry_after_ceiling,
            };
            provider_trace::openai_provider_new_start(
                provider_build_started,
                overrides.base_url.is_some(),
            );
            let provider = OpenAiProvider::new(config).await?;
            provider_trace::openai_provider_new_done(provider_build_started);
            Ok(BuiltProvider::OpenAi(Arc::new(provider)))
        }
        ProviderKind::OpenaiCompatible => {
            let base_url = overrides.base_url.clone().ok_or_else(|| {
                ProviderBuildError::Provider(
                    "--provider openai-compatible requires provider.base_url or -c base_url"
                        .to_string(),
                )
            })?;
            let api_key_env = overrides
                .api_key_env
                .as_deref()
                .unwrap_or(DEFAULT_OPENAI_COMPAT_API_KEY_ENV);
            provider_trace::openai_compatible_api_key_env_resolved(
                provider_build_started,
                api_key_env,
            );
            let api_key = read_required_api_key(api_key_env, "--provider openai-compatible")?;
            let config = ProviderConfig {
                auth_source: AuthSource::ApiKey {
                    key: SecretString::new(api_key),
                },
                base_url: Some(base_url),
                timeout: overrides.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
                max_retries: overrides.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
                provider_options: overrides.provider_options.clone(),
                debug_dump_file: overrides.debug_dump_file.clone(),
                rate_limit: overrides.rate_limit,
                rate_limit_interval: overrides.rate_limit_interval,
                retry_backoff: overrides.retry_backoff,
                retry_after_ceiling: overrides.retry_after_ceiling,
            };
            provider_trace::openai_compatible_provider_new_start(provider_build_started, true);
            let provider = OpenAiCompatibleProvider::new(config).await?;
            provider_trace::openai_compatible_provider_new_done(provider_build_started);
            Ok(BuiltProvider::OpenAiCompatible(Arc::new(provider)))
        }
        ProviderKind::ClaudeRunner => {
            // `settings.provider.runner_path` overrides the documented
            // default lookup of `"claude"` on PATH; the default applies
            // only when the setting is unset.
            let runner_path = overrides
                .runner_path
                .clone()
                .unwrap_or_else(|| PathBuf::from(DEFAULT_RUNNER_PATH));
            let config = ClaudeRunnerConfig {
                runner_path,
                model: model.to_owned(),
                max_tokens: None,
            };
            provider_trace::claude_runner_provider_ready(
                provider_build_started,
                &config.runner_path,
            );
            Ok(BuiltProvider::ClaudeRunner(Arc::new(
                ClaudeRunnerAdapter::new(config),
            )))
        }
    }
}

fn read_required_api_key(api_key_env: &str, selector: &str) -> Result<String, ProviderBuildError> {
    let api_key = std::env::var(api_key_env).map_err(|err| {
        ProviderBuildError::Auth(format!(
            "{selector} requires API key env var {api_key_env}: {err}",
        ))
    })?;
    if api_key.trim().is_empty() {
        return Err(ProviderBuildError::Auth(format!(
            "{selector} requires non-empty API key env var {api_key_env}",
        )));
    }
    Ok(api_key)
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
