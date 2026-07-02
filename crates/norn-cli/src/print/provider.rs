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

use norn::error::ProviderError;
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
                None => AuthSource::OAuth { codex_home: None },
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
            kind: norn::error::TransientKind::ConnectionReset,
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
            api_key_env: Some("LOCAL_AI_KEY".to_owned()),
            debug_dump_dir: None,
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: Some(Duration::from_secs(30)),
            retry_backoff: Some(Duration::from_millis(250)),
            retry_after_ceiling: Some(Duration::from_secs(90)),
            runner_path: None,
        };
        let config = ProviderConfig {
            auth_source: AuthSource::OAuth { codex_home: None },
            timeout: overrides.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
            max_retries: overrides.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
            base_url: overrides.base_url,
            provider_options: overrides.provider_options,
            debug_dump_file: None,
            rate_limit: overrides.rate_limit,
            rate_limit_interval: overrides.rate_limit_interval,
            retry_backoff: overrides.retry_backoff,
            retry_after_ceiling: overrides.retry_after_ceiling,
        };
        assert_eq!(config.base_url, Some("http://localhost:8080".to_owned()));
        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(config.max_retries, 5);
        assert!(config.provider_options.is_some());
        assert_eq!(config.rate_limit_interval, Some(Duration::from_secs(30)));
        assert_eq!(config.retry_backoff, Some(Duration::from_millis(250)));
        assert_eq!(config.retry_after_ceiling, Some(Duration::from_secs(90)));
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
    async fn openai_compatible_requires_base_url() {
        let overrides = ProviderConfigOverrides {
            api_key_env: Some("NORN_TEST_COMPAT_KEY_BASE_URL".to_owned()),
            ..ProviderConfigOverrides::default()
        };
        let result = temp_env::async_with_vars(
            [("NORN_TEST_COMPAT_KEY_BASE_URL", Some("test-key"))],
            build_provider(ProviderKind::OpenaiCompatible, &overrides, "local-model"),
        )
        .await;
        let Err(err) = result else {
            panic!("expected provider error");
        };
        match err {
            ProviderBuildError::Provider(reason) => assert!(reason.contains("base_url")),
            other @ ProviderBuildError::Auth(_) => {
                panic!("expected provider error, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn openai_compatible_requires_api_key_env() {
        let overrides = ProviderConfigOverrides {
            base_url: Some("http://localhost:11434/v1".to_owned()),
            api_key_env: Some("NORN_TEST_COMPAT_KEY_MISSING".to_owned()),
            ..ProviderConfigOverrides::default()
        };
        let result = temp_env::async_with_vars(
            [("NORN_TEST_COMPAT_KEY_MISSING", None::<&str>)],
            build_provider(ProviderKind::OpenaiCompatible, &overrides, "local-model"),
        )
        .await;
        let Err(err) = result else {
            panic!("expected auth error");
        };
        match err {
            ProviderBuildError::Auth(reason) => {
                assert!(reason.contains("NORN_TEST_COMPAT_KEY_MISSING"));
            }
            other @ ProviderBuildError::Provider(_) => {
                panic!("expected auth error, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn openai_compatible_builds_with_api_key_env() {
        let overrides = ProviderConfigOverrides {
            base_url: Some("http://localhost:11434/v1".to_owned()),
            api_key_env: Some("NORN_TEST_COMPAT_KEY_PRESENT".to_owned()),
            ..ProviderConfigOverrides::default()
        };
        let built = temp_env::async_with_vars(
            [("NORN_TEST_COMPAT_KEY_PRESENT", Some("test-key"))],
            build_provider(ProviderKind::OpenaiCompatible, &overrides, "local-model"),
        )
        .await
        .expect("compatible provider builds without network I/O");
        match built {
            BuiltProvider::OpenAiCompatible(_) => {}
            BuiltProvider::OpenAi(_) | BuiltProvider::ClaudeRunner(_) => {
                panic!("expected OpenAiCompatible variant")
            }
        }
    }

    #[tokio::test]
    async fn openai_responses_builds_with_api_key_env_when_selected() {
        let overrides = ProviderConfigOverrides {
            api_key_env: Some("NORN_TEST_OPENAI_KEY_PRESENT".to_owned()),
            ..ProviderConfigOverrides::default()
        };
        let built = temp_env::async_with_vars(
            [("NORN_TEST_OPENAI_KEY_PRESENT", Some("test-key"))],
            build_provider(ProviderKind::Openai, &overrides, "gpt-5.5"),
        )
        .await
        .expect("OpenAI provider builds without network I/O");
        match built {
            BuiltProvider::OpenAi(_) => {}
            BuiltProvider::OpenAiCompatible(_) | BuiltProvider::ClaudeRunner(_) => {
                panic!("expected OpenAi variant")
            }
        }
    }

    #[tokio::test]
    async fn claude_runner_honors_settings_runner_path_override() {
        // Regression for the ignored `settings.provider.runner_path`:
        // the documented override must reach the constructed adapter.
        let overrides = ProviderConfigOverrides {
            runner_path: Some(PathBuf::from("/opt/tools/claude-custom")),
            ..ProviderConfigOverrides::default()
        };
        let built = build_provider(ProviderKind::ClaudeRunner, &overrides, "sonnet")
            .await
            .expect("claude-runner construction is infallible");
        match built {
            BuiltProvider::ClaudeRunner(adapter) => assert_eq!(
                adapter.runner_path(),
                std::path::Path::new("/opt/tools/claude-custom"),
            ),
            BuiltProvider::OpenAi(_) | BuiltProvider::OpenAiCompatible(_) => {
                panic!("expected ClaudeRunner variant")
            }
        }
    }

    #[tokio::test]
    async fn claude_runner_defaults_to_claude_when_runner_path_unset() {
        let overrides = ProviderConfigOverrides::default();
        let built = build_provider(ProviderKind::ClaudeRunner, &overrides, "sonnet")
            .await
            .expect("claude-runner construction is infallible");
        match built {
            BuiltProvider::ClaudeRunner(adapter) => assert_eq!(
                adapter.runner_path(),
                std::path::Path::new(DEFAULT_RUNNER_PATH),
            ),
            BuiltProvider::OpenAi(_) | BuiltProvider::OpenAiCompatible(_) => {
                panic!("expected ClaudeRunner variant")
            }
        }
    }

    #[tokio::test]
    async fn claude_runner_construction_is_synchronous_and_succeeds() {
        // ClaudeRunnerAdapter::new is infallible — verify build_provider
        // wraps it correctly and returns a usable &dyn Provider.
        let overrides = ProviderConfigOverrides::default();
        let built = build_provider(ProviderKind::ClaudeRunner, &overrides, "sonnet")
            .await
            .expect("claude-runner construction is infallible");
        match built {
            BuiltProvider::ClaudeRunner(_) => {}
            BuiltProvider::OpenAi(_) | BuiltProvider::OpenAiCompatible(_) => {
                panic!("expected ClaudeRunner variant")
            }
        }
        // Borrowing as &dyn Provider must compile.
        let _: &dyn Provider = built.as_dyn();
    }
}
