//! Shared HTTP client construction with credential-safe redirect policy.

use std::time::Duration;

/// Builds a long-lived streaming client with bounded connection setup.
///
/// Redirects are disabled because a redirect can replay request bodies and
/// non-standard credential headers even when the HTTP library strips a bearer
/// header on a cross-origin hop.
pub(crate) fn build_streaming_client(
    connect_timeout: Duration,
) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect_timeout)
        .tcp_keepalive(Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .http2_keep_alive_interval(Duration::from_secs(30))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
}

/// Builds an asynchronous client with a whole-request deadline.
pub(crate) fn build_bounded_client(
    request_timeout: Duration,
) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(request_timeout)
        .build()
}

/// Builds a blocking client with a whole-request deadline.
pub(crate) fn build_blocking_bounded_client(
    request_timeout: Duration,
) -> Result<reqwest::blocking::Client, reqwest::Error> {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(request_timeout)
        .build()
}

#[cfg(test)]
mod tests {
    use std::io;

    use reqwest::StatusCode;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    async fn recorded_requests(server: &MockServer) -> Result<Vec<wiremock::Request>, io::Error> {
        server
            .received_requests()
            .await
            .ok_or_else(|| io::Error::other("wiremock request recording is disabled"))
    }

    async fn assert_cross_origin_redirect_is_not_followed(
        client: &reqwest::Client,
        redirect_status: u16,
    ) -> TestResult {
        let target = MockServer::start().await;
        let source = MockServer::start().await;
        let location = format!("{}/captured", target.uri());
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(redirect_status).insert_header("location", location),
            )
            .mount(&source)
            .await;

        let response = client
            .post(format!("{}/initial", source.uri()))
            .header("authorization", "Bearer bearer-secret")
            .header("chatgpt-account-id", "account-secret")
            .body(r#"{"private":"body-marker"}"#)
            .send()
            .await?;

        assert_eq!(response.status(), StatusCode::from_u16(redirect_status)?);
        let source_requests = recorded_requests(&source).await?;
        assert_eq!(source_requests.len(), 1);
        assert_eq!(
            source_requests[0]
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer bearer-secret")
        );
        assert_eq!(
            source_requests[0]
                .headers
                .get("chatgpt-account-id")
                .and_then(|value| value.to_str().ok()),
            Some("account-secret")
        );
        assert_eq!(source_requests[0].body, br#"{"private":"body-marker"}"#);
        assert!(recorded_requests(&target).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn streaming_client_does_not_follow_any_redirect_status() -> TestResult {
        let client = build_streaming_client(Duration::from_secs(5))?;
        for status in [301, 302, 303, 307, 308] {
            assert_cross_origin_redirect_is_not_followed(&client, status).await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn streaming_client_does_not_follow_relative_same_origin_redirect() -> TestResult {
        let source = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", "/captured"))
            .mount(&source)
            .await;
        let client = build_streaming_client(Duration::from_secs(5))?;

        let response = client
            .post(format!("{}/initial", source.uri()))
            .header("authorization", "Bearer bearer-secret")
            .body("body-marker")
            .send()
            .await?;

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(recorded_requests(&source).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn bounded_async_client_does_not_follow_redirects() -> TestResult {
        let client = build_bounded_client(Duration::from_secs(5))?;
        assert_cross_origin_redirect_is_not_followed(&client, 307).await
    }

    #[tokio::test]
    async fn bounded_blocking_client_does_not_follow_redirects() -> TestResult {
        let target = MockServer::start().await;
        let source = MockServer::start().await;
        let location = format!("{}/captured", target.uri());
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(308).insert_header("location", location))
            .mount(&source)
            .await;
        let source_uri = format!("{}/initial", source.uri());

        let status = tokio::task::spawn_blocking(move || -> Result<StatusCode, reqwest::Error> {
            let client = build_blocking_bounded_client(Duration::from_secs(5))?;
            client
                .post(source_uri)
                .header("authorization", "Bearer bearer-secret")
                .header("chatgpt-account-id", "account-secret")
                .body(r#"{"private":"body-marker"}"#)
                .send()
                .map(|response| response.status())
        })
        .await??;

        assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(recorded_requests(&source).await?.len(), 1);
        assert!(recorded_requests(&target).await?.is_empty());
        Ok(())
    }
}
