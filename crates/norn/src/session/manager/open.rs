use std::path::Path;

use chrono::Utc;
use uuid::Uuid;

use crate::provider::usage::Usage;
use crate::session::persistence::index::{
    publish_new_session, resolve_latest_session_in_working_dir_with_deadline,
    resolve_session_with_deadline, revalidate_registered_entry, update_registered_entry,
};
use crate::session::persistence::read_session_events_for_entry_with_deadline;
use crate::session::persistence::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionIndexEntry, SessionPersistError,
    SessionRecordOrigin, SessionStatus,
};
use crate::session::spool::SpoolWriter;
use crate::session::store::{DurabilityPolicy, EventStore, JsonlSink};
use crate::session::{ResponseAudioStore, referenced_response_audio_artifacts};

use super::resume_policy::{authorize_resume, ensure_migrated_epoch_boundary, lock_migrated_epoch};
use super::{CreateSessionOptions, OpenSession, ReplaySummary, ResumePolicy, SessionManager};

impl SessionManager {
    /// Create a fresh canonical native session with a random identifier.
    pub fn create(
        &self,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let candidate = new_index_entry(Uuid::new_v4().to_string(), options);
        let entry = publish_new_session(&self.data_dir, &candidate, &[], self.index_lock_deadline)?;
        self.open_fresh(entry, durability)
    }

    /// Create a fresh canonical native session under an exact unused id.
    pub fn create_with_id(
        &self,
        id: &str,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        validate_explicit_session_id(id)?;
        let candidate = new_index_entry(id.to_owned(), options);
        let entry = publish_new_session(&self.data_dir, &candidate, &[], self.index_lock_deadline)?;
        self.open_fresh(entry, durability)
    }

    /// Resume a canonical session selected by id, name, or unique prefix.
    pub fn resume(
        &self,
        id_or_name: &str,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        self.resume_with_policy(id_or_name, durability, ResumePolicy::RequireCanonical)
    }

    /// Resume with an explicit fidelity policy.
    pub fn resume_with_policy(
        &self,
        id_or_name: &str,
        durability: DurabilityPolicy,
        policy: ResumePolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let entry =
            resolve_session_with_deadline(&self.data_dir, id_or_name, self.index_lock_deadline)?;
        self.resume_entry(&entry, durability, policy)
    }

    /// Resume the latest canonical session belonging to `working_dir`.
    pub fn resume_latest_in_working_dir(
        &self,
        working_dir: impl AsRef<Path>,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        self.resume_latest_in_working_dir_with_policy(
            working_dir,
            durability,
            ResumePolicy::RequireCanonical,
        )
    }

    /// Resume the latest project session with an explicit fidelity policy.
    pub fn resume_latest_in_working_dir_with_policy(
        &self,
        working_dir: impl AsRef<Path>,
        durability: DurabilityPolicy,
        policy: ResumePolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let entry = resolve_latest_session_in_working_dir_with_deadline(
            &self.data_dir,
            working_dir.as_ref(),
            self.index_lock_deadline,
        )?;
        self.resume_entry(&entry, durability, policy)
    }

    /// Idempotently create or canonical-only resume an exact session id.
    pub fn open_or_resume(
        &self,
        id: &str,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        self.open_or_resume_with_policy(id, options, durability, ResumePolicy::RequireCanonical)
    }

    /// Idempotently create or resume with an explicit existing-entry policy.
    pub fn open_or_resume_with_policy(
        &self,
        id: &str,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
        policy: ResumePolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        validate_explicit_session_id(id)?;
        let candidate = new_index_entry(id.to_owned(), options);
        match publish_new_session(&self.data_dir, &candidate, &[], self.index_lock_deadline) {
            Ok(entry) => self.open_fresh(entry, durability),
            Err(SessionPersistError::IdExists { .. }) => {
                let existing = resolve_session_with_deadline(
                    &self.data_dir,
                    &candidate.id,
                    self.index_lock_deadline,
                )
                .map_err(|error| match error {
                    SessionPersistError::NotFound { .. } => SessionPersistError::IdExists {
                        id: candidate.id.clone(),
                    },
                    other => other,
                })?;
                self.resume_entry(&existing, durability, policy)
            }
            Err(error) => Err(error),
        }
    }

