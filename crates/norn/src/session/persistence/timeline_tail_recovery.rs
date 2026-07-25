//! Quarantine and recovery of a crash-torn response-publication tail.
//!
//! [`super::timeline_file`] already repairs the *syntactic* crash tail: a
//! final row whose bytes stop mid-JSON is provably incomplete, so it is
//! truncated away. This module handles the *semantic* counterpart. A
//! publication group is appended one complete row at a time, so a writer
//! killed between rows — the observed case is the host disk filling with
//! `ENOSPC` — leaves a durable, perfectly well-formed strict prefix of that
//! group: an epoch boundary and a `provider.state.provenance` marker naming
//! an assistant event that never landed. Every row parses; only the *meaning*
//! is torn.
//!
//! Before this module, that tail cost the whole session:
//! [`crate::session::validate_provider_state_provenance`] found the dangling
//! reference and refused the entire timeline, so a history of hundreds of
//! healthy events became unresumable because of its last two rows.
//!
//! Recovery is deliberately narrow and never destructive:
//!
//! * classification is
//!   [`torn_publication_tail`](crate::session::publication_tail_recovery::torn_publication_tail),
//!   which accepts only an incomplete group at the very end of a timeline
//!   whose prefix validates on its own — a violation in the interior keeps
//!   its typed hard failure;
//! * the torn bytes are copied verbatim into a quarantine sidecar beside the
//!   timeline and made durable (file *and* directory) **before** the timeline
//!   is touched, so the evidence outlives the repair;
//! * the timeline is then truncated in place — never re-created, so a live
//!   sink's [`PrivateFileIdentity`](crate::util::PrivateFileIdentity) binding
//!   still holds — and a durable
//!   [`PublicationTailRecovery`] event records what was quarantined, where,
//!   and why;
//! * the repair is announced with a `WARN` line naming the sidecar.
//!
//! Crash window: the sidecar is durable before the truncation, and the
//! truncation is durable before the recovery event. A process killed between
//! the truncation and the recovery event therefore leaves a resumable
//! timeline plus a preserved sidecar, but no in-timeline annotation of the
//! repair. No quarantined byte is ever lost in that window, and the `WARN`
//! line is emitted before the truncation so the operator record survives it.
//!
//! Sidecar names are deterministic (`<timeline>.torn-tail-<boundary event
//! id>.quarantine`), so re-running the repair after a crash reproduces the
//! same name with the same bytes; an existing sidecar with *different* bytes
//! is a typed refusal rather than an overwrite of evidence. The `.quarantine`
//! extension keeps the sidecar outside every `.jsonl` name family the store
//! resolves sessions through.

use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::publication_tail_recovery::{
    PublicationTailRecovery, TornPublicationTail, torn_publication_tail,
};
use crate::session::validate_provider_state_provenance;
use crate::util::{PrivateRoot, validate_private_component};

use super::types::SessionPersistError;

/// Validate `events`, recovering a crash-torn publication tail in place.
///
/// Returns the events unchanged when the timeline is healthy. When the only
/// violation is an incomplete publication group at the tail, the torn rows
/// are quarantined, the timeline is truncated to its healthy prefix, a
/// [`PublicationTailRecovery`] event is appended, and the healed history is
/// returned. Every other violation is propagated as the typed hard failure it
/// already was.
///
/// The caller must hold the timeline lock for `relative`.
pub(super) fn events_with_recovered_publication_tail(
    root: &PrivateRoot,
    relative: &Path,
    events: Vec<SessionEvent>,
) -> Result<Vec<SessionEvent>, SessionPersistError> {
    let Err(violation) = validate_provider_state_provenance(&events) else {
        return Ok(events);
    };
    let Some(torn) = torn_publication_tail(&events) else {
        return Err(violation.into());
    };
    quarantine_torn_tail(root, relative, events, &torn)
}

