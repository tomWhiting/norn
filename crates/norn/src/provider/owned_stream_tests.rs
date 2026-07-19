use std::io;
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::provider::auth::MockAuthProvider;
use crate::provider::exec::{SseEventMapper, StreamExecutor};
use crate::provider::http_client::build_streaming_client;
use crate::provider::openai::rate_limiter::RateLimiter;
use crate::provider::openai::sse::SseEvent;
use crate::provider::owned_stream_test_support::RetryServer;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const CASE_REPETITIONS: usize = 20;

struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[derive(Default)]
struct NeverReachedMapper;

impl SseEventMapper for NeverReachedMapper {
    fn map_event(&mut self, _event: &SseEvent) -> Vec<StreamItem> {
        Vec::new()
    }

    fn finish_on_clean_close(&mut self) -> Result<Option<ProviderEvent>, ProviderError> {
        Ok(None)
    }

    fn dump_label<'event>(&self, _event: &'event SseEvent) -> &'event str {
        "unreachable"
    }
}

fn executor(endpoint: String) -> TestResult<StreamExecutor> {
    Ok(StreamExecutor {
        client: build_streaming_client(Duration::from_secs(86_400))?,
        endpoint,
        timeout: Duration::from_secs(86_400),
        max_retries: 1,
        retry_backoff: Duration::from_secs(3_600),
        retry_after_ceiling: None,
        rate_limiter: Arc::new(RateLimiter::new(60, Duration::from_secs(60))),
        auth_provider: Arc::new(MockAuthProvider::single("test-key")),
        request_headers: reqwest::header::HeaderMap::new(),
        debug_dump_file: None,
        backend_label: "responses",
    })
}

#[tokio::test]
async fn dropping_stream_aborts_its_producer_task() -> TestResult {
    for _ in 0..CASE_REPETITIONS {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let (dropped_sender, dropped_receiver) = tokio::sync::oneshot::channel();
        let producer = tokio::spawn(async move {
            let _drop_signal = DropSignal(Some(dropped_sender));
            let _ = started_sender.send(());
            let _sender = sender;
            std::future::pending::<()>().await;
        });
        let stream = task_owned_provider_stream(receiver, producer);

        started_receiver
            .await
            .map_err(|error| io::Error::other(format!("producer did not start: {error}")))?;
        drop(stream);
        dropped_receiver
            .await
            .map_err(|error| io::Error::other(format!("producer was not aborted: {error}")))?;
    }
    Ok(())
}

#[tokio::test]
async fn dropping_stream_releases_a_blocked_channel_send() -> TestResult {
    for _ in 0..CASE_REPETITIONS {
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        let (blocked_sender, blocked_receiver) = tokio::sync::oneshot::channel();
        let (dropped_sender, dropped_receiver) = tokio::sync::oneshot::channel();
        let producer = tokio::spawn(async move {
            let _drop_signal = DropSignal(Some(dropped_sender));
            for index in 0..64 {
                if sender
                    .send(Ok(ProviderEvent::TextDelta {
                        text: index.to_string(),
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _ = blocked_sender.send(());
            let _ = sender
                .send(Ok(ProviderEvent::TextDelta {
                    text: "blocked".to_owned(),
                }))
                .await;
        });
        let stream = task_owned_provider_stream(receiver, producer);

        blocked_receiver
            .await
            .map_err(|error| io::Error::other(format!("producer did not fill channel: {error}")))?;
        drop(stream);
        dropped_receiver.await.map_err(|error| {
            io::Error::other(format!("blocked producer survived drop: {error}"))
        })?;
    }
    Ok(())
}

#[tokio::test]
async fn dropping_stream_aborts_a_rate_limiter_wait() -> TestResult {
    for _ in 0..CASE_REPETITIONS {
        let limiter = Arc::new(RateLimiter::new(1, Duration::from_secs(86_400)));
        limiter.acquire().await;
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        let (waiting_sender, waiting_receiver) = tokio::sync::oneshot::channel();
        let (dropped_sender, dropped_receiver) = tokio::sync::oneshot::channel();
        let task_limiter = Arc::clone(&limiter);
        let producer = tokio::spawn(async move {
            let _drop_signal = DropSignal(Some(dropped_sender));
            let _sender = sender;
            let _ = waiting_sender.send(());
            task_limiter.acquire().await;
        });
        let stream = task_owned_provider_stream(receiver, producer);

        waiting_receiver.await.map_err(|error| {
            io::Error::other(format!("producer did not reach limiter: {error}"))
        })?;
        drop(stream);
        dropped_receiver.await.map_err(|error| {
            io::Error::other(format!("rate-limited producer survived: {error}"))
        })?;
    }
    Ok(())
}

#[tokio::test]
async fn dropping_stream_aborts_real_429_backoff() -> TestResult {
    for _ in 0..CASE_REPETITIONS {
        let mut server = RetryServer::spawn().await?;
        let executor = executor(format!("{}/responses", server.base_url))?;
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        let (dropped_sender, dropped_receiver) = tokio::sync::oneshot::channel();
        let producer = tokio::spawn(async move {
            let _drop_signal = DropSignal(Some(dropped_sender));
            let mut mapper = NeverReachedMapper;
            let _ = executor
                .execute("{}".to_owned(), &mut mapper, &sender)
                .await;
        });
        let stream = task_owned_provider_stream(receiver, producer);

        server.wait_first_closed().await?;
        drop(stream);
        dropped_receiver.await.map_err(|error| {
            io::Error::other(format!("429-backoff producer survived drop: {error}"))
        })?;
    }
    Ok(())
}
