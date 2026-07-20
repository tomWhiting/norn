use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::events::SessionEvent;
use crate::session::response_audio::{
    response_audio_artifact_path, validate_response_audio_stream,
};
use crate::session::spool::registered_root_session_id;
use crate::session::{ResponseAudioArtifactRef, SessionIndexEntry, SessionPersistError};
use crate::util::{PrivateEntryKind, PrivateRoot};

use super::names::audio_stage_path;
use super::publication_audio_links::{collect_reference_requirements, validate_link_binding};
use super::publication_conflict::conflict;
use super::publication_hash::HashingReader;

const ARTIFACTS_DIRECTORY: &str = "artifacts";
const RESPONSE_AUDIO_DIRECTORY: &str = "response-audio";

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AudioBundleJournal {
    pub(super) source_session_id: String,
    pub(super) source_generation: uuid::Uuid,
    pub(super) files: Vec<AudioFileFacts>,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AudioFileFacts {
    pub(super) reference: ResponseAudioArtifactRef,
    pub(super) bytes: u64,
    pub(super) sha256: String,
    pub(super) sealed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) terminal_response_id: Option<String>,
}

struct FileDigest {
    bytes: u64,
    sha256: String,
}

pub(super) fn validate_source_generation(
    entries: &[SessionIndexEntry],
    source: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    let current = entries.iter().find(|entry| {
        entry.id == source.id
            && entry.generation == source.generation
            && entry.rel_path == source.rel_path
    });
    let Some(current) = current else {
        return Err(SessionPersistError::GenerationChanged {
            id: source.id.clone(),
        });
    };
    if current.provider_state_identity == source.provider_state_identity {
        Ok(())
    } else {
        Err(SessionPersistError::ProviderStateIdentityMismatch)
    }
}

pub(super) fn stage_audio_bundle(
    root: &PrivateRoot,
    transaction_id: &str,
    source: &SessionIndexEntry,
    destination: &SessionIndexEntry,
    events: &[SessionEvent],
) -> Result<Option<AudioBundleJournal>, SessionPersistError> {
    let references = collect_reference_requirements(events)?;
    if references.is_empty() {
        return Ok(None);
    }
    ensure_destination_unclaimed(root, destination)?;
    let stage = audio_stage_path(transaction_id);
    let response_audio_dir = stage
        .join(ARTIFACTS_DIRECTORY)
        .join(RESPONSE_AUDIO_DIRECTORY);
    let result = (|| {
        root.create_dir_all(&response_audio_dir)?;
        let mut files = Vec::with_capacity(references.len());
        let source_root = registered_root_session_id(source);
        for requirement in references {
            let reference = requirement.reference;
            let source_path = response_audio_artifact_path(source_root, reference);
            let destination_path = response_audio_dir.join(reference.file_name());
            let copied = copy_artifact(root, &source_path, &destination_path, reference)?;
            let validated = inspect_artifact(root, &destination_path, reference)?;
            if copied.bytes != validated.bytes || copied.sha256 != validated.sha256 {
                return Err(invalid_artifact(
                    reference,
                    "staged artifact changed after its exact copy",
                ));
            }
            validate_link_binding(
                &requirement,
                validated.sealed,
                validated.terminal_response_id.as_deref(),
            )?;
            files.push(validated);
        }
        root.sync_dir(&response_audio_dir)?;
        root.sync_dir(&stage.join(ARTIFACTS_DIRECTORY))?;
        root.sync_dir(&stage)?;
        root.sync_dir(Path::new(""))?;
        Ok(AudioBundleJournal {
            source_session_id: source.id.clone(),
            source_generation: source.generation,
            files,
        })
    })();
    if result.is_err() {
        remove_stage_after_failure(root, &stage);
    }
    result.map(Some)
}

