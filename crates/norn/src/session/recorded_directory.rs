//! Read-only recorded identities; listing never opens child timelines or creates live agents.

use serde::Serialize;
use uuid::Uuid;

use super::{Persistence, SessionBinding};
use crate::session::persistence::index::registered_subtree;
use crate::session::persistence::types::SessionPersistError;

/// A registered session identity, not a live agent or a messaging capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecordedSession {
    /// Persisted session ID; never substituted with a runtime agent ID.
    pub session_id: String,
    /// Immutable incarnation needed to reject replacement on subsequent reads.
    pub generation: Uuid,
    /// Recorded parent, absent on the listing root to avoid exposing its ancestors.
    pub parent_session_id: Option<String>,
    /// Optional persisted name; not guaranteed unique or a registry address.
    pub name: Option<String>,
    /// Recorded project directory, without inferring that it still exists.
    pub working_dir: String,
}

/// Explicit directory coverage. Registered rows do not prove timeline availability,
/// live recipients, or completeness of ephemeral and unjournaled branch reservations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedSessionDirectory {
    /// Caller has no persistent identity, so it has no registered directory.
    Ephemeral,
    /// Index snapshot in preorder, caller first. No child timeline was inspected.
    Registered {
        /// Only the caller and registered descendants at this locked snapshot.
        sessions: Vec<RecordedSession>,
    },
}

impl SessionBinding {
    /// Discover this incarnation's registered subtree on demand without loading histories.
    ///
    /// # Errors
    /// Refuses stale caller generations, invalid index data, reachable parent cycles,
    /// lock deadlines and filesystem errors. Missing child files are not probed here.
    pub fn recorded_directory(&self) -> Result<RecordedSessionDirectory, SessionPersistError> {
        let Persistence::Persistent {
            brancher,
            registered,
        } = &self.persistence
        else {
            return Ok(RecordedSessionDirectory::Ephemeral);
        };
        let rows = registered_subtree(
            brancher.manager.data_dir(),
            registered,
            brancher.manager.index_lock_deadline(),
        )?;
        let sessions = rows
            .into_iter()
            .enumerate()
            .map(|(position, entry)| RecordedSession {
                session_id: entry.id,
                generation: entry.generation,
                parent_session_id: if position == 0 { None } else { entry.parent_id },
                name: entry.name,
                working_dir: entry.working_dir,
            })
            .collect();
        Ok(RecordedSessionDirectory::Registered { sessions })
    }
}

#[cfg(test)]
#[path = "recorded_directory_tests.rs"]
mod tests;