fn quarantine_torn_tail(
    root: &PrivateRoot,
    relative: &Path,
    mut events: Vec<SessionEvent>,
    torn: &TornPublicationTail,
) -> Result<Vec<SessionEvent>, SessionPersistError> {
    let display_path = root.display_path(relative);
    let names =
        QuarantineNames::for_timeline(relative, &torn.quarantined_event_ids, &display_path)?;

    let mut file = root.open_read_append(relative)?;
    let length = file.metadata()?.len();
    // The format header occupies the first line, so retaining
    // `boundary_index` events means retaining `boundary_index + 1` lines.
    let retained_lines = torn.boundary_index.checked_add(1).ok_or_else(|| {
        quarantine_error(
            &display_path,
            "the retained line count is not representable",
        )
    })?;
    let prefix_len = complete_line_prefix_len(&mut file, retained_lines, &display_path)?;
    let removed_bytes = length.checked_sub(prefix_len).ok_or_else(|| {
        quarantine_error(
            &display_path,
            "the torn tail offset exceeds the timeline length",
        )
    })?;

    file.seek(SeekFrom::Start(prefix_len))?;
    let mut torn_bytes = Vec::new();
    file.read_to_end(&mut torn_bytes)?;
    // Every retained row is newline-terminated, so the removed region splits
    // into exactly one segment per quarantined row.
    let torn_rows = torn_bytes.split_inclusive(|byte| *byte == b'\n').count();
    if removed_bytes == 0
        || torn_rows != torn.quarantined_event_ids.len()
        || torn_bytes.last() != Some(&b'\n')
    {
        return Err(quarantine_error(
            &display_path,
            "the torn tail bytes do not match the classified rows",
        ));
    }

    // Build and fully validate the healed timeline before mutating anything:
    // a recovery event that would not survive a strict re-read must never be
    // written over a session that is merely unresumable.
    let recovery_event = build_recovery_event(
        &events,
        &names.quarantine_name,
        torn,
        removed_bytes,
        &display_path,
    )?;
    let recovery_row = encode_row(&recovery_event, &display_path)?;
    events.truncate(torn.boundary_index);
    events.push(recovery_event);
    validate_provider_state_provenance(&events).map_err(|error| {
        quarantine_error_owned(
            &display_path,
            format!("the recovered prefix does not validate: {error}"),
        )
    })?;

    publish_quarantine(root, &names, &torn_bytes, &display_path)?;
    tracing::warn!(
        path = %display_path.display(),
        quarantine = %root.display_path(&names.quarantine_relative).display(),
        quarantined_rows = torn_rows,
        quarantined_bytes = removed_bytes,
        orphaned_assistant_event_id =
            torn.orphaned_assistant_event_id.as_ref().map(EventId::as_str),
        "quarantining a crash-torn response publication group and resuming from the healthy prefix",
    );

    file.set_len(prefix_len)?;
    file.sync_all()?;
    file.write_all(&recovery_row)?;
    file.sync_all()?;
    Ok(events)
}

fn build_recovery_event(
    events: &[SessionEvent],
    quarantine_name: &str,
    torn: &TornPublicationTail,
    removed_bytes: u64,
    display_path: &Path,
) -> Result<SessionEvent, SessionPersistError> {
    let record = PublicationTailRecovery::new(
        quarantine_name.to_owned(),
        torn.quarantined_event_ids.clone(),
        removed_bytes,
        torn.orphaned_assistant_event_id.clone(),
    )
    .map_err(|error| quarantine_error_owned(display_path, error.to_string()))?;
    let parent_id = torn
        .boundary_index
        .checked_sub(1)
        .and_then(|index| events.get(index))
        .map(|event| event.base().id.clone());
    record
        .into_custom_event(EventBase::new(parent_id))
        .map_err(SessionPersistError::Serde)
}

/// Encode one row and prove it survives a lossless strict decode.
///
/// The strict reader rejects any row whose typed decoding would change field
/// presence or value. Proving that here keeps a recovery event from turning
/// an unresumable session into an unreadable one.
fn encode_row(event: &SessionEvent, display_path: &Path) -> Result<Vec<u8>, SessionPersistError> {
    let mut row = serde_json::to_vec(event)?;
    let decoded: SessionEvent = serde_json::from_slice(&row)?;
    if serde_json::to_value(&decoded)? != serde_json::to_value(event)? {
        return Err(quarantine_error(
            display_path,
            "the recovery event would not survive a strict re-read",
        ));
    }
    row.push(b'\n');
    Ok(row)
}

