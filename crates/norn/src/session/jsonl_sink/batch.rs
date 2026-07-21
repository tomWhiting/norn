//! Cross-handle transaction for one ordered session-event group.

use std::io::Write as _;

use crate::session::events::SessionEvent;
use crate::session::persistence::io::{
    retry_prefix_from_file, serialize_events, session_file_relative, strict_events_from_file,
};
use crate::session::persistence::{IndexCounters, SessionPersistError};
use crate::session::{
    validate_no_incomplete_legacy_response_publications, validate_provider_state_provenance,
};

use super::{DurabilityPolicy, JsonlSink, JsonlTarget, PersistenceSink};

impl JsonlSink {
    pub(super) fn persist_batch_inner(
        &mut self,
        events: &[SessionEvent],
    ) -> Result<(), SessionPersistError> {
        match events {
            [] => return Ok(()),
            [event] => return PersistenceSink::persist(self, event),
            _ => {}
        }

        let first_id = events[0].base().id.as_str();
        if self.pending_write.is_some() {
            return Err(SessionPersistError::EventAppendConflict {
                event_id: first_id.to_owned(),
                reason: "an event group followed an unresolved single-event write",
            });
        }

        let encoded_rows = encode_rows(events)?;
        let lock_deadline = self.index.as_ref().and_then(|index| index.lock_deadline);
        let display_path = self.target.display_path()?;
        let (mut file, _inspection) = self.target.reopen_bound(
            self.identity,
            self.provider_state_identity,
            first_id,
            &encoded_rows[0],
            lock_deadline,
        )?;
        let existing = strict_events_from_file(&mut file, &display_path)?;
        validate_no_incomplete_legacy_response_publications(&existing)?;
        let facts = retry_prefix_from_file(&mut file, &display_path, events)?;
        let pending = &events[facts.retry_prefix..];
        preflight_timeline_counters(facts.counters, pending, self.counter_session_id(first_id))?;
        let next_index_pending = self.preview_index_pending(events)?;
        let (sync_boundaries, next_events_since_sync) = self.preview_cadence(events, first_id)?;

        let mut completed = existing;
        completed.extend_from_slice(pending);
        validate_provider_state_provenance(&completed)?;

        for (index, line) in encoded_rows.iter().enumerate().skip(facts.retry_prefix) {
            file.write_all(line)?;
            #[cfg(test)]
            if std::mem::take(&mut self.fail_after_write_once) {
                return Err(std::io::Error::other("injected ambiguous session write").into());
            }
            if sync_boundaries[index] {
                file.sync_all()?;
            }
        }
        let crossed_boundary = sync_boundaries.iter().any(|boundary| *boundary);
        let retry_crossed_boundary = sync_boundaries[..facts.retry_prefix]
            .iter()
            .any(|boundary| *boundary);
        if retry_crossed_boundary
            || (crossed_boundary && !sync_boundaries.last().copied().unwrap_or(false))
        {
            file.sync_all()?;
        }
        drop(file);

        self.events_since_sync = next_events_since_sync;
        if let (Some(registration), Some(pending_counters)) = (&mut self.index, next_index_pending)
        {
            registration.pending = pending_counters;
            if crossed_boundary && let Err(error) = registration.flush() {
                tracing::error!(
                    session_id = %registration.entry.id,
                    %error,
                    pending_events = registration.pending.event_count,
                    "event group persisted but session index delta remains pending",
                );
            }
        }
        Ok(())
    }

    fn counter_session_id<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.index
            .as_ref()
            .map_or(fallback, |index| index.entry.id.as_str())
    }

    fn preview_index_pending(
        &self,
        events: &[SessionEvent],
    ) -> Result<Option<IndexCounters>, SessionPersistError> {
        let Some(registration) = &self.index else {
            return Ok(None);
        };
        let mut pending = registration.pending;
        for event in events {
            pending = pending.checked_with(event).map_err(|overflow| {
                SessionPersistError::IndexCounterOverflow {
                    id: registration.entry.id.clone(),
                    field: overflow.field(),
                }
            })?;
        }
        Ok(Some(pending))
    }

    fn preview_cadence(
        &self,
        events: &[SessionEvent],
        event_id: &str,
    ) -> Result<(Vec<bool>, u64), SessionPersistError> {
        let mut since_sync = self.events_since_sync;
        let mut boundaries = Vec::with_capacity(events.len());
        for _event in events {
            let boundary = match self.durability {
                DurabilityPolicy::Flush => false,
                DurabilityPolicy::FsyncPerEvent => true,
                DurabilityPolicy::FsyncEveryEvents(limit) => {
                    let next = since_sync.checked_add(1).ok_or_else(|| {
                        SessionPersistError::EventAppendConflict {
                            event_id: event_id.to_owned(),
                            reason: "durability cadence counter overflowed",
                        }
                    })?;
                    if next >= limit.get() {
                        since_sync = 0;
                        true
                    } else {
                        since_sync = next;
                        false
                    }
                }
            };
            boundaries.push(boundary);
        }
        Ok((boundaries, since_sync))
    }
}

impl JsonlTarget {
    pub(super) fn display_path(&self) -> Result<std::path::PathBuf, SessionPersistError> {
        match self {
            #[cfg(test)]
            Self::Path(path) => Ok(path.clone()),
            Self::Registered { data_dir, entry } => {
                Ok(data_dir.join(session_file_relative(entry)?))
            }
        }
    }
}

fn encode_rows(events: &[SessionEvent]) -> Result<Vec<Vec<u8>>, SessionPersistError> {
    events
        .iter()
        .map(|event| serialize_events(std::slice::from_ref(event)))
        .collect()
}

fn preflight_timeline_counters(
    mut counters: IndexCounters,
    events: &[SessionEvent],
    session_id: &str,
) -> Result<(), SessionPersistError> {
    for event in events {
        counters = counters.checked_with(event).map_err(|overflow| {
            SessionPersistError::IndexCounterOverflow {
                id: session_id.to_owned(),
                field: overflow.field(),
            }
        })?;
    }
    Ok(())
}
