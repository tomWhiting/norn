//! [`SessionSpec`] — how
//! [`AgentBuilder::open_session`](crate::agent::builder::AgentBuilder::open_session)
//! brings a persisted session to life through a
//! [`SessionManager`].
//!
//! The spec is deliberately metadata-light: the index entry's `model`
//! and `working_dir` are filled in at build time from the *resolved*
//! builder state, so the persisted record always matches what the agent
//! actually ran with — callers never duplicate (or contradict) the
//! builder's own configuration.

use crate::session::store::DurabilityPolicy;
use crate::session::{CreateSessionOptions, OpenSession, SessionManager, SessionPersistError};

/// Which session-lifecycle operation
/// [`AgentBuilder::open_session`](crate::agent::builder::AgentBuilder::open_session)
/// performs at build time. Mirrors the four front doors of
/// [`SessionManager`].
#[derive(Clone, Debug)]
pub enum SessionSpec {
    /// Create a fresh session under a newly minted UUID v7 id.
    Create {
        /// Optional human-readable name recorded in the index entry.
        name: Option<String>,
    },
    /// Resume a persisted session (exact id, exact name, unique id
    /// prefix, or empty string for the most recently updated session).
    Resume {
        /// The identifier to resolve.
        id_or_name: String,
    },
    /// Fork a persisted session into a new one carrying the copied
    /// history plus a fork marker.
    Fork {
        /// The source session to fork (resolves like
        /// [`SessionSpec::Resume`]).
        source: String,
        /// Optional human-readable name for the new session.
        name: Option<String>,
    },
    /// Idempotently open the session with this exact caller-supplied id:
    /// create it when absent, resume it when present. The retry-safe
    /// primitive for embedders that derive the id deterministically from
    /// a unit of work.
    OpenOrResume {
        /// The exact session id (also the `{id}.jsonl` file name).
        id: String,
    },
}

/// A deferred session-open request stored on the builder: the manager,
/// the spec, and the explicit durability policy. Executed during
/// `build()` once the model and working directory are resolved.
pub(super) struct SessionRequest {
    pub(super) manager: SessionManager,
    pub(super) spec: SessionSpec,
    pub(super) durability: DurabilityPolicy,
}

impl SessionRequest {
    /// Execute the request against the manager, recording the resolved
    /// `model` and `working_dir` on any newly created index entry.
    pub(super) fn open(
        self,
        model: &str,
        working_dir: &str,
    ) -> Result<OpenSession, SessionPersistError> {
        let options = |name: Option<String>| CreateSessionOptions {
            model: model.to_owned(),
            working_dir: working_dir.to_owned(),
            name,
        };
        match self.spec {
            SessionSpec::Create { name } => self.manager.create(options(name), self.durability),
            SessionSpec::Resume { id_or_name } => self.manager.resume(&id_or_name, self.durability),
            SessionSpec::Fork { source, name } => {
                self.manager.fork(&source, options(name), self.durability)
            }
            SessionSpec::OpenOrResume { id } => {
                self.manager
                    .open_or_resume(&id, options(None), self.durability)
            }
        }
    }
}
