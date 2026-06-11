//! Terminal-entry reclamation for naturally-finished children.
//!
//! # Reclamation ownership
//!
//! A child's terminal [`crate::agent::registry::AgentRegistry`] entry and
//! its parent-held [`AgentHandle`](super::handle::AgentHandle) are
//! reclaimed by exactly one owner per launch path:
//!
//! - **`close_agent`** — the closer reclaims immediately (it owns the
//!   lifecycle end it forced).
//! - **TUI / external status observers** — when *no*
//!   [`ReclaimOnResultDelivery`] marker is installed, naturally-finished
//!   children stay in the registry with terminal status so a polling
//!   observer (the TUI agent status panel) can display the outcome
//!   through its hold window and then call
//!   [`AgentRegistry::remove_terminal`](crate::agent::registry::AgentRegistry::remove_terminal)
//!   itself. This is the default because reclamation-on-delivery would
//!   race the hold window into nonexistence.
//! - **Embedded / headless runtimes** — installing
//!   [`ReclaimOnResultDelivery`] on the orchestrator's shared
//!   [`ToolContext`](crate::tool::context::ToolContext) declares that no
//!   external observer exists: once a child's
//!   [`ChildAgentResult`](crate::agent::result_channel::ChildAgentResult)
//!   has been handed to the parent's result channel (which the agent
//!   loop drains at iteration boundaries), the spawn/fork wrapper
//!   reclaims the registry entry and drops the parent-held handle, so
//!   long-running embedded processes do not pin one
//!   [`EventStore`](crate::session::store::EventStore) per finished
//!   child forever.
//! - **No result channel installed** — whoever holds the
//!   [`AgentHandle`](super::handle::AgentHandle) owns the end of life:
//!   join the handle, then reclaim explicitly. The wrapper never
//!   reclaims in this mode (there was no delivery to anchor it to), even
//!   when the marker is installed.
//!
//! In `SessionTree` mode the child's audit trail outlives reclamation —
//! the handle's `EventStore` aliases the tree's branch store. In
//! standalone mode the delivered result message is the parent's record;
//! dropping the handle releases the child's private store.

use parking_lot::RwLock;
use uuid::Uuid;

use super::handle::AgentHandles;
use crate::agent::registry::AgentRegistry;

/// Marker [`ToolContext`](crate::tool::context::ToolContext) extension:
/// reclaim a naturally-finished child's registry entry and parent-held
/// handle as soon as its result has been delivered through the
/// [`ChildResultSender`](crate::agent::result_channel::ChildResultSender).
///
/// Install via
/// [`install_terminal_reclamation`](crate::runtime_init::install_terminal_reclamation)
/// on runtimes with no external status observer (embedded / headless).
/// Do **not** install it when a polling observer such as the TUI agent
/// status panel owns reclamation — see the module docs for the full
/// ownership rule.
pub struct ReclaimOnResultDelivery;

/// Reclaim one delivered child: drop the parent-held
/// [`AgentHandle`](super::handle::AgentHandle) (if still tracked) and
/// remove the child's terminal registry entry.
///
/// Idempotent and safe to call from both the child wrapper task and the
/// spawning tool's execute path — whichever runs last completes the
/// reclamation, which is what closes the insert/finish race between
/// `AgentHandles::insert` and a fast-finishing child.
/// [`AgentRegistry::remove_terminal`] never removes non-terminal
/// entries, so a hook-suppressed (still Active) child is never reclaimed
/// by accident.
pub(super) fn reclaim_delivered_child(
    registry: &RwLock<AgentRegistry>,
    handles: &AgentHandles,
    child_id: Uuid,
) {
    // Dropping the handle detaches the (already finished or finishing)
    // child task and releases the parent's reference to the child's
    // EventStore.
    drop(handles.remove(child_id));
    registry.write().remove_terminal(child_id);
}

/// True when the registry no longer needs the parent to keep the child
/// observable: the entry is terminal (awaiting reclamation) or already
/// reclaimed. Used by the spawning tool's execute path to close the race
/// where the child finishes — and the wrapper's own reclamation runs —
/// before `AgentHandles::insert` has stored the handle.
pub(super) fn entry_terminal_or_reclaimed(
    registry: &RwLock<AgentRegistry>,
    child_id: Uuid,
) -> bool {
    registry
        .read()
        .get(child_id)
        .is_none_or(|entry| entry.status.is_terminal())
}
