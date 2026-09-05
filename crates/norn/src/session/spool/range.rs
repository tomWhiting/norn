//! Registered-owner spool reads bounded by an explicit original JSON byte demand.

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};

use super::{SPOOL_DIR_NAME, SPOOL_FILE_EXTENSION, SpoolWriter, validate_spool_ref};
use crate::session::branch::MailboxId;
use crate::session::events::{EventId, SessionEvent};
use crate::session::persistence::SessionPersistError;
use crate::session::persistence::index::{
    revalidate_registered_entry, with_registered_spool_entries,
};
use crate::session::store::HistoryReadError;
use crate::session_view::{BodyRange, SessionIdentity, ViewError};

/// A demanded spool range failed without returning substituted or partial success.
#[derive(Debug, thiserror::Error)]
pub enum SpoolRangeError {
    /// The recorded capability or current registered generation was refused.
    #[error("spool body for event {event_id} could not be opened: {source}")]
    Persistence {
        /// Actual owning event.
        event_id: EventId,
        /// Generation, path validation or private filesystem failure.
        #[source]
        source: SessionPersistError,
    },
    /// The owning event does not carry a full-output spool reference.
    #[error("event {event_id} does not expose a spool body")]
    Unavailable {
        /// Requested event.
        event_id: EventId,
    },
    /// An explicit range violates its UTF-8 or byte boundary contract.
    #[error("spool range for event {event_id} is invalid: {source}")]
    Range {
        /// Actual owning event.
        event_id: EventId,
        /// Requested range failure.
        #[source]
        source: ViewError,
    },
    /// File contents could not be read exactly as observed by the owner.
    #[error("spool bytes for event {event_id} could not be read: {source}")]
    Io {
        /// Actual owning event.
        event_id: EventId,
        /// Actual filesystem failure, including short reads.
        #[source]
        source: std::io::Error,
    },
    /// The selected bytes contain malformed UTF-8, not an incomplete final codepoint.
    #[error("spool event {event_id} contains malformed UTF-8 at byte {offset}")]
    MalformedUtf8 {
        /// Actual owning event.
        event_id: EventId,
        /// Original file offset of the malformed sequence.
        offset: usize,
    },
    /// The platform cannot represent this file size as an addressable range.
    #[error("spool event {event_id} has unrepresentable byte length {bytes}: {source}")]
    Length {
        /// Actual owning event.
        event_id: EventId,
        /// Observed file size.
        bytes: u64,
        /// The platform's failed integer conversion.
        #[source]
        source: std::num::TryFromIntError,
    },
    /// The requested byte offset cannot be represented by a file seek.
    #[error("spool event {event_id} has unrepresentable byte offset {offset}: {source}")]
    Offset {
        /// Actual owning event.
        event_id: EventId,
        /// Requested byte offset.
        offset: usize,
        /// The platform's failed integer conversion.
        #[source]
        source: std::num::TryFromIntError,
    },
}

pub(crate) struct SpoolChunk {
    pub text: String,
    pub end: usize,
    pub total_bytes: usize,
}

impl SpoolWriter {
    /// Validate the current registered incarnation for an exact off-paint record read.
    /// The index lock is released before this returns: this is a current-owner
    /// check, not a lease pinning the generation across the later in-memory read.
    pub(crate) fn validate_record_owner(&self) -> Result<(), SessionPersistError> {
        revalidate_registered_entry(&self.data_dir, &self.registered, self.index_lock_deadline)
            .map(|_| ())
    }

    pub(crate) fn validate_view_binding(
        &self,
        session: &SessionIdentity,
        mailbox: MailboxId,
    ) -> Result<(), HistoryReadError> {
        let expected = SessionIdentity::Persisted(self.registered.id.clone());
        if session != &expected {
            return Err(HistoryReadError::BindingMismatch {
                expected,
                actual: session.clone(),
            });
        }
        if mailbox.generation() != self.registered.generation {
            return Err(HistoryReadError::BindingGenerationMismatch {
                session: session.clone(),
                expected: self.registered.generation,
                actual: mailbox.generation(),
            });
        }
        Ok(())
    }

