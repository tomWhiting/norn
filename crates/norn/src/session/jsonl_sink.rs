//! Lazy, inode-bound JSONL session persistence.

use std::io::Write as _;
use std::num::NonZeroU64;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use crate::provider::ProviderStateIdentity;
use crate::session::events::SessionEvent;
use crate::session::persistence::index::{
    open_registered_timeline_bound, read_index_with_deadline, reconcile_registered_timeline,
    registered_timeline_identity,
};
use crate::session::persistence::io::{
    ExistingEventInspection, ExistingEventState, strict_events_from_file,
};
#[cfg(test)]
use crate::session::persistence::io::{open_session_append, open_session_append_bound};
use crate::session::persistence::{IndexCounters, LockedTimelineFile, SessionPersistError};
use crate::session::{
    validate_no_incomplete_legacy_response_publications, validate_provider_state_provenance,
};
use crate::util::PrivateFileIdentity;

use super::store::PersistenceSink;

mod batch;

/// Durability level applied by [`JsonlSink`] after each event write.
///
/// The policy also sets the cadence of session-index maintenance for an
/// index-registered sink. Pending deltas are additionally flushed by
/// [`crate::session::EventStore::checkpoint`] and when the sink is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurabilityPolicy {
    /// Write without issuing `fsync`; index deltas flush at checkpoint/drop.
    Flush,
    /// `fsync` the session file and update the index after every event.
    FsyncPerEvent,
    /// `fsync` and update the index after every caller-specified `n` events.
    FsyncEveryEvents(NonZeroU64),
}

#[derive(Debug)]
struct IndexRegistration {
    data_dir: PathBuf,
    entry: Box<crate::session::persistence::SessionIndexEntry>,
    timeline_identity: PrivateFileIdentity,
    pending: IndexCounters,
    lock_deadline: Option<Duration>,
}

impl IndexRegistration {
    fn accumulate(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        self.pending = self.pending.checked_with(event).map_err(|overflow| {
            SessionPersistError::IndexCounterOverflow {
                id: self.entry.id.clone(),
                field: overflow.field(),
            }
        })?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), SessionPersistError> {
        if self.pending.event_count == 0 {
            return Ok(());
        }
        reconcile_registered_timeline(
            &self.data_dir,
            &self.entry,
            self.timeline_identity,
            self.lock_deadline,
        )?;
        self.pending = IndexCounters::default();
        Ok(())
    }
}

#[derive(Debug)]
enum JsonlTarget {
    #[cfg(test)]
    Path(PathBuf),
    Registered {
        data_dir: PathBuf,
        entry: Box<crate::session::persistence::SessionIndexEntry>,
    },
}

impl JsonlTarget {
    fn reopen_bound(
        &self,
        identity: PrivateFileIdentity,
        provider_state_identity: Option<ProviderStateIdentity>,
        candidate_id: &str,
        candidate_line: &[u8],
        lock_deadline: Option<Duration>,
    ) -> Result<(LockedTimelineFile, ExistingEventInspection), SessionPersistError> {
        match self {
            #[cfg(test)]
            Self::Path(path) => {
                open_session_append_bound(path, identity, candidate_id, candidate_line)
            }
            Self::Registered { data_dir, entry } => open_registered_timeline_bound(
                data_dir,
                entry,
                identity,
                provider_state_identity,
                candidate_id,
                candidate_line,
                lock_deadline,
            ),
        }
    }

