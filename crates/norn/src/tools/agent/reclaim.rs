//! Terminal-entry reclamation for naturally-finished children.
//!
//! # Reclamation ownership
//!
//! A child's terminal [`crate::agent::registry::AgentRegistry`] entry and
//! its parent-held [`AgentHandle`](super::handle::AgentHandle) are
//! reclaimed by exactly one owner per launch path:
//!
//! - **`close_agent`** — the closer reclaims immediately (it owns the
//!   lifecycle end it forced). For children it holds the handle of, the
//!   closer cancels the run token (cascading to the whole spawned
//!   subtree since W3.5 — never an abort) and *joins* the wrapper before
//!   touching the registry, so the wrapper and the closer can never race
//!   over the terminal transition. Reclamation leaves an
//!   [`AgentTombstone`](crate::agent::registry::AgentTombstone) so the
//!   completion stays reportable.
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
//! # Single-owner terminal sequence
//!
//! The spawn/fork completion wrapper is the **sole owner** of a
//! naturally-finishing child's terminal sequence: registry mark →
//! completion event → lifecycle emit → result delivery → status broadcast
//! → reclamation, strictly in that order on the wrapper task. The
//! launching tool never reclaims from its own execute path; instead it
//! signals the wrapper through an explicit handshake (a oneshot ack sent
//! after [`AgentHandles::insert`](super::handle::AgentHandles::insert)),
//! and the wrapper defers its reclamation pass until that ack arrives —
//! so the wrapper is guaranteed to find the handle even when the child
//! finishes before the tool has stored it. Nothing infers lifecycle
//! state from a registry entry's *absence*: an entry missing at
//! terminal-transition time is an invariant violation and is logged as
//! an error, never silently tolerated.
//!
//! Under a persistent parent the child's audit trail outlives
//! reclamation — its store writes through to a real on-disk timeline
//! under the root's `children/` directory. Under an ephemeral parent the
//! delivered result message is the parent's record; dropping the handle
//! releases the child's memory-only store. Either way
//! the registry retains an
//! [`AgentTombstone`](crate::agent::registry::AgentTombstone) so
//! coordination tools can report the completion honestly for the rest of
//! the session.

use std::sync::Arc;

use parking_lot::RwLock;
use uuid::Uuid;

use super::handle::AgentHandles;
use crate::agent::registry::{AgentRegistry, StatusTransitionError};

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

/// Wrapper-side half of the launch handshake for delivery-anchored
/// reclamation.
///
/// Built by the launching tool when [`ReclaimOnResultDelivery`] is in
/// force: the parent's handle map to reclaim from, plus the receiver the
/// tool resolves immediately after
/// [`AgentHandles::insert`](super::handle::AgentHandles::insert) has
/// stored the child's handle. The completion wrapper awaits
/// [`Self::handle_installed`] before its reclamation pass, so even a
/// child that finishes before the tool has stored the handle is
/// reclaimed by exactly one owner — the wrapper — with the handle
/// guaranteed to be present. The receiver completes with an error only
/// when the tool's execute was torn down before storing the handle
/// (e.g. the parent task was cancelled mid-launch); the wrapper then
/// reclaims the registry entry alone.
pub(super) struct ReclaimHandshake {
    /// The launching agent's handle map.
    pub(super) handles: Arc<AgentHandles>,
    /// Resolved by the launching tool once the child's handle is stored.
    pub(super) handle_installed: tokio::sync::oneshot::Receiver<()>,
}

/// Log a failed terminal transition as the invariant violation it is.
///
/// The completion wrapper is the sole owner of a live child's terminal
/// transition: `close_agent` joins the wrapper before touching the
/// registry, [`SpawnGuard`](crate::agent::registry::SpawnGuard) rollback
/// only ever removes `Spawning` reservations, and
/// [`AgentRegistry::remove_terminal`] never removes non-terminal
/// entries. A transition failure therefore means another actor mutated
/// an entry it does not own; the log carries the entry's
/// [`AgentTombstone`](crate::agent::registry::AgentTombstone) when one
/// exists so the conflicting owner is identifiable. `surface` names the
/// launch surface (`"fork"` / `"spawn_agent"`) for log correlation.
pub(crate) fn log_terminal_transition_violation(
    registry: &AgentRegistry,
    child_id: Uuid,
    surface: &str,
    error: &StatusTransitionError,
) {
    if let Some(tombstone) = registry.tombstone(child_id) {
        tracing::error!(
            child_id = %child_id,
            error = %error,
            tombstone_path = %tombstone.path,
            tombstone_status = ?tombstone.status,
            tombstone_completed_at = %tombstone.completed_at,
            "{surface}: invariant violation: terminal transition failed — the entry \
             was already terminally recorded and reclaimed by another actor",
        );
    } else {
        tracing::error!(
            child_id = %child_id,
            error = %error,
            "{surface}: invariant violation: terminal transition failed — the \
             completion wrapper is the sole owner of this entry's terminal sequence",
        );
    }
}

/// Reclaim one delivered child: drop the parent-held
/// [`AgentHandle`](super::handle::AgentHandle) (if still tracked) and
/// remove the child's terminal registry entry, leaving an
/// [`AgentTombstone`](crate::agent::registry::AgentTombstone).
///
/// Called exclusively from the spawn/fork completion wrapper, after the
/// terminal mark, result delivery, and the handle-installed ack from the
/// launching tool (see the module docs). Idempotent —
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
