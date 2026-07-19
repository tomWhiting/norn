//! Request-task ownership for channel-backed provider streams.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;

use crate::error::ProviderError;
use crate::provider::events::ProviderEvent;
use crate::provider::traits::ProviderStream;

type StreamItem = Result<ProviderEvent, ProviderError>;

/// Channel-backed stream that owns the task producing its events.
///
/// Dropping the consumer is the cancellation boundary for the complete HTTP
/// request lifecycle. Aborting an already-finished task is harmless.
struct TaskOwnedProviderStream {
    receiver: tokio_stream::wrappers::ReceiverStream<StreamItem>,
    producer: tokio::task::JoinHandle<()>,
}

impl Stream for TaskOwnedProviderStream {
    type Item = StreamItem;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        Pin::new(&mut this.receiver).poll_next(context)
    }
}

impl Drop for TaskOwnedProviderStream {
    fn drop(&mut self) {
        self.producer.abort();
    }
}

pub(crate) fn task_owned_provider_stream(
    receiver: tokio::sync::mpsc::Receiver<StreamItem>,
    producer: tokio::task::JoinHandle<()>,
) -> ProviderStream {
    Box::pin(TaskOwnedProviderStream {
        receiver: tokio_stream::wrappers::ReceiverStream::new(receiver),
        producer,
    })
}

#[cfg(test)]
#[path = "owned_stream_tests.rs"]
mod tests;
