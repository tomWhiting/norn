//! Internal helpers shared by the coordination tools.

use uuid::Uuid;

use crate::agent::registry::AgentRegistry;

/// Resolve a human-readable author label for an outbound message.
///
/// Prefers the sender's hierarchical registry path; falls back to the bare
/// UUID when the entry has been dropped (which should not happen mid-call
/// but is handled to avoid relying on `.expect`).
pub(super) fn sender_label(registry: &AgentRegistry, sender_id: Uuid) -> String {
    registry
        .get(sender_id)
        .map_or_else(|| sender_id.to_string(), |entry| entry.path)
}
