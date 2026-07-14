//! Mock provider for deterministic testing.
//!
//! `MockProvider` implements `Provider` with scripted event sequences,
//! enabling Layer 2 tests of the agent loop without real API calls.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::stream;

use super::events::ProviderEvent;
use super::request::ProviderRequest;
use super::tools::ProviderCapabilities;
use super::traits::{Provider, ProviderStream};
use crate::error::ProviderError;

/// A test provider that returns pre-configured event sequences.
///
/// Each call to `stream()` pops the next sequence from the list. When
/// called more times than sequences were provided, `stream()` returns a
/// [`ProviderError::StreamError`] describing the exhaustion.
pub struct MockProvider {
    responses: Mutex<Vec<Vec<ProviderEvent>>>,
    requests: Mutex<Vec<ProviderRequest>>,
    call_count: AtomicUsize,
    capabilities: ProviderCapabilities,
}

impl MockProvider {
    /// Creates a mock provider with one response sequence per expected call.
    ///
    /// The sequences are consumed in order: the first `stream()` call
    /// returns the first sequence, the second call returns the second, etc.
    pub fn new(responses: Vec<Vec<ProviderEvent>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
            capabilities: ProviderCapabilities::default(),
        }
    }

    /// Creates a mock provider with explicit provider capabilities.
    pub fn with_capabilities(
        responses: Vec<Vec<ProviderEvent>>,
        capabilities: ProviderCapabilities,
    ) -> Self {
        Self {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
            capabilities,
        }
    }

    /// Returns the number of times `stream()` has been called.
    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    /// Returns every request sent to the provider.
    pub fn requests(&self) -> Result<Vec<ProviderRequest>, ProviderError> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .map_err(|e| ProviderError::StreamError {
                reason: format!("mock provider request lock poisoned: {e}"),
                transient: None,
            })
    }
}

impl Provider for MockProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .map_err(|e| ProviderError::StreamError {
                reason: format!("mock provider request lock poisoned: {e}"),
                transient: None,
            })?
            .push(request);

        let mut responses = self
            .responses
            .lock()
            .map_err(|e| ProviderError::StreamError {
                reason: format!("mock provider lock poisoned: {e}"),
                transient: None,
            })?;

        if responses.is_empty() {
            return Err(ProviderError::StreamError {
                reason: format!(
                    "MockProvider: stream() called {} times but no more response sequences available",
                    self.call_count()
                ),
                transient: None,
            });
        }

        let events = responses.remove(0);
        let event_stream = stream::iter(events.into_iter().map(Ok));
        Ok(Box::pin(event_stream))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities
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
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use futures_util::StreamExt;

    use super::super::events::StopReason;
    use super::super::usage::Usage;
    use super::*;

    fn make_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![],
            tools: vec![],
            model: "test-model".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        }
    }

    #[tokio::test]
    async fn mock_provider_returns_configured_sequences() {
        let response1 = vec![
            ProviderEvent::TextDelta {
                text: "Hello".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ];
        let response2 = vec![ProviderEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                ..Usage::default()
            },
            response_id: None,
        }];

        let provider = MockProvider::new(vec![response1, response2]);
        assert_eq!(provider.call_count(), 0);

        let mut stream1 = provider.stream(make_request()).expect("first stream");
        let event1 = stream1.next().await.expect("first event").expect("ok");
        assert!(matches!(event1, ProviderEvent::TextDelta { ref text } if text == "Hello"));
        let event2 = stream1.next().await.expect("second event").expect("ok");
        assert!(matches!(
            event2,
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                ..
            }
        ));
        assert!(stream1.next().await.is_none());
        assert_eq!(provider.call_count(), 1);

        let mut stream2 = provider.stream(make_request()).expect("second stream");
        let event3 = stream2.next().await.expect("third event").expect("ok");
        assert!(matches!(
            event3,
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn mock_provider_errors_when_exhausted() {
        let provider = MockProvider::new(vec![]);
        let result = provider.stream(make_request());
        assert!(result.is_err());
    }

    #[test]
    fn mock_provider_is_object_safe() {
        use std::sync::Arc;
        let _provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
    }
}
