use uuid::Uuid;

use crate::session::branch::ROOT_PATH_ADDRESS;
use crate::session::events::{ChildBranchKind, EventBase, SessionEvent};
use crate::session::persistence::index::{
    publish_new_session, resolve_latest_session_in_working_dir_with_deadline,
    resolve_session_with_deadline, revalidate_registered_entry,
};
use crate::session::persistence::read_session_events_for_entry_with_deadline;
use crate::session::persistence::{SessionIndexEntry, SessionPersistError};
use crate::session::spool::SpoolWriter;
use crate::session::store::{DurabilityPolicy, EventStore, JsonlSink};

use super::open::new_index_entry;
use super::resume_policy::{authorize_resume, ensure_migrated_epoch_boundary_in_events};
use super::{CreateSessionOptions, OpenSession, ReplaySummary, ResumePolicy, SessionManager};

impl SessionManager {
    /// Fork a canonical source into a new persisted session.
    pub fn fork(
        &self,
        source: &str,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        self.fork_with_policy(source, options, durability, ResumePolicy::RequireCanonical)
    }

    /// Fork a source under an explicit fidelity policy.
    pub fn fork_with_policy(
        &self,
        source: &str,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
        policy: ResumePolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let source_entry =
            resolve_session_with_deadline(&self.data_dir, source, self.index_lock_deadline)?;
        self.fork_entry(&source_entry, options, durability, policy)
    }

    /// Fork the latest canonical source belonging to `working_dir`.
    pub fn fork_latest_in_working_dir(
        &self,
        working_dir: impl AsRef<std::path::Path>,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        self.fork_latest_in_working_dir_with_policy(
            working_dir,
            options,
            durability,
            ResumePolicy::RequireCanonical,
        )
    }

    /// Fork the latest project source under an explicit fidelity policy.
    pub fn fork_latest_in_working_dir_with_policy(
        &self,
        working_dir: impl AsRef<std::path::Path>,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
        policy: ResumePolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let source_entry = resolve_latest_session_in_working_dir_with_deadline(
            &self.data_dir,
            working_dir.as_ref(),
            self.index_lock_deadline,
        )?;
        self.fork_entry(&source_entry, options, durability, policy)
    }

    fn fork_entry(
        &self,
        source_entry: &SessionIndexEntry,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
        policy: ResumePolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        authorize_resume(source_entry, policy)?;
        let artifacts = read_session_events_for_entry_with_deadline(
            &self.data_dir,
            source_entry,
            self.index_lock_deadline,
        )?;
        revalidate_registered_entry(&self.data_dir, source_entry, self.index_lock_deadline)?;
        let mut events = artifacts.events;
        if events.is_empty() {
            return Err(SessionPersistError::EmptySource {
                id: source_entry.id.clone(),
            });
        }
        ensure_migrated_epoch_boundary_in_events(source_entry, &mut events)?;
        let last_event_id = events
            .last()
            .ok_or_else(|| SessionPersistError::EmptySource {
                id: source_entry.id.clone(),
            })?
            .base()
            .id
            .clone();

        let mut new_entry = new_index_entry(Uuid::new_v4().to_string(), options);
        new_entry.fidelity = source_entry.fidelity;
        new_entry.origin = source_entry.origin.clone();
        events.push(SessionEvent::ChildBranch {
            base: EventBase::new(Some(last_event_id.clone())),
            parent_session_id: Some(source_entry.id.clone()),
            child_session_id: Some(new_entry.id.clone()),
            path_address: ROOT_PATH_ADDRESS.to_owned(),
            parent_event_anchor: Some(last_event_id),
            kind: ChildBranchKind::Fork,
        });
        let entry = publish_new_session(
            &self.data_dir,
            &new_entry,
            &events,
            self.index_lock_deadline,
        )?;
        let sink = JsonlSink::open_registered(
            &self.data_dir,
            &entry,
            durability,
            self.index_lock_deadline,
        )?;
        let replay = ReplaySummary {
            replayed_events: events.len(),
        };
        let mut store = EventStore::with_sink_and_events(Box::new(sink), events);
        store.attach_spool(SpoolWriter::for_session(
            &self.data_dir,
            &entry,
            durability,
            self.index_lock_deadline,
        ));
        Ok(OpenSession {
            store,
            entry,
            replay,
        })
    }
}
