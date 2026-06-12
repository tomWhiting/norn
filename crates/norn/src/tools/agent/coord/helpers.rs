//! Internal helpers shared by the coordination tools.

use uuid::Uuid;

use crate::agent::registry::AgentRegistry;

/// Resolve a human-readable author label for an outbound message.
///
/// Prefers the sender's hierarchical registry path, then the path on its
/// completion record if the entry was reclaimed; falls back to the bare
/// UUID only when no record exists (which should not happen mid-call but
/// is handled to avoid relying on `.expect`).
pub(super) fn sender_label(registry: &AgentRegistry, sender_id: Uuid) -> String {
    registry
        .get(sender_id)
        .map(|entry| entry.path)
        .or_else(|| registry.tombstone(sender_id).map(|t| t.path))
        .unwrap_or_else(|| sender_id.to_string())
}
