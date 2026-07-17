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
use crate::session::{SessionArtifactStore, SessionBinding, SessionBrancher, SessionIndexEntry};

/// The opened persisted root session: its index entry and replay summary
/// (surfaced on the built agent), the replayed store, and the root's
/// session-branching identity every child mint under this agent routes
/// through.
pub(super) struct OpenedRootSession {
    /// The session's index entry, recording the values the agent
    /// actually runs with.
    pub(super) entry: SessionIndexEntry,
    /// Replay summary from strict format-2 decoding.
    pub(super) replay: ReplaySummary,
    /// The replayed, sink-equipped event store.
    pub(super) store: Arc<EventStore>,
    /// The root's persistent [`SessionBinding`] (child-persistence V2):
    /// the single allocation authority every spawn/fork/rhai child mint
    /// under this agent routes through.
    pub(super) binding: Arc<SessionBinding>,
    /// Private artifact authority shared by this root and its descendants.
    pub(super) artifacts: Arc<SessionArtifactStore>,
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
    let opened = request
        .open(model, working_dir)
        .map_err(|error| map_session_open_error("open_session failed", error))?;
    // Heal a transcript killed mid-turn before the first provider request is
    // ever assembled from it: a persisted assistant turn whose tool result
    // never landed (hard kill in the window between the assistant turn and
    // its ToolResult) would otherwise replay as a `function_call` with no
    // `function_call_output`, which the Responses API rejects with HTTP 400
    // on every retry. Runs on every open; it is a no-op on a freshly created
    // (empty) session and on any already-well-formed log, so a healthy
    // session file is left untouched. The synthetic results persist through
    // the store's write-through sink, so the reopened session is well-formed
    // on disk, not just in memory.
    let repaired =
        crate::session::repair_dangling_tool_calls(&opened.store).map_err(NornError::Session)?;
    if !repaired.is_empty() {
        tracing::warn!(
            session_id = %opened.entry.id,
            repaired_tool_calls = repaired.len(),
            call_ids = ?repaired,
            "resume repair: synthesized interrupted-tool-call results for a \
             transcript killed mid-turn; the reopened session is now \
             well-formed",
        );
    }
    let root_for_children =
        crate::session::spool::registered_root_session_id(&opened.entry).to_owned();
    let artifacts = Arc::new(
        SessionArtifactStore::for_session(
            manager.data_dir(),
            &opened.entry,
            durability,
            manager.index_lock_deadline(),
        )
        .map_err(|error| map_session_open_error("open_session artifact storage failed", error))?,
    );
    let brancher = Arc::new(SessionBrancher::new(manager, root_for_children, durability));
    let binding = Arc::new(SessionBinding::persistent_root(
        brancher,
        &opened.entry,
        &opened.store.events(),
    ));
    Ok(OpenedRootSession {
        entry: opened.entry,
        replay: opened.replay,
        store: Arc::new(opened.store),
        binding,
        artifacts,
    })
}

fn map_session_open_error(context: &str, error: crate::session::SessionPersistError) -> NornError {
    match error {
        crate::session::SessionPersistError::DescriptorExhausted(source) => {
            NornError::Session(SessionError::DescriptorExhausted(source))
        }
        other => NornError::Session(SessionError::StorageError {
            reason: format!("{context}: {other}"),
        }),
    }
}
