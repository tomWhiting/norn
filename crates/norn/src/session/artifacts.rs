//! Private, session-owned artifacts that tools may persist outside the workspace.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::session::persistence::SessionPersistError;
use crate::session::persistence::io::ensure_session_id_path_safe;
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
    data_root: PrivateRoot,
    root_session_id: String,
    fsync: bool,
}

impl SessionArtifactStore {
    /// Create the private artifact tree for an owning root session.
    pub(crate) fn for_session(
        data_dir: &Path,
        root_session_id: &str,
        durability: DurabilityPolicy,
    ) -> Result<Self, SessionPersistError> {
        ensure_session_id_path_safe(root_session_id)?;
        let data_root = PrivateRoot::create(data_dir)?;
        let store = Self {
            data_root,
            root_session_id: root_session_id.to_owned(),
            fsync: durability != DurabilityPolicy::Flush,
        };
        store
            .data_root
            .create_dir_all(&store.artifacts_relative_dir())?;
        Ok(store)
    }

    /// Absolute root a confined model may read for this session's artifacts.
    #[must_use]
    pub fn readable_root(&self) -> PathBuf {
        self.data_root.display_path(&self.artifacts_relative_dir())
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
        let artifacts_dir = self.artifacts_relative_dir();
        let fetched_dir = artifacts_dir.join(FETCHED_DIR_NAME);
        self.data_root.create_dir_all(&fetched_dir)?;

        let filename = format!("{}.md", Uuid::new_v4());
        let relative = fetched_dir.join(filename);
        let mut file = self.data_root.create_new(&relative)?;
        let body = strip_frontmatter(content);
        let escaped_url = serde_json::to_string(source_url)?;
        let fetched_at = serde_json::to_string(&chrono::Utc::now().to_rfc3339())?;
        write!(
            file,
            "---\nurl: {escaped_url}\nfetched: {fetched_at}\n---\n\n{body}"
        )?;

        if self.fsync {
            file.sync_all()?;
            self.data_root.sync_dir(&fetched_dir)?;
            self.data_root.sync_dir(&artifacts_dir)?;
            self.data_root.sync_dir(Path::new(&self.root_session_id))?;
            self.data_root.sync_dir(Path::new(""))?;
        }
        Ok(self.data_root.display_path(&relative))
    }

    fn artifacts_relative_dir(&self) -> PathBuf {
        PathBuf::from(&self.root_session_id).join(ARTIFACTS_DIR_NAME)
    }
}

fn strip_frontmatter(content: &str) -> &str {
    if !content.starts_with("---") {
        return content;
    }
    if let Some(end) = content[3..].find("\n---") {
        let after = 3 + end + 4;
        content[after..].trim_start_matches('\n')
    } else {
        content
    }
}
