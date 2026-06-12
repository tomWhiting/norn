//! Internal helpers shared by the coordination tools.

use uuid::Uuid;

use crate::agent::registry::AgentRegistry;

/// Resolve the harness attribution for an outbound message from registry
/// ground truth: `(label, role)`.
///
/// - A live registry entry yields its hierarchical path and its role
///   (every registered agent has one set at spawn; forks carry `"fork"`).
/// - A reclaimed sender yields its tombstone path; tombstones carry no
///   role, so none is attributed.
/// - An unregistered sender with no parent is the root agent — the
///   literal `root` (root agents are never registry entries).
/// - Anything else falls back to the bare UUID. This should not occur
///   mid-call but is handled rather than relying on `.expect`.
///
/// The sending *model* never controls these values; they feed the
/// `<agent_message>` frame attributes and the `agent_message.sent` audit
/// event.
pub(super) fn sender_attribution(
    registry: &AgentRegistry,
    sender_id: Uuid,
    parent_id: Option<Uuid>,
) -> (String, Option<String>) {
    if let Some(entry) = registry.get(sender_id) {
        return (entry.path, Some(entry.role));
    }
    if let Some(tombstone) = registry.tombstone(sender_id) {
        return (tombstone.path, None);
    }
    if parent_id.is_none() {
        return ("root".to_owned(), None);
    }
    (sender_id.to_string(), None)
}
