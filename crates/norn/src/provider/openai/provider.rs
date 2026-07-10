//! [`OpenAiProvider`] construction and the [`Provider`] trait impl.

use std::sync::Arc;
use std::time::Duration;

use super::backend::OpenAiBackend;
use super::execute::SenderProvider;
use super::rate_limiter::RateLimiter;
use crate::error::ProviderError;
use crate::provider::auth::{AuthProvider, build_from_auth_source};
use crate::provider::events::ProviderEvent;
use crate::provider::exec::{DEFAULT_RETRY_BACKOFF, StreamExecutor};
use crate::provider::request::{ProviderConfig, ProviderOptions, ProviderRequest};
use crate::provider::startup_trace;
use crate::provider::traits::{Provider, ProviderStream};

/// Deliberate, owner-approved default (2026-06-11) used when
/// [`ProviderConfig::rate_limit`] is `None`: 60 permits per interval.
const DEFAULT_PERMITS_PER_INTERVAL: u32 = 60;
/// Deliberate, owner-approved default (2026-06-11) used when
/// [`ProviderConfig::rate_limit_interval`] is `None`: a 60-second
/// replenishment window, giving the default permits-per-minute
/// semantics.
const DEFAULT_RATE_LIMIT_INTERVAL: Duration = Duration::from_mins(1);

/// `OpenAI` Responses API provider.
///
/// Shared across agents via `Arc`. Owns an HTTP client, a
/// token-bucket rate limiter, and the [`AuthProvider`] that
/// authenticates each outgoing request.
pub struct OpenAiProvider {
    client: reqwest::Client,
    backend: OpenAiBackend,
    config: ProviderConfig,
    rate_limiter: Arc<RateLimiter>,
    auth_provider: Arc<dyn AuthProvider>,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("backend", &self.backend.label())
            .field("timeout", &self.config.timeout)
            .field("max_retries", &self.config.max_retries)
            .finish_non_exhaustive()
    }
}

impl OpenAiProvider {
    /// Creates a new `OpenAI` provider from the given configuration.
    ///
    /// Builds the [`AuthProvider`] from `config.auth_source`. For
    /// `AuthSource::OAuth`, this initialises the underlying
    /// local OAuth `AuthManager`, which may read from disk.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::ConnectionFailed`] if the HTTP client
    /// cannot be built, [`ProviderError::AuthenticationFailed`] if the auth
    /// provider cannot be initialised, or [`ProviderError::InvalidRequest`]
    /// when the auth source and destination do not form a permitted backend.
    pub async fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        let backend = OpenAiBackend::resolve(&config.auth_source, config.base_url.as_deref())?;
        let auth_started = startup_trace::start("openai_auth_provider_build_start");
        let auth_provider = build_from_auth_source(&config.auth_source).await?;
        startup_trace::elapsed("openai_auth_provider_build_done", auth_started);

        let provider_started = startup_trace::start("openai_http_provider_build_start");
        let provider = Self::from_parts(config, backend, auth_provider)?;
        startup_trace::elapsed("openai_http_provider_build_done", provider_started);
        Ok(provider)
    }

    /// Constructs a provider directly from a pre-built [`AuthProvider`]. Used
    /// only by this crate's unit tests.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidRequest`] when the auth source and
    /// destination do not form a permitted backend, or
    /// [`ProviderError::ConnectionFailed`] if the HTTP client cannot be built.
    #[cfg(test)]
    pub(crate) fn with_auth_provider(
        config: ProviderConfig,
        auth_provider: Arc<dyn AuthProvider>,
    ) -> Result<Self, ProviderError> {
        let backend = OpenAiBackend::resolve(&config.auth_source, config.base_url.as_deref())?;
        Self::from_parts(config, backend, auth_provider)
    }

    fn from_parts(
        config: ProviderConfig,
        backend: OpenAiBackend,
        auth_provider: Arc<dyn AuthProvider>,
    ) -> Result<Self, ProviderError> {
        let client_started = startup_trace::start("openai_http_client_build_start");
        let client = build_http_client(config.timeout)?;
        startup_trace::elapsed("openai_http_client_build_done", client_started);

        let rate_limiter_started = startup_trace::start("openai_rate_limiter_build_start");
        let rate_limiter = Arc::new(RateLimiter::new(
            config.rate_limit.unwrap_or(DEFAULT_PERMITS_PER_INTERVAL),
            config
                .rate_limit_interval
                .unwrap_or(DEFAULT_RATE_LIMIT_INTERVAL),
        ));
        startup_trace::elapsed("openai_rate_limiter_build_done", rate_limiter_started);

        Ok(Self {
            client,
            backend,
            config,
            rate_limiter,
            auth_provider,
        })
    }

    fn base_url(&self) -> &str {
        self.backend.base_url()
    }

    fn endpoint(&self) -> String {
        format!("{}/responses", self.base_url())
    }

    /// Catalog backend identifier for the connection this provider is
    /// actually using; governs service-tier resolution during payload
    /// construction.
    fn catalog_backend(&self) -> &'static str {
        self.backend.catalog_backend()
    }
}

