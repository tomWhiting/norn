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
use crate::session::{
    CreateSessionOptions, OpenSession, ResumePolicy, SessionManager, SessionPersistError,
};

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
        /// Explicit fidelity policy; ordinary callers use canonical-only.
        policy: ResumePolicy,
    },
    /// Resume the most recently updated persisted session whose indexed
    /// working directory matches `working_dir` — the no-argument
    /// `--resume` sentinel. Scoped to the current project, never the
    /// globally most-recently-updated session across every directory
    /// (which would cross-contaminate unrelated projects).
    ResumeLatestInWorkingDir {
        /// The working directory whose latest session to resume.
        working_dir: String,
        /// Explicit fidelity policy; ordinary callers use canonical-only.
        policy: ResumePolicy,
    },
    /// Fork a persisted session into a new one carrying the copied
    /// history plus a fork marker.
    Fork {
        /// The source session to fork (resolves like
        /// [`SessionSpec::Resume`]).
        source: String,
        /// Optional human-readable name for the new session.
        name: Option<String>,
        /// Explicit source-fidelity policy.
        policy: ResumePolicy,
    },
    /// Fork the most recently updated persisted session whose indexed
    /// working directory matches `working_dir` — the no-argument
    /// `--fork` sentinel. Mirrors [`SessionSpec::ResumeLatestInWorkingDir`]:
    /// the source is selected within the current project, never globally.
    ForkLatestInWorkingDir {
        /// The working directory whose latest session to fork.
        working_dir: String,
        /// Explicit source-fidelity policy.
        policy: ResumePolicy,
    },
    /// Idempotently open the session with this exact caller-supplied id:
    /// create it when absent, resume it when present. The retry-safe
    /// primitive for embedders that derive the id deterministically from
    /// a unit of work.
    OpenOrResume {
        /// The exact session id (also the `{id}.jsonl` file name).
        id: String,
        /// Explicit policy for the existing-session arm.
        policy: ResumePolicy,
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

impl SessionSpec {
    /// Construct an ordinary canonical-only resume request.
    pub fn resume(id_or_name: impl Into<String>) -> Self {
        Self::Resume {
            id_or_name: id_or_name.into(),
            policy: ResumePolicy::RequireCanonical,
        }
    }

    /// Construct a resume request with explicit degraded-history approval.
    pub fn resume_with_policy(id_or_name: impl Into<String>, policy: ResumePolicy) -> Self {
        Self::Resume {
            id_or_name: id_or_name.into(),
            policy,
        }
    }

    /// Construct an ordinary canonical-only latest-project resume request.
    pub fn resume_latest(working_dir: impl Into<String>) -> Self {
        Self::ResumeLatestInWorkingDir {
            working_dir: working_dir.into(),
            policy: ResumePolicy::RequireCanonical,
        }
    }

    /// Construct a latest-project resume request with an explicit policy.
    pub fn resume_latest_with_policy(working_dir: impl Into<String>, policy: ResumePolicy) -> Self {
        Self::ResumeLatestInWorkingDir {
            working_dir: working_dir.into(),
            policy,
        }
    }

    /// Construct an ordinary canonical-only fork request.
    pub fn fork(source: impl Into<String>, name: Option<String>) -> Self {
        Self::Fork {
            source: source.into(),
            name,
            policy: ResumePolicy::RequireCanonical,
        }
    }

    /// Construct a fork request with an explicit source-fidelity policy.
    pub fn fork_with_policy(
        source: impl Into<String>,
        name: Option<String>,
        policy: ResumePolicy,
    ) -> Self {
        Self::Fork {
            source: source.into(),
            name,
            policy,
        }
    }

    /// Construct an ordinary canonical-only latest-project fork request.
    pub fn fork_latest(working_dir: impl Into<String>) -> Self {
        Self::ForkLatestInWorkingDir {
            working_dir: working_dir.into(),
            policy: ResumePolicy::RequireCanonical,
        }
    }

    /// Construct a latest-project fork with an explicit source policy.
    pub fn fork_latest_with_policy(working_dir: impl Into<String>, policy: ResumePolicy) -> Self {
        Self::ForkLatestInWorkingDir {
            working_dir: working_dir.into(),
            policy,
        }
    }

    /// Construct an ordinary canonical-only idempotent open request.
    pub fn open_or_resume(id: impl Into<String>) -> Self {
        Self::OpenOrResume {
            id: id.into(),
            policy: ResumePolicy::RequireCanonical,
        }
    }

    /// Construct an idempotent open with explicit policy for its resume arm.
    pub fn open_or_resume_with_policy(id: impl Into<String>, policy: ResumePolicy) -> Self {
        Self::OpenOrResume {
            id: id.into(),
            policy,
        }
    }
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
            SessionSpec::Resume { id_or_name, policy } => {
                self.manager
                    .resume_with_policy(&id_or_name, self.durability, policy)
            }
            SessionSpec::ResumeLatestInWorkingDir {
                working_dir,
                policy,
            } => self.manager.resume_latest_in_working_dir_with_policy(
                &working_dir,
                self.durability,
                policy,
            ),
            SessionSpec::Fork {
                source,
                name,
                policy,
            } => self
                .manager
                .fork_with_policy(&source, options(name), self.durability, policy),
            SessionSpec::ForkLatestInWorkingDir {
                working_dir,
                policy,
            } => self.manager.fork_latest_in_working_dir_with_policy(
                &working_dir,
                options(None),
                self.durability,
                policy,
            ),
            SessionSpec::OpenOrResume { id, policy } => {
                self.manager
                    .open_or_resume_with_policy(&id, options(None), self.durability, policy)
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
    fn resume_latest_in_working_dir_spec_ignores_global_latest_elsewhere()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let manager = SessionManager::new(tmp.path());
        let current_id = seed(&manager, "/repo/current", "current work");

        std::thread::sleep(std::time::Duration::from_millis(5));
        let other_id = seed(&manager, "/repo/other", "other work");

        let opened = request(&manager, SessionSpec::resume_latest("/repo/current"))
            .open("test-model", "/repo/current")?;
        assert_eq!(
            opened.entry.id, current_id,
            "must resume the current-dir session, not globally newer {other_id}",
        );
        Ok(())
    }

    /// Regression (F1): the empty-`--fork` sentinel routes through
    /// [`SessionSpec::ForkLatestInWorkingDir`] and forks the latest session
    /// *for the given working directory*, copying that session's history
    /// into the new fork — never a globally newer session elsewhere.
    #[test]
    fn fork_latest_in_working_dir_spec_forks_current_directory_source()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let manager = SessionManager::new(tmp.path());
        let current_id = seed(&manager, "/repo/current", "current source");

        std::thread::sleep(std::time::Duration::from_millis(5));
        let _other_id = seed(&manager, "/repo/other", "other source");

        let opened = request(&manager, SessionSpec::fork_latest("/repo/current"))
            .open("test-model", "/repo/current")?;
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
        Ok(())
    }

    #[test]
    fn explicit_fresh_epoch_policy_reaches_manager() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let manager = SessionManager::new(temp.path());
        let opened = manager.create(
            CreateSessionOptions {
                model: "test-model".to_owned(),
                working_dir: "/repo/current".to_owned(),
                name: None,
            },
            DurabilityPolicy::Flush,
        )?;
        let id = opened.entry.id.clone();
        drop(opened);
        crate::session::update_index_entry(manager.data_dir(), &id, None, |entry| {
            entry.fidelity = crate::session::ResumeFidelity::FreshEpochProjection;
            entry.origin = crate::session::SessionRecordOrigin::MigratedLegacy {
                source_format: 1,
                source_sha256: "a".repeat(64),
            };
        })?;

        let approved = request(
            &manager,
            SessionSpec::resume_with_policy(&id, ResumePolicy::ApproveFreshEpochProjection),
        )
        .open("test-model", "/repo/current")?;
        assert!(approved.store.events().iter().any(|event| matches!(
            event,
            crate::session::events::SessionEvent::ProviderEpochBoundary {
                reason: crate::session::events::ProviderEpochBoundaryReason::MigratedLegacy,
                ..
            }
        )));
        Ok(())
    }
}
