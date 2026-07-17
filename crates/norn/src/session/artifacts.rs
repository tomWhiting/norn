//! Private, session-owned artifacts that tools may persist outside the workspace.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::session::persistence::index::with_registered_generation;
use crate::session::persistence::io::ensure_session_id_path_safe;
use crate::session::persistence::{SessionIndexEntry, SessionPersistError};
use crate::session::spool::registered_root_session_id;
use crate::session::store::DurabilityPolicy;
use crate::util::PrivateRoot;

const ARTIFACTS_DIR_NAME: &str = "artifacts";
const FETCHED_DIR_NAME: &str = "fetched";

/// Narrow authority for artifacts owned by one persisted root session.
///
/// The capability exposes only artifact operations and the model-readable
/// artifact directory. It does not expose the enclosing session directory,
/// transcripts, indexes, spooled tool results, or credentials.
#[derive(Debug)]
pub struct SessionArtifactStore {
    data_dir: PathBuf,
    registered: SessionIndexEntry,
    root_session_id: String,
    index_lock_deadline: Option<std::time::Duration>,
    fsync: bool,
}

impl SessionArtifactStore {
    /// Create the private artifact tree for an owning root session.
    pub(crate) fn for_session(
        data_dir: &Path,
        registered: &SessionIndexEntry,
        durability: DurabilityPolicy,
        index_lock_deadline: Option<std::time::Duration>,
    ) -> Result<Self, SessionPersistError> {
        let root_session_id = registered_root_session_id(registered);
        ensure_session_id_path_safe(root_session_id)?;
        let store = Self {
            data_dir: data_dir.to_path_buf(),
            registered: registered.clone(),
            root_session_id: root_session_id.to_owned(),
            index_lock_deadline,
            fsync: durability != DurabilityPolicy::Flush,
        };
        with_registered_generation(data_dir, registered, index_lock_deadline, |root| {
            root.create_dir_all(&store.artifacts_relative_dir())?;
            Ok(())
        })?;
        Ok(store)
    }

    /// Absolute root a confined model may read for this session's artifacts.
    #[must_use]
    pub fn readable_root(&self) -> PathBuf {
        self.data_dir.join(self.artifacts_relative_dir())
    }

    /// Persist one immutable fetched-document artifact.
    ///
    /// Every invocation receives a fresh UUID filename and uses exclusive
    /// creation, so repeated or concurrent fetches never overwrite history.
    pub(crate) fn write_fetched(
        &self,
        source_url: &str,
        content: &str,
    ) -> Result<PathBuf, SessionPersistError> {
        with_registered_generation(
            &self.data_dir,
            &self.registered,
            self.index_lock_deadline,
            |root| self.write_fetched_under(root, source_url, content),
        )
    }

    fn write_fetched_under(
        &self,
        data_root: &PrivateRoot,
        source_url: &str,
        content: &str,
    ) -> Result<PathBuf, SessionPersistError> {
        let artifacts_dir = self.artifacts_relative_dir();
        let fetched_dir = artifacts_dir.join(FETCHED_DIR_NAME);
        data_root.create_dir_all(&fetched_dir)?;

        let filename = format!("{}.md", Uuid::new_v4());
        let relative = fetched_dir.join(filename);
        let mut file = data_root.create_new(&relative)?;
        let escaped_url = serde_json::to_string(source_url)?;
        let fetched_at = serde_json::to_string(&chrono::Utc::now().to_rfc3339())?;
        write!(
            file,
            "---\nurl: {escaped_url}\nfetched: {fetched_at}\n---\n\n{content}"
        )?;

        if self.fsync {
            file.sync_all()?;
            data_root.sync_dir(&fetched_dir)?;
            data_root.sync_dir(&artifacts_dir)?;
            data_root.sync_dir(Path::new(&self.root_session_id))?;
            data_root.sync_dir(Path::new(""))?;
        }
        Ok(data_root.display_path(&relative))
    }

    fn artifacts_relative_dir(&self) -> PathBuf {
        PathBuf::from(&self.root_session_id).join(ARTIFACTS_DIR_NAME)
    }
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
