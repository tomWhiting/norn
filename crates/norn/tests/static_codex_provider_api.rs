//! Public-API contract for dispatch-scoped Codex credentials.

use std::time::Duration;

use norn::provider::auth::{AuthSource, StaticCodexCredential};
use norn::provider::openai::OpenAiProvider;
use norn::provider::request::{ProviderConfig, SecretString};
use norn::provider::traits::Provider;

#[test]
fn embedder_can_construct_pinned_non_refreshing_codex_provider()
-> Result<(), Box<dyn std::error::Error>> {
    let credential = StaticCodexCredential::new(
        SecretString::new("dispatch-access-token"),
        Some(SecretString::new("dispatch-account")),
    )?;
    let provider = OpenAiProvider::with_static_codex_credential(
        ProviderConfig {
            auth_source: AuthSource::oauth_default(),
            base_url: None,
            timeout: Duration::from_secs(5),
            max_retries: 0,
            provider_options: None,
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: None,
            retry_backoff: None,
            retry_after_ceiling: None,
        },
        credential,
    )?;

    let debug = format!("{provider:?}");
    assert!(debug.contains("codex_subscription"));
    assert!(!debug.contains("dispatch-access-token"));
    assert!(provider.capabilities().hosted_web_search);
    assert!(!provider.capabilities().response_threading);
    Ok(())
}