    /// Read the exact event-owned JSON spool through this writer's private authority.
    /// No path or root is accepted from a body consumer. JSON syntax is not decoded
    /// by this raw-range interface; incomplete JSON chunks remain explicitly raw.
    pub(crate) fn read_event_range(
        &self,
        event: &SessionEvent,
        demand: BodyRange,
    ) -> Result<SpoolChunk, SpoolRangeError> {
        let event_id = &event.base().id;
        let SessionEvent::ToolResult {
            spool_ref: Some(reference),
            ..
        } = event
        else {
            return Err(SpoolRangeError::Unavailable {
                event_id: event_id.clone(),
            });
        };
        let persistence = |source| SpoolRangeError::Persistence {
            event_id: event_id.clone(),
            source,
        };
        let relative = validate_spool_ref(reference).map_err(persistence)?;
        let expected = format!(
            "{}/{SPOOL_DIR_NAME}/{event_id}.{SPOOL_FILE_EXTENSION}",
            self.root_session_id
        );
        with_registered_spool_entries(
            &self.data_dir,
            &self.registered,
            self.index_lock_deadline,
            |root, entries| {
                if reference != &expected {
                    let inherited = super::inheritance::SpoolInheritance::read(root, entries, &self.registered)?;
                    if !inherited.as_ref().is_some_and(|manifest| manifest.authorizes(event, reference)) {
                        return Err(SessionPersistError::InvalidSpoolRef {
                            spool_ref: reference.clone(),
                            reason: format!("event {event_id} has no publication-owned inherited spool authority"),
                        });
                    }
                }
                let mut file = root.open_read(&relative)?;
                let length = file.metadata()?.len();
                Ok(read_file_range(&mut file, event_id, demand, length))
            },
        )
        .map_err(persistence)?
    }
}

fn read_file_range(
    file: &mut File,
    event_id: &EventId,
    demand: BodyRange,
    length: u64,
) -> Result<SpoolChunk, SpoolRangeError> {
    let total = usize::try_from(length).map_err(|source| SpoolRangeError::Length {
        event_id: event_id.clone(),
        bytes: length,
        source,
    })?;
    let invalid = |source| SpoolRangeError::Range {
        event_id: event_id.clone(),
        source,
    };
    if demand.offset > total {
        return Err(invalid(ViewError::InvalidRange {
            offset: demand.offset,
        }));
    }
    let wanted = demand.max_bytes.get().min(total - demand.offset);
    let io = |source| SpoolRangeError::Io {
        event_id: event_id.clone(),
        source,
    };
    let offset = u64::try_from(demand.offset).map_err(|source| SpoolRangeError::Offset {
        event_id: event_id.clone(),
        offset: demand.offset,
        source,
    })?;
    file.seek(SeekFrom::Start(offset)).map_err(io)?;
    let mut bytes = vec![0; wanted];
    file.read_exact(&mut bytes).map_err(io)?;
    if file.metadata().map_err(io)?.len() != length {
        return Err(io(std::io::Error::other(
            "spool length changed during the requested range read",
        )));
    }
    if bytes.first().is_some_and(|first| first & 0xc0 == 0x80) {
        return Err(invalid(ViewError::InvalidRange {
            offset: demand.offset,
        }));
    }
    let complete = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(error) if error.error_len().is_none() && demand.offset + wanted < total => {
            error.valid_up_to()
        }
        Err(error) => {
            return Err(SpoolRangeError::MalformedUtf8 {
                event_id: event_id.clone(),
                offset: demand.offset + error.valid_up_to(),
            });
        }
    };
    if complete == 0 && demand.offset < total {
        return Err(invalid(ViewError::RangeTooSmall {
            offset: demand.offset,
            demand: demand.max_bytes.get(),
        }));
    }
    bytes.truncate(complete);
    let text = String::from_utf8(bytes).map_err(|error| SpoolRangeError::MalformedUtf8 {
        event_id: event_id.clone(),
        offset: demand.offset + error.utf8_error().valid_up_to(),
    })?;
    Ok(SpoolChunk {
        text,
        end: demand.offset + complete,
        total_bytes: total,
    })
}

#[cfg(test)]
#[path = "range_tests.rs"]
mod tests;