pub(super) fn validate_audio_journal(
    journal: &AudioBundleJournal,
    destination_id: &str,
) -> Result<(), SessionPersistError> {
    if journal.source_session_id.is_empty()
        || journal.source_generation.get_version_num() != 4
        || journal.files.is_empty()
    {
        return Err(conflict(
            destination_id,
            "the publication audio manifest is empty or has no source",
        ));
    }
    let mut previous = None;
    for file in &journal.files {
        let name = file.reference.file_name();
        if previous.as_ref().is_some_and(|prior| prior >= &name) {
            return Err(conflict(
                destination_id,
                "the publication audio manifest is not uniquely sorted",
            ));
        }
        if !is_sha256(&file.sha256) || (!file.sealed && file.terminal_response_id.is_some()) {
            return Err(conflict(
                destination_id,
                "the publication audio manifest facts are malformed",
            ));
        }
        previous = Some(name);
    }
    Ok(())
}

pub(super) fn audio_stage_is_present(
    root: &PrivateRoot,
    transaction_id: &str,
) -> Result<bool, SessionPersistError> {
    top_level_directory_is_present(root, &audio_stage_path(transaction_id), transaction_id)
}

pub(super) fn recover_audio_bundle(
    root: &PrivateRoot,
    transaction_id: &str,
    destination: &SessionIndexEntry,
    manifest: &AudioBundleJournal,
) -> Result<bool, SessionPersistError> {
    let stage = audio_stage_path(transaction_id);
    let final_path = PathBuf::from(&destination.id);
    let stage_exists = inspect_bundle_if_present(root, &stage, destination, manifest)?;
    let final_exists = inspect_bundle_if_present(root, &final_path, destination, manifest)?;
    if !final_exists {
        if !stage_exists {
            return Err(conflict(
                &destination.id,
                "both staged and published response-audio bundles are missing",
            ));
        }
        root.publish_new_dir(&stage, &final_path)?;
        root.sync_dir(Path::new(""))?;
        if !inspect_bundle_if_present(root, &final_path, destination, manifest)? {
            return Err(conflict(
                &destination.id,
                "published response-audio bundle disappeared after publication",
            ));
        }
    }
    Ok(stage_exists)
}

