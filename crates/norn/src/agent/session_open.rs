//! Managed-session opening for
//! [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build).
//!
//! Split out of `agent/builder.rs` per the 500-line production budget:
//! this owns the `open_session` half of the build — opening (create /
//! resume / fork) the persisted root session and minting the root's
//! session-branching identity — while the builder keeps the assembly
//! sequencing.

use std::sync::Arc;

use crate::agent::session_spec::SessionRequest;
use crate::error::{NornError, SessionError};
use crate::session::manager::ReplaySummary;
use crate::session::store::EventStore;
use crate::session::{SessionBinding, SessionBrancher, SessionIndexEntry};

/// The opened persisted root session: its index entry and replay summary
/// (surfaced on the built agent), the replayed store, and the root's
/// session-branching identity every child mint under this agent routes
/// through.
pub(super) struct OpenedRootSession {
    /// The session's index entry, recording the values the agent
    /// actually runs with.
    pub(super) entry: SessionIndexEntry,
    /// Replay summary from the tolerant reader.
    pub(super) replay: ReplaySummary,
    /// The replayed, sink-equipped event store.
    pub(super) store: Arc<EventStore>,
    /// The root's persistent [`SessionBinding`] (child-persistence V2):
    /// the single allocation authority every spawn/fork/rhai child mint
    /// under this agent routes through.
    pub(super) binding: Arc<SessionBinding>,
}

/// Open the managed persisted session once the model and working
/// directory are resolved — the index entry records the values the agent
/// actually runs with — and mint the root's persistent branching binding.
///
/// The manager and fsync cadence survive the open: the child brancher
/// applies the SAME data dir, lock deadline, and durability the root
/// itself runs with — inherited, never invented here. The `children/`
/// directory is keyed by the ROOT session id: for a flat (root) session
/// that is the session's own id; resuming a nested child session directly
/// keeps branching into the same root-keyed directory its `rel_path`
/// points into, so grandchild files keep their full-path slugs across
/// restarts. Replayed history seeds the ever-used child-name set (Q2
/// for-all-time uniqueness) and recovers a resumed child's own path
/// address from its provenance header.
///
/// # Errors
///
/// [`NornError::Session`] when the request fails to create, resume, or
/// fork the persisted session.
pub(super) fn open_root_session(
    request: SessionRequest,
    model: &str,
    working_dir: &str,
) -> Result<OpenedRootSession, NornError> {
    let manager = request.manager.clone();
    let durability = request.durability;
    let opened = request.open(model, working_dir).map_err(|e| {
        NornError::Session(SessionError::StorageError {
            reason: format!("open_session failed: {e}"),
        })
    })?;
    if opened.replay.skipped_lines > 0 {
        tracing::warn!(
            session_id = %opened.entry.id,
            skipped_lines = opened.replay.skipped_lines,
            "open_session: tolerant reader skipped lines — the replayed \
             session history is incomplete",
        );
    }
    let root_for_children = opened
        .entry
        .rel_path
        .as_deref()
        .and_then(|rel| rel.split('/').next())
        .map_or_else(|| opened.entry.id.clone(), str::to_owned);
    let brancher = Arc::new(SessionBrancher::new(manager, root_for_children, durability));
    let binding = Arc::new(SessionBinding::persistent_root(
        brancher,
        opened.entry.id.clone(),
        &opened.store.events(),
    ));
    Ok(OpenedRootSession {
        entry: opened.entry,
        replay: opened.replay,
        store: Arc::new(opened.store),
        binding,
    })
}
