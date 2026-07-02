//! Provider construction for OpenAI-compatible Chat Completions endpoints.

use std::sync::Arc;
use std::time::Duration;

use super::execute::SenderProvider;
use crate::error::ProviderError;
use crate::provider::auth::{AuthProvider, build_from_auth_source};
use crate::provider::events::ProviderEvent;
use crate::provider::exec::{DEFAULT_RETRY_BACKOFF, StreamExecutor};
use crate::provider::openai::rate_limiter::RateLimiter;
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
            .field("endpoint", &self.endpoint)
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
    /// Returns a [`ProviderError`] when authentication cannot be built, the
    /// HTTP client cannot be constructed, or `base_url` is absent.
    pub async fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        let auth_started = startup_trace::start("openai_compatible_auth_provider_build_start");
        let auth_provider = build_from_auth_source(&config.auth_source).await?;
        startup_trace::elapsed("openai_compatible_auth_provider_build_done", auth_started);

        let provider_started = startup_trace::start("openai_compatible_http_provider_build_start");
        let provider = Self::with_auth_provider(config, auth_provider)?;
        startup_trace::elapsed(
            "openai_compatible_http_provider_build_done",
            provider_started,
        );
        Ok(provider)
    }

    /// Constructs a provider with an injected auth provider.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidRequest`] when `base_url` is absent or
    /// blank, and [`ProviderError::ConnectionFailed`] when the HTTP client
    /// cannot be built.
    pub fn with_auth_provider(
        config: ProviderConfig,
        auth_provider: Arc<dyn AuthProvider>,
    ) -> Result<Self, ProviderError> {
        let endpoint = endpoint_from_base_url(config.base_url.as_deref())?;
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
        tokio::spawn(async move {
            if let Err(err) = sender.execute(request, tx.clone()).await {
                let _ = tx.send(Err(err)).await;
            }
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

fn endpoint_from_base_url(base_url: Option<&str>) -> Result<String, ProviderError> {
    let Some(base_url) = base_url.map(str::trim).filter(|url| !url.is_empty()) else {
        return Err(ProviderError::InvalidRequest {
            message: "openai-compatible provider requires provider.base_url or -c base_url"
                .to_string(),
        });
    };
    Ok(format!(
        "{}/chat/completions",
        base_url.trim_end_matches('/')
    ))
}

fn build_http_client(timeout: Duration) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .connect_timeout(timeout)
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .http2_keep_alive_interval(std::time::Duration::from_secs(30))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
        .map_err(|err| ProviderError::ConnectionFailed {
            reason: format!("failed to build HTTP client: {err}"),
            kind: crate::error::TransientKind::ConnectionReset,
        })
}

const _: fn() = || {
    fn check<T: Send + Sync>() {}
    check::<OpenAiCompatibleProvider>();
    check::<Arc<OpenAiCompatibleProvider>>();
};
