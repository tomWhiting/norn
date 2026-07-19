//! Provider construction for OpenAI-compatible Chat Completions endpoints.

use std::sync::Arc;
use std::time::Duration;

use super::execute::SenderProvider;
use crate::error::ProviderError;
use crate::provider::auth::{AuthProvider, AuthSource, build_from_auth_source};
use crate::provider::events::ProviderEvent;
use crate::provider::exec::{DEFAULT_RETRY_BACKOFF, StreamExecutor};
use crate::provider::openai::rate_limiter::RateLimiter;
use crate::provider::owned_stream::task_owned_provider_stream;
use crate::provider::request::{ProviderConfig, ProviderOptions, ProviderRequest};
use crate::provider::startup_trace;
use crate::provider::tools::ProviderCapabilities;
use crate::provider::traits::{Provider, ProviderStream};

const DEFAULT_PERMITS_PER_INTERVAL: u32 = 60;
const DEFAULT_RATE_LIMIT_INTERVAL: Duration = Duration::from_mins(1);

/// OpenAI-compatible Chat Completions provider.
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    endpoint: String,
    config: ProviderConfig,
    rate_limiter: Arc<RateLimiter>,
    auth_provider: Arc<dyn AuthProvider>,
}

impl std::fmt::Debug for OpenAiCompatibleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatibleProvider")
            .field("backend", &"openai_compatible")
            .field("timeout", &self.config.timeout)
            .field("max_retries", &self.config.max_retries)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatibleProvider {
    /// Creates a provider from a standard provider config.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidRequest`] when the authentication source
    /// and destination do not form a permitted compatible backend, or another
    /// [`ProviderError`] when authentication or HTTP client construction fails.
    pub async fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        validate_auth_source(&config.auth_source)?;
        let endpoint = endpoint_from_base_url(config.base_url.as_deref())?;
        let auth_started = startup_trace::start("openai_compatible_auth_provider_build_start");
        let auth_provider = build_from_auth_source(&config.auth_source).await?;
        startup_trace::elapsed("openai_compatible_auth_provider_build_done", auth_started);

        let provider_started = startup_trace::start("openai_compatible_http_provider_build_start");
        let provider = Self::from_parts(config, endpoint, auth_provider)?;
        startup_trace::elapsed(
            "openai_compatible_http_provider_build_done",
            provider_started,
        );
        Ok(provider)
    }

    /// Constructs a provider with an injected auth provider for this crate's
    /// unit tests.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidRequest`] when the authentication source
    /// and destination do not form a permitted compatible backend, and
    /// [`ProviderError::ConnectionFailed`] when the HTTP client cannot be built.
    #[cfg(test)]
    pub(crate) fn with_auth_provider(
        config: ProviderConfig,
        auth_provider: Arc<dyn AuthProvider>,
    ) -> Result<Self, ProviderError> {
        validate_auth_source(&config.auth_source)?;
        let endpoint = endpoint_from_base_url(config.base_url.as_deref())?;
        Self::from_parts(config, endpoint, auth_provider)
    }

    fn from_parts(
        config: ProviderConfig,
        endpoint: String,
        auth_provider: Arc<dyn AuthProvider>,
    ) -> Result<Self, ProviderError> {
        let client_started = startup_trace::start("openai_compatible_http_client_build_start");
        let client = build_http_client(config.timeout)?;
        startup_trace::elapsed("openai_compatible_http_client_build_done", client_started);
        let rate_limiter_started =
            startup_trace::start("openai_compatible_rate_limiter_build_start");
        let rate_limiter = Arc::new(RateLimiter::new(
            config.rate_limit.unwrap_or(DEFAULT_PERMITS_PER_INTERVAL),
            config
                .rate_limit_interval
                .unwrap_or(DEFAULT_RATE_LIMIT_INTERVAL),
        ));
        startup_trace::elapsed(
            "openai_compatible_rate_limiter_build_done",
            rate_limiter_started,
        );
        Ok(Self {
            client,
            endpoint,
            config,
            rate_limiter,
            auth_provider,
        })
    }
}

fn validate_auth_source(auth_source: &AuthSource) -> Result<(), ProviderError> {
    if matches!(auth_source, AuthSource::OAuth { .. }) {
        return Err(ProviderError::InvalidRequest {
            message: "Codex OAuth credentials cannot authenticate an OpenAI-compatible custom endpoint; use API-key authentication"
                .to_owned(),
        });
    }
    Ok(())
}