/// Builds the shared HTTP client.
///
/// `timeout` (from [`ProviderConfig::timeout`]) bounds connection
/// establishment. No whole-request timeout is set: streamed responses
/// are legitimately long-lived. Stalls after connect are bounded by the
/// header/read deadlines applied per request in
/// [`SenderProvider::execute`](super::execute::SenderProvider).
fn build_http_client(timeout: Duration) -> Result<reqwest::Client, ProviderError> {
    crate::provider::http_client::build_streaming_client(timeout).map_err(|e| {
        ProviderError::ConnectionFailed {
            reason: format!("failed to build HTTP client: {e}"),
            kind: crate::error::TransientKind::ConnectionReset,
        }
    })
}

impl Provider for OpenAiProvider {
    fn capabilities(&self) -> crate::provider::tools::ProviderCapabilities {
        if self.backend.is_codex_subscription() {
            return crate::provider::tools::ProviderCapabilities {
                hosted_web_search: true,
                response_threading: false,
                server_compaction: false,
            };
        }
        crate::provider::tools::ProviderCapabilities::openai_responses()
    }

    fn stream(&self, mut request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        if request.config.is_none() {
            request.config = self.config.provider_options.clone().map(ProviderOptions);
        }
        let sender = SenderProvider {
            executor: StreamExecutor {
                client: self.client.clone(),
                endpoint: self.endpoint(),
                timeout: self.config.timeout,
                max_retries: self.config.max_retries,
                retry_backoff: self.config.retry_backoff.unwrap_or(DEFAULT_RETRY_BACKOFF),
                retry_after_ceiling: self.config.retry_after_ceiling,
                rate_limiter: Arc::clone(&self.rate_limiter),
                auth_provider: Arc::clone(&self.auth_provider),
                debug_dump_file: self.config.debug_dump_file.clone(),
                backend_label: "responses",
            },
            catalog_backend: self.catalog_backend(),
        };

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ProviderEvent, ProviderError>>(64);

        tokio::spawn(async move {
            if let Err(e) = sender.execute(request, tx.clone()).await {
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

const _: fn() = || {
    fn check<T: Send + Sync>() {}
    check::<OpenAiProvider>();
    check::<Arc<OpenAiProvider>>();
};

#[cfg(test)]
mod security_tests {
    use super::*;
    use crate::provider::auth::{AuthSource, MockAuthProvider};
    use crate::provider::request::{Message, MessageRole, SecretString, ServiceTier};

    fn oauth_config(base_url: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            auth_source: AuthSource::OAuth { codex_home: None },
            base_url: base_url.map(str::to_owned),
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
    fn implicit_and_explicit_canonical_oauth_have_identical_semantics() -> Result<(), ProviderError>
    {
        let implicit_auth: Arc<dyn AuthProvider> =
            Arc::new(MockAuthProvider::single("implicit-token"));
        let explicit_auth: Arc<dyn AuthProvider> =
            Arc::new(MockAuthProvider::single("explicit-token"));
        let implicit = OpenAiProvider::with_auth_provider(oauth_config(None), implicit_auth)?;
        let explicit = OpenAiProvider::with_auth_provider(
            oauth_config(Some(super::super::backend::CHATGPT_BASE_URL)),
            explicit_auth,
        )?;

        assert_eq!(implicit.base_url(), explicit.base_url());
        assert_eq!(implicit.endpoint(), explicit.endpoint());
        assert_eq!(implicit.catalog_backend(), explicit.catalog_backend());
        assert_eq!(implicit.capabilities(), explicit.capabilities());
        assert!(!explicit.capabilities().response_threading);
        assert!(!explicit.capabilities().server_compaction);
        Ok(())
    }

    #[test]
    fn hostile_oauth_destination_is_rejected_before_auth_application() {
        let mock = Arc::new(MockAuthProvider::single("oauth-secret"));
        let auth_provider: Arc<dyn AuthProvider> = Arc::clone(&mock) as Arc<dyn AuthProvider>;
        let result = OpenAiProvider::with_auth_provider(
            oauth_config(Some("https://attacker.example/v1")),
            auth_provider,
        );

        assert!(matches!(result, Err(ProviderError::InvalidRequest { .. })));
        assert_eq!(mock.apply_call_count(), 0);
    }

    #[test]
    fn api_key_custom_endpoint_remains_supported() -> Result<(), ProviderError> {
        let config = ProviderConfig {
            auth_source: AuthSource::ApiKey {
                key: SecretString::new("api-key"),
            },
            base_url: Some("http://localhost:11434/v1".to_owned()),
            timeout: Duration::from_secs(5),
            max_retries: 0,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: None,
            retry_backoff: None,
            retry_after_ceiling: None,
        };
        let auth_provider: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("api-key"));
        let provider = OpenAiProvider::with_auth_provider(config, auth_provider)?;

        assert_eq!(provider.base_url(), "http://localhost:11434/v1");
        assert_eq!(provider.catalog_backend(), "responses_api");
        Ok(())
    }

    #[test]
    fn implicit_and_explicit_canonical_oauth_serialize_identical_payloads()
    -> Result<(), ProviderError> {
        let implicit_auth: Arc<dyn AuthProvider> =
            Arc::new(MockAuthProvider::single("implicit-token"));
        let explicit_auth: Arc<dyn AuthProvider> =
            Arc::new(MockAuthProvider::single("explicit-token"));
        let implicit = OpenAiProvider::with_auth_provider(oauth_config(None), implicit_auth)?;
        let explicit = OpenAiProvider::with_auth_provider(
            oauth_config(Some(super::super::backend::CHATGPT_BASE_URL)),
            explicit_auth,
        )?;
        let request = ProviderRequest {
            messages: vec![
                message(MessageRole::System, "stable instructions"),
                message(MessageRole::Developer, "dynamic developer context"),
                message(MessageRole::User, "user input"),
            ],
            tools: Vec::new(),
            model: "gpt-5.6-sol".to_owned(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: Some(ServiceTier::Fast),
            config: None,
            cache_key: Some("stable-cache-key".to_owned()),
            previous_response_id: None,
            store: false,
            context_management: None,
        };

        let implicit_payload =
            super::super::request::build_payload(&request, implicit.catalog_backend())?;
        let explicit_payload =
            super::super::request::build_payload(&request, explicit.catalog_backend())?;

        assert_eq!(implicit_payload, explicit_payload);
        assert_eq!(explicit_payload["store"], false);
        assert_eq!(explicit_payload["instructions"], "stable instructions");
        assert_eq!(explicit_payload["input"][0]["role"], "developer");
        assert_eq!(explicit_payload["service_tier"], "priority");
        assert_eq!(explicit_payload["prompt_cache_key"], "stable-cache-key");
        assert_eq!(
            explicit_payload["include"],
            serde_json::json!(["reasoning.encrypted_content"]),
        );
        assert!(explicit_payload.get("previous_response_id").is_none());
        assert!(explicit_payload.get("context_management").is_none());
        Ok(())
    }

    fn message(role: MessageRole, content: &str) -> Message {
        Message {
            role,
            content: Some(content.to_owned()),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use crate::provider::auth::{AuthSource, MockAuthProvider};
    use crate::provider::request::SecretString;

    fn test_config() -> ProviderConfig {
        ProviderConfig {
            auth_source: AuthSource::ApiKey {
                key: SecretString::new("test-key"),
            },
            base_url: Some("http://localhost:9999/v1".to_string()),
            timeout: Duration::from_secs(5),
            max_retries: 2,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: None,
            retry_backoff: None,
            retry_after_ceiling: None,
        }
    }

    fn test_provider() -> OpenAiProvider {
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
        OpenAiProvider::with_auth_provider(test_config(), mock).expect("create")
    }

    #[test]
    fn debug_does_not_expose_api_key() {
        let provider = test_provider();
        let debug = format!("{provider:?}");
        assert!(!debug.contains("test-key"));
        assert!(debug.contains("OpenAiProvider"));
    }

    #[test]
    fn arc_openai_provider_compiles() {
        let provider = test_provider();
        let _arc: Arc<OpenAiProvider> = Arc::new(provider);
    }

    #[test]
    fn default_base_url() {
        let mut config = test_config();
        config.base_url = None;
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("k"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        assert_eq!(provider.base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn custom_base_url() {
        let provider = test_provider();
        assert_eq!(provider.base_url(), "http://localhost:9999/v1");
    }

    #[test]
    fn chatgpt_oauth_capabilities_do_not_enable_response_threading() {
        let mut config = test_config();
        config.auth_source = AuthSource::OAuth { codex_home: None };
        config.base_url = None;
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("oauth-token"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");

        let capabilities = provider.capabilities();

        assert!(capabilities.hosted_web_search);
        assert!(!capabilities.response_threading);
        assert!(!capabilities.server_compaction);
    }

    #[test]
    fn api_key_openai_capabilities_keep_responses_state_features() {
        let mut config = test_config();
        config.base_url = None;
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("api-key"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");

        let capabilities = provider.capabilities();

        assert!(capabilities.hosted_web_search);
        assert!(capabilities.response_threading);
        assert!(capabilities.server_compaction);
    }

    #[test]
    fn endpoint_construction() {
        let provider = test_provider();
        assert_eq!(provider.endpoint(), "http://localhost:9999/v1/responses");
    }

    /// Regression test (final-state hardening, T1 item 9): the catalog
    /// backend used for service-tier resolution must track the connection
    /// the provider actually uses, not the catalog default. OAuth against
    /// the compiled `ChatGPT` base URL is the Codex subscription backend;
    /// API-key auth is the direct Responses API.
    #[test]
    fn catalog_backend_tracks_actual_connection() {
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("k"));

        let mut oauth_config = test_config();
        oauth_config.auth_source = AuthSource::OAuth { codex_home: None };
        oauth_config.base_url = None;
        let oauth_provider =
            OpenAiProvider::with_auth_provider(oauth_config, Arc::clone(&mock)).expect("create");
        assert_eq!(oauth_provider.catalog_backend(), "codex_subscription");

        let mut api_key_config = test_config();
        api_key_config.base_url = None;
        let api_key_provider =
            OpenAiProvider::with_auth_provider(api_key_config, Arc::clone(&mock)).expect("create");
        assert_eq!(api_key_provider.catalog_backend(), "responses_api");
    }

    #[test]
    fn rate_limit_none_uses_default_permits() {
        let mut config = test_config();
        config.rate_limit = None;
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("k"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        assert_eq!(
            provider.rate_limiter.permits_per_interval(),
            DEFAULT_PERMITS_PER_INTERVAL,
        );
    }

    #[test]
    fn rate_limit_some_overrides_default_permits() {
        let mut config = test_config();
        config.rate_limit = Some(120);
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("k"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        assert_eq!(provider.rate_limiter.permits_per_interval(), 120);
    }

    /// `rate_limit_interval: None` falls back to the deliberate,
    /// owner-approved 60-second window (permits-per-minute semantics).
    #[tokio::test]
    async fn rate_limit_interval_none_uses_default() {
        let mut config = test_config();
        config.rate_limit_interval = None;
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("k"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        assert_eq!(
            provider.rate_limiter.interval().await,
            DEFAULT_RATE_LIMIT_INTERVAL,
        );
    }

    /// `ProviderConfig::rate_limit_interval` overrides the replenishment
    /// window wired into the limiter.
    #[tokio::test]
    async fn rate_limit_interval_some_overrides_default() {
        let mut config = test_config();
        config.rate_limit_interval = Some(Duration::from_secs(5));
        let mock: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("k"));
        let provider = OpenAiProvider::with_auth_provider(config, mock).expect("create");
        assert_eq!(
            provider.rate_limiter.interval().await,
            Duration::from_secs(5),
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod integration_tests {
    use super::*;
    use crate::provider::auth::AuthSource;
    use crate::provider::events::ProviderEvent;
    use crate::provider::request::{
        Message, MessageRole, ProviderConfig, ProviderRequest, SecretString,
    };
    use futures_util::StreamExt;

    #[tokio::test]
    async fn openai_integration_test() {
        let api_key = match std::env::var("OPENAI_TEST_KEY") {
            Ok(key) if !key.is_empty() => key,
            _ => {
                tracing::info!("OPENAI_TEST_KEY not set, skipping");
                return;
            }
        };

        let config = ProviderConfig {
            auth_source: AuthSource::ApiKey {
                key: SecretString::new(api_key),
            },
            base_url: None,
            timeout: Duration::from_secs(30),
            max_retries: 2,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: None,
            retry_backoff: None,
            retry_after_ceiling: None,
        };

        let provider = OpenAiProvider::new(config).await.expect("create provider");
        let request = ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
                role: MessageRole::User,
                content: Some("Say hello in exactly one word.".to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }],
            tools: vec![],
            model: "gpt-4.1-mini".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        };

        let mut stream = provider.stream(request).expect("stream");
        let mut saw_text_delta = false;
        let mut saw_done = false;

        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderEvent::TextDelta { .. }) => saw_text_delta = true,
                Ok(ProviderEvent::Done { .. }) => saw_done = true,
                Ok(_) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert!(saw_text_delta, "expected at least one TextDelta event");
        assert!(saw_done, "expected a Done event");
    }
}
