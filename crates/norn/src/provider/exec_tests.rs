use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::error::ErrorClass;
use crate::provider::auth::MockAuthProvider;
use crate::provider::http_client::build_streaming_client;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl io::Write for SharedLog {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut destination = self
            .0
            .lock()
            .map_err(|error| io::Error::other(format!("shared log lock is poisoned: {error}")))?;
        std::io::Write::write(&mut *destination, buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for SharedLog {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

impl SharedLog {
    fn rendered(&self) -> io::Result<String> {
        let bytes = self
            .0
            .lock()
            .map_err(|error| io::Error::other(format!("shared log lock is poisoned: {error}")))?
            .clone();
        String::from_utf8(bytes).map_err(io::Error::other)
    }
}

#[derive(Default)]
struct NeverReachedMapper;

impl SseEventMapper for NeverReachedMapper {
    fn map_event(&mut self, _event: &SseEvent) -> Vec<Result<ProviderEvent, ProviderError>> {
        Vec::new()
    }

    fn finish_on_clean_close(&mut self) -> Result<Option<ProviderEvent>, ProviderError> {
        Ok(None)
    }

    fn dump_label<'event>(&self, _event: &'event SseEvent) -> &'event str {
        "unreachable"
    }
}

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
        request_headers: reqwest::header::HeaderMap::new(),
        debug_dump_file: None,
        backend_label: "responses",
    };
    Ok(executor)
}

async fn stalled_status_endpoint(
    status: u16,
    extra_header: Option<&str>,
) -> io::Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let extra_header = extra_header.unwrap_or_default().to_owned();
    let task = tokio::spawn(async move {
        let Ok((mut stream, _peer)) = listener.accept().await else {
            return;
        };
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let Ok(count) = stream.read(&mut chunk).await else {
                return;
            };
            if count == 0 {
                return;
            }
            request.extend_from_slice(&chunk[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let reason = if status == 401 {
            "Unauthorized"
        } else {
            "Too Many Requests"
        };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: 1000000\r\nConnection: keep-alive\r\n{extra_header}\r\nprivate-body-sentinel"
        );
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        std::future::pending::<()>().await;
    });
    Ok((format!("http://{address}/responses"), task))
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

#[tokio::test]
async fn specialized_401_and_429_paths_never_wait_for_or_disclose_the_body() -> TestResult {
    for (status, header) in [(401_u16, None), (429, Some("Retry-After: 0\r\n"))] {
        let (endpoint, server) = stalled_status_endpoint(status, header).await?;
        let executor = executor(endpoint)?;
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            executor.send_with_retries(r#"{"request":"sentinel"}"#),
        )
        .await
        .map_err(|_elapsed| {
            io::Error::other(format!("HTTP {status} path waited for stalled body"))
        })?;
        server.abort();

        let Err(error) = result else {
            return Err(io::Error::other(format!("HTTP {status} unexpectedly succeeded")).into());
        };
        let rendered = error.to_string();
        assert!(!rendered.contains("private-body-sentinel"));
        match status {
            401 => assert!(matches!(error, ProviderError::AuthenticationFailed { .. })),
            429 => assert!(matches!(error, ProviderError::RateLimited { .. })),
            _ => return Err(io::Error::other("test fixture used an unsupported status").into()),
        }
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn endpoint_path_secrets_do_not_reach_errors_or_traces() -> TestResult {
    const PATH_SECRET: &str = "endpoint-path-secret-must-not-escape";
    const QUERY_SECRET: &str = "endpoint-query-secret-must-not-escape";
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    drop(listener);
    let endpoint = format!("http://{address}/{PATH_SECRET}?token={QUERY_SECRET}");
    let executor = executor(endpoint)?;
    let logs = SharedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(logs.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    let mut mapper = NeverReachedMapper;

    let error = executor
        .execute("{}".to_owned(), &mut mapper, &sender)
        .await
        .err()
        .ok_or_else(|| io::Error::other("connection-refusal fixture unexpectedly succeeded"))?;
    let rendered_error = error.to_string();
    let rendered_logs = logs.rendered()?;
    for secret in [PATH_SECRET, QUERY_SECRET] {
        assert!(
            !rendered_error.contains(secret),
            "rendered provider error disclosed endpoint secret {secret}"
        );
        assert!(
            !rendered_logs.contains(secret),
            "provider trace disclosed endpoint secret {secret}"
        );
    }
    Ok(())
}