    fn revalidate_registration(
        &self,
        lock_deadline: Option<Duration>,
        candidate_id: &str,
    ) -> Result<(), SessionPersistError> {
        match self {
            #[cfg(test)]
            Self::Path(_) => Ok(()),
            Self::Registered { data_dir, entry } => {
                let entries = read_index_with_deadline(data_dir, lock_deadline)?;
                let current = entries
                    .iter()
                    .find(|current| current.id == entry.id)
                    .ok_or_else(|| SessionPersistError::GenerationChanged {
                        id: entry.id.clone(),
                    })?;
                if current.generation != entry.generation {
                    return Err(SessionPersistError::GenerationChanged {
                        id: entry.id.clone(),
                    });
                }
                if current.rel_path != entry.rel_path {
                    return Err(SessionPersistError::EventAppendConflict {
                        event_id: candidate_id.to_owned(),
                        reason: "the registered session path changed after the sink was opened",
                    });
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
struct PendingWrite {
    event_id: String,
    line: Vec<u8>,
    at_sync_boundary: Option<bool>,
}

/// JSONL sink that retains an inode identity rather than an open descriptor.
///
/// Every append securely reopens and verifies the original inode, heals a torn
/// tail, writes the complete event, and closes the descriptor. Index-registered
/// sinks batch their index delta according to [`DurabilityPolicy`].
pub struct JsonlSink {
    target: JsonlTarget,
    identity: PrivateFileIdentity,
    provider_state_identity: Option<ProviderStateIdentity>,
    durability: DurabilityPolicy,
    events_since_sync: u64,
    index: Option<IndexRegistration>,
    pending_write: Option<PendingWrite>,
    #[cfg(test)]
    fail_after_write_once: bool,
}

impl JsonlSink {
    /// Open or create `path` with [`DurabilityPolicy::Flush`].
    ///
    /// # Errors
    ///
    /// Returns an error when the private session file cannot be opened,
    /// created, healed, or identity-bound.
    #[cfg(test)]
    pub(crate) fn open(path: &Path) -> Result<Self, SessionPersistError> {
        Self::open_with(path, DurabilityPolicy::Flush)
    }

    /// Open or create `path` with an explicit durability policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the private session file cannot be opened,
    /// created, healed, or identity-bound.
    #[cfg(test)]
    pub(crate) fn open_with(
        path: &Path,
        durability: DurabilityPolicy,
    ) -> Result<Self, SessionPersistError> {
        let target = JsonlTarget::Path(path.to_path_buf());
        let file = open_session_append(path)?;
        Ok(Self {
            target,
            identity: PrivateFileIdentity::capture(&file)?,
            provider_state_identity: None,
            durability,
            events_since_sync: 0,
            index: None,
            pending_write: None,
            #[cfg(test)]
            fail_after_write_once: false,
        })
    }

    /// Open a registered session file and maintain its index delta.
    ///
    /// # Errors
    ///
    /// Returns an error for a reserved session id or when the private session
    /// file cannot be opened, created, healed, or identity-bound.
    pub fn open_registered(
        data_dir: &Path,
        entry: &crate::session::persistence::SessionIndexEntry,
        durability: DurabilityPolicy,
        index_lock_deadline: Option<Duration>,
    ) -> Result<Self, SessionPersistError> {
        crate::session::persistence::io::ensure_session_id_not_reserved(&entry.id)?;
        let target = JsonlTarget::Registered {
            data_dir: data_dir.to_path_buf(),
            entry: Box::new(entry.clone()),
        };
        let timeline_identity = registered_timeline_identity(data_dir, entry, index_lock_deadline)?;
        target.revalidate_registration(
            index_lock_deadline,
            "binding registered session timeline identity",
        )?;
        Ok(Self {
            target,
            identity: timeline_identity,
            provider_state_identity: entry.provider_state_identity,
            durability,
            events_since_sync: 0,
            index: Some(IndexRegistration {
                data_dir: data_dir.to_path_buf(),
                entry: Box::new(entry.clone()),
                timeline_identity,
                pending: IndexCounters::default(),
                lock_deadline: index_lock_deadline,
            }),
            pending_write: None,
            #[cfg(test)]
            fail_after_write_once: false,
        })
    }

    fn remember_ambiguous(&mut self, event_id: &str, line: &[u8], at_sync_boundary: Option<bool>) {
        self.pending_write = Some(PendingWrite {
            event_id: event_id.to_owned(),
            line: line.to_vec(),
            at_sync_boundary,
        });
    }

    fn advance_durability_cadence(&mut self, event_id: &str) -> Result<bool, SessionPersistError> {
        match self.durability {
            DurabilityPolicy::Flush => Ok(false),
            DurabilityPolicy::FsyncPerEvent => Ok(true),
            DurabilityPolicy::FsyncEveryEvents(n) => {
                let next = self.events_since_sync.checked_add(1).ok_or_else(|| {
                    SessionPersistError::EventAppendConflict {
                        event_id: event_id.to_owned(),
                        reason: "durability cadence counter overflowed",
                    }
                })?;
                if next >= n.get() {
                    self.events_since_sync = 0;
                    Ok(true)
                } else {
                    self.events_since_sync = next;
                    Ok(false)
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_after_write_once(&mut self) {
        self.fail_after_write_once = true;
    }
}

impl Drop for JsonlSink {
    fn drop(&mut self) {
        if let Some(registration) = &mut self.index
            && let Err(error) = registration.flush()
        {
            tracing::error!(
                session_id = %registration.entry.id,
                %error,
                pending_events = registration.pending.event_count,
                "failed to flush pending session index delta on sink close; index remains stale",
            );
        }
    }
}

#[cfg(test)]
pub(super) fn write_event_line<W: std::io::Write>(
    writer: &mut W,
    needs_newline: &mut bool,
    line: &[u8],
) -> std::io::Result<()> {
    if *needs_newline {
        writer.write_all(b"\n")?;
        *needs_newline = false;
    }
    if let Err(error) = writer.write_all(line) {
        *needs_newline = true;
        return Err(error);
    }
    Ok(())
}

impl PersistenceSink for JsonlSink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        let event_id = event.base().id.as_str();
        if let Some(pending) = &self.pending_write
            && (pending.event_id != event_id || pending.line != line)
        {
            return Err(SessionPersistError::EventAppendConflict {
                event_id: event_id.to_owned(),
                reason: "a different event was supplied after an ambiguous write",
            });
        }
        let lock_deadline = self.index.as_ref().and_then(|index| index.lock_deadline);
        let counter_session_id = self
            .index
            .as_ref()
            .map_or(event_id, |index| index.entry.id.as_str())
            .to_owned();
        let (mut file, inspection) = self.target.reopen_bound(
            self.identity,
            self.provider_state_identity,
            event_id,
            &line,
            lock_deadline,
        )?;
        match inspection.state {
            ExistingEventState::Absent => {
                let display_path = self.target.display_path()?;
                let mut candidate = strict_events_from_file(&mut file, &display_path)?;
                validate_no_incomplete_legacy_response_publications(&candidate)?;
                candidate.push(event.clone());
                validate_provider_state_provenance(&candidate)?;
                inspection
                    .counters
                    .checked_with(event)
                    .map_err(|overflow| SessionPersistError::IndexCounterOverflow {
                        id: counter_session_id,
                        field: overflow.field(),
                    })?;
                if let Err(error) = file.write_all(&line) {
                    let cadence = self
                        .pending_write
                        .as_ref()
                        .and_then(|pending| pending.at_sync_boundary);
                    self.remember_ambiguous(event_id, &line, cadence);
                    return Err(error.into());
                }
            }
            ExistingEventState::ExactTail if self.pending_write.is_some() => {
                let display_path = self.target.display_path()?;
                let candidate = strict_events_from_file(&mut file, &display_path)?;
                validate_provider_state_provenance(&candidate)?;
            }
            ExistingEventState::ExactTail => {
                return Err(SessionPersistError::EventAppendConflict {
                    event_id: event_id.to_owned(),
                    reason: "the event is already durable without an in-process retry record",
                });
            }
            ExistingEventState::ExactNotTail => {
                return Err(SessionPersistError::EventAppendConflict {
                    event_id: event_id.to_owned(),
                    reason: "the event already exists before the durable timeline tail",
                });
            }
            ExistingEventState::ConflictingId => {
                return Err(SessionPersistError::EventAppendConflict {
                    event_id: event_id.to_owned(),
                    reason: "the event id is already durable with different content",
                });
            }
        }
        #[cfg(test)]
        if std::mem::take(&mut self.fail_after_write_once) {
            let cadence = self
                .pending_write
                .as_ref()
                .and_then(|pending| pending.at_sync_boundary);
            self.remember_ambiguous(event_id, &line, cadence);
            return Err(std::io::Error::other("injected ambiguous session write").into());
        }
        let at_boundary = match self
            .pending_write
            .as_ref()
            .and_then(|pending| pending.at_sync_boundary)
        {
            Some(at_boundary) => at_boundary,
            None => self.advance_durability_cadence(event_id)?,
        };
        if at_boundary && let Err(error) = file.sync_all() {
            self.remember_ambiguous(event_id, &line, Some(at_boundary));
            return Err(error.into());
        }
        // Index operations never nest beneath a timeline transaction. Delete
        // and publication take the opposite (index-before-timeline) order.
        drop(file);
        if let Some(registration) = &mut self.index {
            if let Err(error) = registration.accumulate(event) {
                self.remember_ambiguous(event_id, &line, Some(at_boundary));
                return Err(error);
            }
            if at_boundary && let Err(error) = registration.flush() {
                tracing::error!(
                    session_id = %registration.entry.id,
                    %error,
                    pending_events = registration.pending.event_count,
                    "event persisted but session index delta remains pending",
                );
            }
        }
        self.pending_write = None;
        Ok(())
    }

    fn persist_batch(&mut self, events: &[SessionEvent]) -> Result<(), SessionPersistError> {
        self.persist_batch_inner(events)
    }

    fn checkpoint(&mut self) -> Result<(), SessionPersistError> {
        if let Some(registration) = &mut self.index {
            registration.flush()?;
        }
        Ok(())
    }

    fn set_provider_state_identity(&mut self, identity: Option<ProviderStateIdentity>) {
        self.provider_state_identity = identity;
    }
}
