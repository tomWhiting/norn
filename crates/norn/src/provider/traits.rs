//! Provider trait definition.

use std::pin::Pin;

use futures_util::Stream;

use super::events::ProviderEvent;
use super::request::{Message, ProviderRequest};
use super::state_identity::ProviderStateIdentity;
use super::tools::ProviderCapabilities;
use super::turn::ProviderTurnContext;
use crate::error::ProviderError;

/// A stream of provider events.
pub type ProviderStream = Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>;

/// Abstraction over an LLM provider.
///
/// Provider instances are shared across agents via `Arc<dyn Provider>`.
/// Each distinct configuration (API key, endpoint, model family) gets
/// one instance.
///
/// The trait is object-safe: all methods take `&self` and the async
/// `stream` method returns a boxed stream.
pub trait Provider: Send + Sync {
    /// Stable opaque identity for credential-and-authority-scoped provider
    /// state.
    ///
    /// Providers that support response threading or private turn state return
    /// `Some`; the loop rejects those stateful paths when the identity is
    /// absent. Stateless providers return `None`.
    fn state_identity(&self) -> Option<ProviderStateIdentity> {
        None
    }

    /// Returns provider capabilities that affect request construction.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Validate provider-specific replay requirements before the caller makes
    /// durable mutations for a new turn.
    ///
    /// The default accepts every provider-neutral message shape. Providers
    /// whose opaque state must be replayed exactly override this method; the
    /// request serializer remains a second defensive validation boundary.
    ///
    /// # Errors
    ///
    /// Returns a typed provider error when the supplied request view cannot be
    /// replayed without losing provider-owned state.
    fn validate_replay(&self, _messages: &[Message]) -> Result<(), ProviderError> {
        Ok(())
    }

    /// Sends a request to the provider and returns a stream of events.
    ///
    /// The returned stream yields `ProviderEvent` values as they arrive
    /// from the provider, ending with a `Done` event on success.
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError>;

    /// Sends a request within one live logical user turn.
    ///
    /// Providers without turn-scoped transport semantics use the ordinary
    /// [`Self::stream`] path. The Responses provider overrides this to retain
    /// private Codex sticky-routing state without putting it in the request or
    /// persisted transcript.
    fn stream_with_context(
        &self,
        request: ProviderRequest,
        _context: ProviderTurnContext,
    ) -> Result<ProviderStream, ProviderError> {
        self.stream(request)
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
    use super::*;
    use std::sync::Arc;

    fn _assert_object_safe(_: Arc<dyn Provider>) {}
}
