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
    /// Create a fresh session under a newly minted UUID v4 id (R8).
    Create {
        /// Optional human-readable name recorded in the index entry.
        name: Option<String>,
    },
    /// Resume a persisted session (exact id, exact name, or unique id
    /// prefix). The empty-string "latest" sentinel is *not* this variant —
    /// it is [`SessionSpec::ResumeLatestInWorkingDir`], scoped to a
    /// working directory rather than the global index.
    Resume {
        /// The identifier to resolve.
        id_or_name: String,
    },
    /// Resume the most recently updated persisted session whose indexed
    /// working directory matches `working_dir` — the no-argument
    /// `--resume` sentinel. Scoped to the current project, never the
    /// globally most-recently-updated session across every directory
    /// (which would cross-contaminate unrelated projects).
    ResumeLatestInWorkingDir {
        /// The working directory whose latest session to resume.
        working_dir: String,
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
    /// Fork the most recently updated persisted session whose indexed
    /// working directory matches `working_dir` — the no-argument
    /// `--fork` sentinel. Mirrors [`SessionSpec::ResumeLatestInWorkingDir`]:
    /// the source is selected within the current project, never globally.
    ForkLatestInWorkingDir {
        /// The working directory whose latest session to fork.
        working_dir: String,
    },
    /// Idempotently open the session with this exact caller-supplied id:
    /// create it when absent, resume it when present. The retry-safe
    /// primitive for embedders that derive the id deterministically from
    /// a unit of work.
    OpenOrResume {
        /// The exact session id (also the `{id}.jsonl` file name).
        id: String,
    },
    /// Create a fresh session under this exact caller-supplied id, failing
    /// when a session with that id already exists. The non-idempotent
    /// counterpart of [`SessionSpec::OpenOrResume`] for a CLI `--session-id`
    /// without `--resume-if-exists`.
    CreateWithId {
        /// The exact session id (also the `{id}.jsonl` file name).
        id: String,
        /// Optional human-readable name recorded in the index entry.
        name: Option<String>,
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
            SessionSpec::ResumeLatestInWorkingDir { working_dir } => self
                .manager
                .resume_latest_in_working_dir(&working_dir, self.durability),
            SessionSpec::Fork { source, name } => {
                self.manager.fork(&source, options(name), self.durability)
            }
            SessionSpec::ForkLatestInWorkingDir { working_dir } => self
                .manager
                .fork_latest_in_working_dir(&working_dir, options(None), self.durability),
            SessionSpec::OpenOrResume { id } => {
                self.manager
                    .open_or_resume(&id, options(None), self.durability)
            }
            SessionSpec::CreateWithId { id, name } => {
                self.manager
                    .create_with_id(&id, options(name), self.durability)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::session::CreateSessionOptions;

    fn request(manager: &SessionManager, spec: SessionSpec) -> SessionRequest {
        SessionRequest {
            manager: manager.clone(),
            spec,
            durability: DurabilityPolicy::Flush,
        }
    }

    fn seed(manager: &SessionManager, working_dir: &str, content: &str) -> String {
        let opened = manager
            .create(
                CreateSessionOptions {
                    model: "test-model".to_owned(),
                    working_dir: working_dir.to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .unwrap();
        let id = opened.entry.id.clone();
        opened
            .store
            .append(crate::session::events::SessionEvent::UserMessage {
                base: crate::session::events::EventBase::new(None),
                content: content.to_owned(),
            })
            .unwrap();
        id
    }

    /// Regression (F1): the empty-`--resume` sentinel routes through
    /// [`SessionSpec::ResumeLatestInWorkingDir`] and resolves to the latest
    /// session *for the given working directory*, never the globally
    /// most-recently-updated session in another directory.
    #[test]
    fn resume_latest_in_working_dir_spec_ignores_global_latest_elsewhere() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let current_id = seed(&manager, "/repo/current", "current work");

        std::thread::sleep(std::time::Duration::from_millis(5));
        let other_id = seed(&manager, "/repo/other", "other work");

        let opened = request(
            &manager,
            SessionSpec::ResumeLatestInWorkingDir {
                working_dir: "/repo/current".to_owned(),
            },
        )
        .open("test-model", "/repo/current")
        .unwrap();
        assert_eq!(
            opened.entry.id, current_id,
            "must resume the current-dir session, not globally newer {other_id}",
        );
    }

    /// Regression (F1): the empty-`--fork` sentinel routes through
    /// [`SessionSpec::ForkLatestInWorkingDir`] and forks the latest session
    /// *for the given working directory*, copying that session's history
    /// into the new fork — never a globally newer session elsewhere.
    #[test]
    fn fork_latest_in_working_dir_spec_forks_current_directory_source() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let current_id = seed(&manager, "/repo/current", "current source");

        std::thread::sleep(std::time::Duration::from_millis(5));
        let _other_id = seed(&manager, "/repo/other", "other source");

        let opened = request(
            &manager,
            SessionSpec::ForkLatestInWorkingDir {
                working_dir: "/repo/current".to_owned(),
            },
        )
        .open("test-model", "/repo/current")
        .unwrap();
        assert_ne!(opened.entry.id, current_id, "fork mints a new session id");
        let events = opened.store.events();
        assert!(
            events.iter().any(|event| matches!(
                event,
                crate::session::events::SessionEvent::UserMessage { content, .. }
                    if content == "current source"
            )),
            "fork must copy the current-directory source history, not the other dir",
        );
    }
}
