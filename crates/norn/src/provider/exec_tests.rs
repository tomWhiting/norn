use std::io;
use std::sync::Arc;
use std::time::Duration;

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::error::ErrorClass;
use crate::provider::auth::MockAuthProvider;
use crate::provider::http_client::build_streaming_client;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn executor(endpoint: String) -> Result<StreamExecutor, Box<dyn std::error::Error + Send + Sync>> {
    let client = build_streaming_client(Duration::from_secs(5))?;
    let auth_provider = Arc::new(MockAuthProvider::single("private-test-token"));
    let executor = StreamExecutor {
        client,
        endpoint,
        timeout: Duration::from_secs(5),
        max_retries: 0,
        retry_backoff: Duration::from_secs(1),
        retry_after_ceiling: None,
        rate_limiter: Arc::new(RateLimiter::new(1, Duration::from_secs(1))),
        auth_provider,
        debug_dump_file: None,
        backend_label: "responses",
    };
    Ok(executor)
}

#[tokio::test]
async fn every_redirect_status_is_an_explicit_terminal_policy_refusal() -> TestResult {
    for status in [301_u16, 302, 303, 307, 308] {
        let target = MockServer::start().await;
        let source = MockServer::start().await;
        let private_location = format!("{}/captured-private-location", target.uri());
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("location", private_location.as_str())
                    .set_body_string("private-redirect-body"),
            )
            .mount(&source)
            .await;
        let executor = executor(format!("{}/initial", source.uri()))?;

        let Err(error) = executor
            .send_with_retries(r#"{"private":"request-body"}"#)
            .await
        else {
            return Err(io::Error::other(format!(
                "HTTP {status} redirect was accepted as a successful response"
            ))
            .into());
        };
        let ProviderError::RedirectPolicyRefused {
            status: refused_status,
            backend,
        } = error
        else {
            return Err(io::Error::other(format!(
                "HTTP {status} redirect did not produce a policy refusal"
            ))
            .into());
        };

        assert_eq!(refused_status, status);
        assert_eq!(backend, "responses");
        let error = ProviderError::RedirectPolicyRefused {
            status: refused_status,
            backend,
        };
        let rendered = error.to_string();
        assert!(rendered.contains(&status.to_string()));
        assert!(rendered.contains("redirects are not followed by policy"));
        assert!(!rendered.contains(&private_location));
        assert!(!rendered.contains("private-redirect-body"));
        assert_eq!(error.class(), ErrorClass::Terminal);

        let source_requests = source
            .received_requests()
            .await
            .ok_or_else(|| io::Error::other("source request recording is disabled"))?;
        assert_eq!(source_requests.len(), 1);
        let target_requests = target
            .received_requests()
            .await
            .ok_or_else(|| io::Error::other("target request recording is disabled"))?;
        assert!(target_requests.is_empty());
    }
    Ok(())
}
