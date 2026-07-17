use std::io::{BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::{SessionIndexEntry, SessionPersistError};
use crate::util::PrivateRoot;

use super::names::{journal_path, journal_temp_path};
use super::publication_audio::{AudioBundleJournal, validate_audio_journal};
use super::publication_conflict::conflict;
use super::publication_parent::{ParentPrecondition, validate_parent_precondition_shape};
use super::publication_recovery::remove_owned_after_failure;

pub(super) const TIMELINE_PUBLICATION_VERSION: u32 = 2;
pub(super) const AUDIO_PUBLICATION_VERSION: u32 = 3;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublicationJournal {
    pub(super) norn_session_publication: u32,
    pub(super) transaction_id: String,
    pub(super) parent_precondition: Option<ParentPrecondition>,
    pub(super) entry: SessionIndexEntry,
    pub(super) timeline_bytes: u64,
    pub(super) timeline_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) audio_bundle: Option<AudioBundleJournal>,
}

pub(super) fn validate_journal_metadata(
    journal: &PublicationJournal,
) -> Result<(), SessionPersistError> {
    let version_matches_payload = matches!(
        (journal.norn_session_publication, &journal.audio_bundle),
        (TIMELINE_PUBLICATION_VERSION, None) | (AUDIO_PUBLICATION_VERSION, Some(_))
    );
    if !version_matches_payload {
        return Err(conflict(
            &journal.entry.id,
            "the publication journal version is unsupported",
        ));
    }
    if let Some(audio_bundle) = &journal.audio_bundle {
        if journal.parent_precondition.is_some()
            || journal.entry.parent_id.is_some()
            || journal.entry.rel_path.is_some()
        {
            return Err(conflict(
                &journal.entry.id,
                "response-audio publication is only valid for a root-session fork",
            ));
        }
        validate_audio_journal(audio_bundle, &journal.entry.id)?;
    }
    if journal.timeline_sha256.len() != 64
        || !journal
            .timeline_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(conflict(
            &journal.entry.id,
            "the publication journal timeline digest is malformed",
        ));
    }
    validate_parent_precondition_shape(&journal.entry, journal.parent_precondition.as_ref())
}

pub(super) fn write_journal(
    root: &PrivateRoot,
    journal: &PublicationJournal,
) -> Result<PathBuf, SessionPersistError> {
    let temporary = journal_temp_path(&journal.transaction_id);
    let final_path = journal_path(&journal.transaction_id);
    let result = (|| {
        let file = root.create_new(&temporary)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, journal)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        let file = writer
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        file.sync_all()?;
        root.publish_new(&temporary, &final_path)?;
        root.sync_dir(Path::new(""))?;
        Ok(final_path.clone())
    })();
    if result.is_err() {
        remove_owned_after_failure(root, &temporary);
    }
    result
}

pub(super) fn read_journal(
    root: &PrivateRoot,
    path: &Path,
    transaction_id: &str,
) -> Result<PublicationJournal, SessionPersistError> {
    let file = root.open_read(path)?;
    let journal: PublicationJournal = serde_json::from_reader(BufReader::new(file))?;
    if journal.transaction_id != transaction_id {
        return Err(conflict(
            &journal.entry.id,
            "the publication journal transaction id does not match its owned file name",
        ));
    }
    Ok(journal)
}
