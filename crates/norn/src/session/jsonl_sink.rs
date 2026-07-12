//! Lazy, inode-bound JSONL session persistence.

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::session::persistence::SessionPersistError;
use crate::session::persistence::index::{sum_usage_from_events, update_session_index};
use crate::session::persistence::io::{
    open_session_append, open_session_append_bound, open_session_append_for_entry,
    open_session_append_for_entry_bound,
};
use crate::util::PrivateFileIdentity;

use super::store::PersistenceSink;

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
    session_id: String,
    pending_events: u64,
    pending_usage: Usage,
    lock_deadline: Option<Duration>,
}

impl IndexRegistration {
    fn accumulate(&mut self, event: &SessionEvent) {
        self.pending_events = self.pending_events.saturating_add(1);
        let usage = sum_usage_from_events(std::slice::from_ref(event));
        self.pending_usage.input_tokens = self
            .pending_usage
            .input_tokens
            .saturating_add(usage.input_tokens);
        self.pending_usage.output_tokens = self
            .pending_usage
            .output_tokens
            .saturating_add(usage.output_tokens);
        self.pending_usage.cache_read_tokens = self
            .pending_usage
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens);
    }

    fn flush(&mut self) -> Result<(), SessionPersistError> {
        if self.pending_events == 0 {
            return Ok(());
        }
        update_session_index(
            &self.data_dir,
            &self.session_id,
            self.pending_events,
            &self.pending_usage,
            self.lock_deadline,
        )?;
        self.pending_events = 0;
        self.pending_usage = Usage::default();
        Ok(())
    }
}

#[derive(Debug)]
enum JsonlTarget {
    Path(PathBuf),
    Registered {
        data_dir: PathBuf,
        entry: Box<crate::session::persistence::SessionIndexEntry>,
    },
}

impl JsonlTarget {
    fn open_initial(&self) -> Result<std::fs::File, SessionPersistError> {
        match self {
            Self::Path(path) => open_session_append(path),
            Self::Registered { data_dir, entry } => open_session_append_for_entry(data_dir, entry),
        }
    }

    fn reopen_bound(
        &self,
        identity: PrivateFileIdentity,
    ) -> Result<std::fs::File, SessionPersistError> {
        match self {
            Self::Path(path) => open_session_append_bound(path, identity),
            Self::Registered { data_dir, entry } => {
                open_session_append_for_entry_bound(data_dir, entry, identity)
            }
        }
    }
}

/// JSONL sink that retains an inode identity rather than an open descriptor.
///
/// Every append securely reopens and verifies the original inode, heals a torn
/// tail, writes the complete event, and closes the descriptor. Index-registered
/// sinks batch their index delta according to [`DurabilityPolicy`].
pub struct JsonlSink {
    target: JsonlTarget,
    identity: PrivateFileIdentity,
    durability: DurabilityPolicy,
    needs_newline: bool,
    events_since_sync: u64,
    index: Option<IndexRegistration>,
}

impl JsonlSink {
    /// Open or create `path` with [`DurabilityPolicy::Flush`].
    ///
    /// # Errors
    ///
    /// Returns an error when the private session file cannot be opened,
    /// created, healed, or identity-bound.
    pub fn open(path: &Path) -> Result<Self, SessionPersistError> {
        Self::open_with(path, DurabilityPolicy::Flush)
    }

    /// Open or create `path` with an explicit durability policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the private session file cannot be opened,
    /// created, healed, or identity-bound.
    pub fn open_with(
        path: &Path,
        durability: DurabilityPolicy,
    ) -> Result<Self, SessionPersistError> {
        let target = JsonlTarget::Path(path.to_path_buf());
        let file = target.open_initial()?;
        Ok(Self {
            target,
            identity: PrivateFileIdentity::capture(&file)?,
            durability,
            needs_newline: false,
            events_since_sync: 0,
            index: None,
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
        let file = target.open_initial()?;
        Ok(Self {
            target,
            identity: PrivateFileIdentity::capture(&file)?,
            durability,
            needs_newline: false,
            events_since_sync: 0,
            index: Some(IndexRegistration {
                data_dir: data_dir.to_path_buf(),
                session_id: entry.id.clone(),
                pending_events: 0,
                pending_usage: Usage::default(),
                lock_deadline: index_lock_deadline,
            }),
        })
    }
}

impl Drop for JsonlSink {
    fn drop(&mut self) {
        if let Some(registration) = &mut self.index
            && let Err(error) = registration.flush()
        {
            tracing::error!(
                session_id = %registration.session_id,
                %error,
                pending_events = registration.pending_events,
                "failed to flush pending session index delta on sink close; index remains stale",
            );
        }
    }
}

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
        let mut file = self.target.reopen_bound(self.identity)?;
        self.needs_newline = false;
        write_event_line(&mut file, &mut self.needs_newline, &line)?;
        let at_boundary = match self.durability {
            DurabilityPolicy::Flush => false,
            DurabilityPolicy::FsyncPerEvent => {
                file.sync_all()?;
                true
            }
            DurabilityPolicy::FsyncEveryEvents(n) => {
                self.events_since_sync = self.events_since_sync.saturating_add(1);
                if self.events_since_sync >= n.get() {
                    file.sync_all()?;
                    self.events_since_sync = 0;
                    true
                } else {
                    false
                }
            }
        };
        if let Some(registration) = &mut self.index {
            registration.accumulate(event);
            if at_boundary && let Err(error) = registration.flush() {
                tracing::error!(
                    session_id = %registration.session_id,
                    %error,
                    pending_events = registration.pending_events,
                    "event persisted but session index delta remains pending",
                );
            }
        }
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<(), SessionPersistError> {
        if let Some(registration) = &mut self.index {
            registration.flush()?;
        }
        Ok(())
    }
}
