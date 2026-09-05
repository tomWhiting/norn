//! Norn operating defaults chosen by Tom on 5 September 2026, Melbourne time.

use super::{CatalogBackend, supports_effort};
use crate::provider::request::ReasoningEffort;

/// Astra context policy chosen from the owner's proposed values; not provider metadata.
pub const ASTRA_CONTEXT_WINDOW: u64 = 372_000;

/// Derive Astra's operating window only on its declared Codex route.
#[must_use]
pub fn context_window(backend: Option<CatalogBackend>, model: &str) -> Option<u64> {
    (backend == Some(CatalogBackend::CODEX) && model == "gpt-6-astra")
        .then_some(ASTRA_CONTEXT_WINDOW)
}

/// CLI reasoning default where the selected Codex model declares High support.
/// Explicit settings, profile and CLI values take precedence at the caller.
#[must_use]
pub fn reasoning_effort(backend: Option<CatalogBackend>, model: &str) -> Option<ReasoningEffort> {
    (backend == Some(CatalogBackend::CODEX)
        && supports_effort(backend, model, ReasoningEffort::High))
    .then_some(ReasoningEffort::High)
}