impl Provider for OpenAiCompatibleProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn stream(&self, mut request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        if request.config.is_none() {
            request.config = self.config.provider_options.clone().map(ProviderOptions);
        }
        let sender = SenderProvider {
            executor: StreamExecutor {
                client: self.client.clone(),
                endpoint: self.endpoint.clone(),
                timeout: self.config.timeout,
                max_retries: self.config.max_retries,
                retry_backoff: self.config.retry_backoff.unwrap_or(DEFAULT_RETRY_BACKOFF),
                retry_after_ceiling: self.config.retry_after_ceiling,
                rate_limiter: Arc::clone(&self.rate_limiter),
                auth_provider: Arc::clone(&self.auth_provider),
                debug_dump_file: self.config.debug_dump_file.clone(),
                backend_label: "chat completions",
            },
        };

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ProviderEvent, ProviderError>>(64);
        let producer = tokio::spawn(async move {
            if let Err(err) = sender.execute(request, tx.clone()).await {
                let _ = tx.send(Err(err)).await;
            }
        });
        Ok(task_owned_provider_stream(rx, producer))
    }
}

fn endpoint_from_base_url(base_url: Option<&str>) -> Result<String, ProviderError> {
    let Some(base_url) = base_url.map(str::trim).filter(|url| !url.is_empty()) else {
        return Err(ProviderError::InvalidRequest {
            message: "openai-compatible provider requires provider.base_url or -c base_url"
                .to_string(),
        });
    };
    let base_url = crate::provider::endpoint::validated_credential_base_url(base_url)?;
    crate::provider::endpoint::reject_chatgpt_api_key_destination(&base_url)?;
    Ok(format!("{base_url}/chat/completions"))
}

fn build_http_client(timeout: Duration) -> Result<reqwest::Client, ProviderError> {
    crate::provider::http_client::build_streaming_client(timeout).map_err(|err| {
        ProviderError::ConnectionFailed {
            reason: format!("failed to build HTTP client: {err}"),
            kind: crate::error::TransientKind::ConnectionReset,
        }
    })
}

const _: fn() = || {
    fn check<T: Send + Sync>() {}
    check::<OpenAiCompatibleProvider>();
    check::<Arc<OpenAiCompatibleProvider>>();
};

#[cfg(test)]
mod security_tests {
    use super::*;
    use crate::provider::auth::MockAuthProvider;

    fn oauth_config() -> ProviderConfig {
        ProviderConfig {
            auth_source: AuthSource::OAuth { auth_root: None },
            base_url: Some("https://attacker.example/v1".to_owned()),
            timeout: Duration::from_secs(5),
            max_retries: 0,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: None,
            retry_backoff: None,
            retry_after_ceiling: None,
        }
    }

    #[test]
    fn compatible_provider_rejects_oauth_before_auth_application() {
        let mock = Arc::new(MockAuthProvider::single("oauth-secret"));
        let auth_provider: Arc<dyn AuthProvider> = Arc::clone(&mock) as Arc<dyn AuthProvider>;
        let result = OpenAiCompatibleProvider::with_auth_provider(oauth_config(), auth_provider);

        assert!(matches!(result, Err(ProviderError::InvalidRequest { .. })));
        assert_eq!(mock.apply_call_count(), 0);
    }

    #[test]
    fn compatible_provider_debug_does_not_expose_endpoint_path() -> Result<(), ProviderError> {
        let config = ProviderConfig {
            auth_source: AuthSource::ApiKey {
                key: crate::provider::request::SecretString::new("api-key-secret"),
            },
            base_url: Some("https://example.test/private-path-secret".to_owned()),
            timeout: Duration::from_secs(5),
            max_retries: 0,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: None,
            retry_backoff: None,
            retry_after_ceiling: None,
        };
        let auth_provider: Arc<dyn AuthProvider> =
            Arc::new(MockAuthProvider::single("api-key-secret"));
        let provider = OpenAiCompatibleProvider::with_auth_provider(config, auth_provider)?;

        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("private-path-secret"));
        assert!(!rendered.contains("api-key-secret"));
        Ok(())
    }

    #[test]
    fn compatible_provider_rejects_chatgpt_api_key_destinations() {
        for base_url in [
            "https://chatgpt.com/backend-api/codex",
            "https://chatgpt.com./backend-api/%63odex",
        ] {
            let mut config = oauth_config();
            config.auth_source = AuthSource::ApiKey {
                key: crate::provider::request::SecretString::new("api-key-secret"),
            };
            config.base_url = Some(base_url.to_owned());
            let auth_provider: Arc<dyn AuthProvider> =
                Arc::new(MockAuthProvider::single("api-key-secret"));

            assert!(OpenAiCompatibleProvider::with_auth_provider(config, auth_provider).is_err());
        }
    }
}