    fn open_fresh(
        &self,
        entry: SessionIndexEntry,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let sink = JsonlSink::open_registered(
            &self.data_dir,
            &entry,
            durability,
            self.index_lock_deadline,
        )?;
        let mut store = EventStore::with_sink(Box::new(sink));
        store.attach_spool(SpoolWriter::for_session(
            &self.data_dir,
            &entry,
            durability,
            self.index_lock_deadline,
        ));
        store.attach_response_audio(ResponseAudioStore::for_session(
            &self.data_dir,
            &entry,
            durability,
            self.index_lock_deadline,
        ));
        Ok(OpenSession {
            store,
            entry,
            replay: ReplaySummary::default(),
        })
    }

    fn resume_entry(
        &self,
        entry: &SessionIndexEntry,
        durability: DurabilityPolicy,
        policy: ResumePolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        authorize_resume(entry, policy)?;
        let _epoch_guard = lock_migrated_epoch(&self.data_dir, entry)?;
        let artifacts = read_session_events_for_entry_with_deadline(
            &self.data_dir,
            entry,
            self.index_lock_deadline,
        )?;
        referenced_response_audio_artifacts(&artifacts.events).map_err(|_error| {
            SessionPersistError::InvalidResponseAudioArtifact {
                artifact_id: "<transcript>".to_owned(),
                reason: "the transcript response-audio association was invalid",
            }
        })?;
        let entry = revalidate_registered_entry(&self.data_dir, entry, self.index_lock_deadline)?;
        let actual_count = u64::try_from(artifacts.events.len()).map_err(|error| {
            SessionPersistError::EventStore(format!(
                "session '{}' event count is not representable: {error}",
                entry.id
            ))
        })?;
        let mut entry = reconcile_index_entry(
            &self.data_dir,
            entry,
            actual_count,
            &artifacts.usage,
            self.index_lock_deadline,
        )?;
        let sink = JsonlSink::open_registered(
            &self.data_dir,
            &entry,
            durability,
            self.index_lock_deadline,
        )?;
        let replay = ReplaySummary {
            replayed_events: artifacts.events.len(),
        };
        let mut store = EventStore::with_sink_and_events(Box::new(sink), artifacts.events);
        store.attach_spool(SpoolWriter::for_session(
            &self.data_dir,
            &entry,
            durability,
            self.index_lock_deadline,
        ));
        store.attach_response_audio(ResponseAudioStore::for_session(
            &self.data_dir,
            &entry,
            durability,
            self.index_lock_deadline,
        ));
        if ensure_migrated_epoch_boundary(&entry, &store)? {
            entry = revalidate_registered_entry(&self.data_dir, &entry, self.index_lock_deadline)?;
        }
        Ok(OpenSession {
            store,
            entry,
            replay,
        })
    }
}

pub(super) fn new_index_entry(id: String, options: CreateSessionOptions) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id,
        generation: Uuid::new_v4(),
        name: options.name,
        model: options.model,
        working_dir: options.working_dir,
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path: None,
        parent_id: None,
        fidelity: ResumeFidelity::Canonical,
        origin: SessionRecordOrigin::Native,
    }
}

fn validate_explicit_session_id(id: &str) -> Result<(), SessionPersistError> {
    crate::session::persistence::io::ensure_session_id_path_safe(id)
}

fn reconcile_index_entry(
    data_dir: &Path,
    entry: SessionIndexEntry,
    actual_count: u64,
    actual_usage: &Usage,
    lock_deadline: Option<std::time::Duration>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    if entry.event_count == actual_count
        && entry.total_input_tokens == actual_usage.input_tokens
        && entry.total_output_tokens == actual_usage.output_tokens
        && entry.total_cache_read_tokens == actual_usage.cache_read_tokens
    {
        return Ok(entry);
    }
    let mut repaired = entry;
    repaired.event_count = actual_count;
    repaired.total_input_tokens = actual_usage.input_tokens;
    repaired.total_output_tokens = actual_usage.output_tokens;
    repaired.total_cache_read_tokens = actual_usage.cache_read_tokens;
    match update_registered_entry(data_dir, &repaired, lock_deadline, |entry| {
        entry.event_count = actual_count;
        entry.total_input_tokens = actual_usage.input_tokens;
        entry.total_output_tokens = actual_usage.output_tokens;
        entry.total_cache_read_tokens = actual_usage.cache_read_tokens;
    }) {
        Ok(updated) => Ok(updated),
        Err(error @ SessionPersistError::GenerationChanged { .. }) => Err(error),
        Err(error) => {
            tracing::error!(session_id = %repaired.id, %error, "failed to persist repaired session index totals");
            Ok(repaired)
        }
    }
}