pub(super) fn remove_verified_audio_stage(
    root: &PrivateRoot,
    transaction_id: &str,
    stage_was_verified: bool,
) -> Result<(), SessionPersistError> {
    if stage_was_verified {
        let stage = audio_stage_path(transaction_id);
        match root.remove_dir_all(&stage) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn ensure_destination_unclaimed(
    root: &PrivateRoot,
    destination: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    let path = Path::new(&destination.id);
    if top_level_entry(root, path)?.is_some() {
        return Err(conflict(
            &destination.id,
            "the response-audio destination directory is already occupied",
        ));
    }
    Ok(())
}

fn inspect_bundle_if_present(
    root: &PrivateRoot,
    base: &Path,
    destination: &SessionIndexEntry,
    manifest: &AudioBundleJournal,
) -> Result<bool, SessionPersistError> {
    let Some(kind) = top_level_entry(root, base)? else {
        return Ok(false);
    };
    if kind != PrivateEntryKind::Directory {
        return Err(conflict(
            &destination.id,
            "the response-audio bundle path is not a directory",
        ));
    }
    exact_directory(
        root,
        base,
        [(OsStr::new(ARTIFACTS_DIRECTORY), PrivateEntryKind::Directory)],
        &destination.id,
    )?;
    let artifacts = base.join(ARTIFACTS_DIRECTORY);
    exact_directory(
        root,
        &artifacts,
        [(
            OsStr::new(RESPONSE_AUDIO_DIRECTORY),
            PrivateEntryKind::Directory,
        )],
        &destination.id,
    )?;
    let audio = artifacts.join(RESPONSE_AUDIO_DIRECTORY);
    let expected = manifest
        .files
        .iter()
        .map(|file| (file.reference.file_name(), PrivateEntryKind::File))
        .collect::<BTreeMap<_, _>>();
    exact_owned_audio_files(root, &audio, &expected, &destination.id)?;
    for facts in &manifest.files {
        let path = audio.join(facts.reference.file_name());
        let actual = inspect_artifact(root, &path, facts.reference)?;
        if &actual != facts {
            return Err(conflict(
                &destination.id,
                "a response-audio artifact does not match its publication manifest",
            ));
        }
    }
    Ok(true)
}

fn exact_directory<const N: usize>(
    root: &PrivateRoot,
    path: &Path,
    expected: [(&OsStr, PrivateEntryKind); N],
    destination_id: &str,
) -> Result<(), SessionPersistError> {
    let expected = expected.into_iter().collect::<BTreeMap<_, _>>();
    let actual = root.read_dir(path)?;
    if actual.len() != expected.len()
        || actual
            .iter()
            .any(|entry| expected.get(entry.name.as_os_str()).copied() != Some(entry.kind))
    {
        return Err(conflict(
            destination_id,
            "the response-audio bundle directory shape disagrees with its journal",
        ));
    }
    Ok(())
}

fn exact_owned_audio_files(
    root: &PrivateRoot,
    path: &Path,
    expected: &BTreeMap<String, PrivateEntryKind>,
    destination_id: &str,
) -> Result<(), SessionPersistError> {
    let actual = root.read_dir(path)?;
    if actual.len() != expected.len()
        || actual.iter().any(|entry| {
            entry.name.to_str().and_then(|name| expected.get(name)) != Some(&entry.kind)
        })
    {
        return Err(conflict(
            destination_id,
            "the response-audio bundle file inventory disagrees with its journal",
        ));
    }
    Ok(())
}

fn copy_artifact(
    root: &PrivateRoot,
    source_path: &Path,
    destination_path: &Path,
    reference: ResponseAudioArtifactRef,
) -> Result<FileDigest, SessionPersistError> {
    // The recovered index lock already owns the single six-descriptor private
    // filesystem permit. This pass holds exactly the source and stage files in
    // addition to that lock/root pair; it must not acquire a nested permit.
    let source = root.open_read(source_path)?;
    let initial_length = source.metadata()?.len();
    let mut reader = HashingReader::new(source);
    let mut destination = root.create_new(destination_path)?;
    let copied = io::copy(&mut reader, &mut destination)?;
    destination.flush()?;
    destination.sync_all()?;
    let final_length = reader.metadata_len()?;
    if initial_length != final_length
        || reader.bytes_read() != final_length
        || copied != final_length
    {
        return Err(invalid_artifact(
            reference,
            "artifact changed while it was being copied",
        ));
    }
    Ok(FileDigest {
        bytes: final_length,
        sha256: reader.finish_sha256(),
    })
}

fn inspect_artifact(
    root: &PrivateRoot,
    path: &Path,
    reference: ResponseAudioArtifactRef,
) -> Result<AudioFileFacts, SessionPersistError> {
    let file = root.open_read(path)?;
    let initial_length = file.metadata()?.len();
    let mut reader = HashingReader::new(file);
    let validated = validate_response_audio_stream(&mut reader, reference)?;
    let final_length = reader.metadata_len()?;
    if initial_length != final_length || reader.bytes_read() != final_length {
        return Err(invalid_artifact(
            reference,
            "artifact changed while it was being validated",
        ));
    }
    Ok(AudioFileFacts {
        reference,
        bytes: final_length,
        sha256: reader.finish_sha256(),
        sealed: validated.sealed,
        terminal_response_id: validated.response_id,
    })
}

fn top_level_directory_is_present(
    root: &PrivateRoot,
    path: &Path,
    transaction_id: &str,
) -> Result<bool, SessionPersistError> {
    match top_level_entry(root, path)? {
        None => Ok(false),
        Some(PrivateEntryKind::Directory) => Ok(true),
        Some(_) => Err(conflict(
            transaction_id,
            "the owned response-audio stage name is not a directory",
        )),
    }
}

fn top_level_entry(
    root: &PrivateRoot,
    path: &Path,
) -> Result<Option<PrivateEntryKind>, SessionPersistError> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("top-level publication path has no file name"))?;
    Ok(root
        .read_dir(Path::new(""))?
        .into_iter()
        .find(|entry| entry.name.as_os_str() == name)
        .map(|entry| entry.kind))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn invalid_artifact(
    reference: ResponseAudioArtifactRef,
    reason: &'static str,
) -> SessionPersistError {
    SessionPersistError::InvalidResponseAudioArtifact {
        artifact_id: reference.to_string(),
        reason,
    }
}

fn remove_stage_after_failure(root: &PrivateRoot, stage: &Path) {
    if let Err(error) = root.remove_dir_all(stage)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %root.display_path(stage).display(),
            %error,
            "failed to remove response-audio publication stage",
        );
    }
}