/// Byte length of the first `lines` complete newline-terminated rows.
fn complete_line_prefix_len(
    file: &mut File,
    lines: usize,
    display_path: &Path,
) -> Result<u64, SessionPersistError> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut offset = 0_u64;
    for _ in 0..lines {
        let mut raw = Vec::new();
        let read = reader.read_until(b'\n', &mut raw)?;
        if read == 0 || raw.last() != Some(&b'\n') {
            return Err(quarantine_error(
                display_path,
                "the timeline ended before the classified torn tail",
            ));
        }
        offset = u64::try_from(read)
            .ok()
            .and_then(|read| offset.checked_add(read))
            .ok_or_else(|| {
                quarantine_error(
                    display_path,
                    "the retained prefix length is not representable",
                )
            })?;
    }
    Ok(offset)
}

struct QuarantineNames {
    quarantine_name: String,
    quarantine_relative: PathBuf,
    temporary_relative: PathBuf,
    parent: PathBuf,
}

impl QuarantineNames {
    fn for_timeline(
        relative: &Path,
        quarantined_event_ids: &[EventId],
        display_path: &Path,
    ) -> Result<Self, SessionPersistError> {
        let file_name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                quarantine_error(
                    display_path,
                    "the timeline path has no usable final component",
                )
            })?;
        let boundary_id = quarantined_event_ids
            .first()
            .ok_or_else(|| quarantine_error(display_path, "the torn tail names no rows"))?;
        let quarantine_name = format!("{file_name}.torn-tail-{}.quarantine", boundary_id.as_str());
        validate_private_component(&quarantine_name, "quarantine file")
            .map_err(|error| quarantine_error_owned(display_path, error.to_string()))?;
        let temporary_name = format!("{quarantine_name}.tmp.{}", uuid::Uuid::new_v4());
        validate_private_component(&temporary_name, "quarantine temporary file")
            .map_err(|error| quarantine_error_owned(display_path, error.to_string()))?;
        Ok(Self {
            quarantine_relative: relative.with_file_name(&quarantine_name),
            temporary_relative: relative.with_file_name(&temporary_name),
            parent: relative
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf(),
            quarantine_name,
        })
    }
}

/// Durably publish the torn bytes beside the timeline.
///
/// An existing sidecar with identical bytes is the crash-retry case and is
/// accepted as already done. An existing sidecar with different bytes is
/// refused: quarantined evidence is never overwritten.
fn publish_quarantine(
    root: &PrivateRoot,
    names: &QuarantineNames,
    torn_bytes: &[u8],
    display_path: &Path,
) -> Result<(), SessionPersistError> {
    if quarantine_already_published(root, names, torn_bytes, display_path)? {
        return Ok(());
    }
    let write_result = (|| -> std::io::Result<()> {
        let mut file = root.create_new(&names.temporary_relative)?;
        file.write_all(torn_bytes)?;
        file.sync_all()
    })();
    let publish_result = write_result
        .and_then(|()| root.publish_new(&names.temporary_relative, &names.quarantine_relative));
    if let Err(error) = root.remove_file(&names.temporary_relative)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %root.display_path(&names.temporary_relative).display(),
            %error,
            "failed to remove an inert torn-tail quarantine temporary file",
        );
    }
    match publish_result {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another attempt published the same tear first; accept only when
            // the published bytes are identical.
            if quarantine_already_published(root, names, torn_bytes, display_path)? {
                return Ok(());
            }
            return Err(quarantine_error(
                display_path,
                "the quarantine file vanished between publication attempts",
            ));
        }
        Err(error) => return Err(error.into()),
    }
    root.sync_dir(&names.parent)?;
    Ok(())
}

fn quarantine_already_published(
    root: &PrivateRoot,
    names: &QuarantineNames,
    torn_bytes: &[u8],
    display_path: &Path,
) -> Result<bool, SessionPersistError> {
    let mut existing = match root.open_read(&names.quarantine_relative) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut published = Vec::new();
    existing.read_to_end(&mut published)?;
    if published != torn_bytes {
        return Err(quarantine_error(
            display_path,
            "a quarantine file with different content already occupies the name",
        ));
    }
    Ok(true)
}

fn quarantine_error(display_path: &Path, reason: &'static str) -> SessionPersistError {
    SessionPersistError::TornTailQuarantine {
        path: display_path.to_path_buf(),
        reason: reason.to_owned(),
    }
}

fn quarantine_error_owned(display_path: &Path, reason: String) -> SessionPersistError {
    SessionPersistError::TornTailQuarantine {
        path: display_path.to_path_buf(),
        reason,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
