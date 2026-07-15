//! Test auth provider shared by provider-layer fixtures.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use super::AuthProvider;
use crate::error::ProviderError;

/// Mock auth provider for tests.
///
/// Each call consumes the next configured value. Once a sequence reaches its
/// last entry, that entry is reused. An empty unauthorized sequence returns
/// `Ok(false)`.
pub struct MockAuthProvider {
    token_seq: Mutex<Vec<String>>,
    on_unauthorized_seq: Mutex<Vec<Result<bool, ProviderError>>>,
    apply_count: AtomicUsize,
    refresh_count: AtomicUsize,
}

impl MockAuthProvider {
    /// Constructs a mock with bearer-token and unauthorized-result sequences.
    #[must_use]
    pub fn new(tokens: Vec<String>, on_unauthorized: Vec<Result<bool, ProviderError>>) -> Self {
        let mut tokens_reversed = tokens;
        tokens_reversed.reverse();
        let mut unauth_reversed = on_unauthorized;
        unauth_reversed.reverse();
        Self {
            token_seq: Mutex::new(tokens_reversed),
            on_unauthorized_seq: Mutex::new(unauth_reversed),
            apply_count: AtomicUsize::new(0),
            refresh_count: AtomicUsize::new(0),
        }
    }

    /// Constructs a mock that yields one token on every request.
    #[must_use]
    pub fn single(token: impl Into<String>) -> Self {
        Self::new(vec![token.into()], Vec::new())
    }

    /// Constructs a mock with an explicit bearer-token sequence.
    #[must_use]
    pub fn with_token_sequence(tokens: Vec<String>) -> Self {
        Self::new(tokens, Vec::new())
    }

    /// Rebuilds this mock with an explicit unauthorized-result sequence.
    #[must_use]
    pub fn with_unauthorized_responses(self, responses: Vec<Result<bool, ProviderError>>) -> Self {
        let tokens = match self.token_seq.lock() {
            Ok(guard) => ordered_tokens(&guard),
            Err(poison) => ordered_tokens(&poison.into_inner()),
        };
        Self::new(tokens, responses)
    }

    /// Returns the number of authentication applications.
    #[must_use]
    pub fn apply_call_count(&self) -> usize {
        self.apply_count.load(Ordering::SeqCst)
    }

    /// Returns the number of unauthorized callbacks.
    #[must_use]
    pub fn refresh_call_count(&self) -> usize {
        self.refresh_count.load(Ordering::SeqCst)
    }
}

fn ordered_tokens(reversed: &[String]) -> Vec<String> {
    reversed.iter().rev().cloned().collect()
}

impl std::fmt::Debug for MockAuthProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MockAuthProvider")
            .field("apply_count", &self.apply_call_count())
            .field("refresh_count", &self.refresh_call_count())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AuthProvider for MockAuthProvider {
    async fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        self.apply_count.fetch_add(1, Ordering::SeqCst);
        let mut tokens = self
            .token_seq
            .lock()
            .map_err(|error| ProviderError::StreamError {
                reason: format!("mock auth lock poisoned: {error}"),
                transient: None,
            })?;
        let token = if tokens.len() <= 1 {
            tokens
                .last()
                .cloned()
                .unwrap_or_else(|| "mock-token".to_owned())
        } else {
            tokens.pop().unwrap_or_else(|| "mock-token".to_owned())
        };
        Ok(request.header("Authorization", format!("Bearer {token}")))
    }

    async fn on_unauthorized(&self) -> Result<bool, ProviderError> {
        self.refresh_count.fetch_add(1, Ordering::SeqCst);
        let mut outcomes =
            self.on_unauthorized_seq
                .lock()
                .map_err(|error| ProviderError::StreamError {
                    reason: format!("mock auth lock poisoned: {error}"),
                    transient: None,
                })?;
        if outcomes.len() <= 1 {
            match outcomes.last() {
                Some(Ok(value)) => Ok(*value),
                Some(Err(error)) => Err(error.clone()),
                None => Ok(false),
            }
        } else {
            outcomes.pop().unwrap_or(Ok(false))
        }
    }
}
