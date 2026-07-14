//! Provider trait definition.

use std::pin::Pin;

use futures_util::Stream;

use super::events::ProviderEvent;
use super::request::ProviderRequest;
use super::tools::ProviderCapabilities;
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
    /// Returns provider capabilities that affect request construction.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Sends a request to the provider and returns a stream of events.
    ///
    /// The returned stream yields `ProviderEvent` values as they arrive
    /// from the provider, ending with a `Done` event on success.
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError>;
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
