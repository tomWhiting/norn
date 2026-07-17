use std::io::{Read as _, Write as _};
use std::path::Path;

use crate::util::{PrivateRoot, PrivateRootReader};

use super::error::SessionMigrationError;

pub(super) const MARKER_FILE: &str = ".norn-migration-stage-owner";
const MARKER_MAGIC: &str = "norn-session-migration-stage-v1";
const SHA256_HEX_LENGTH: usize = 64;
const MARKER_NEWLINE_COUNT: usize = 3;
pub(super) const STAGE_OWNERSHIP_VERSION: u32 = 1;

#[derive(Clone, Copy)]
pub(super) enum StageKind {
    Backup,
    StrictStore,
}

impl StageKind {
    fn label(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::StrictStore => "strict-store",
        }
    }
}

/// Remove an interrupted owned stage, then durably claim a fresh stage.
///
/// A directory without the exact marker is never recursively removed. The
/// marker remains inside the directory through final publication so there is
/// no proof-free crash window between staging and rename.
pub(super) fn replace_owned_stage(
    root: &PrivateRoot,
    norn_root: &Path,
    stage: &Path,
    kind: StageKind,
    source_sha256: &str,
) -> Result<(), SessionMigrationError> {
    if stage_exists(norn_root, stage)? {
        verify_owned_directory(norn_root, stage, kind, None)?;
        root.remove_dir_all(stage).map_err(|error| {
            SessionMigrationError::mutation(
                "removing owned interrupted migration stage",
                stage,
                error,
            )
        })?;
        root.sync_dir(Path::new("")).map_err(|error| {
            SessionMigrationError::mutation(
                "synchronizing removed migration stage",
                norn_root,
                error,
            )
        })?;
    }

    root.create_dir_all(stage).map_err(|error| {
        SessionMigrationError::mutation("creating owned migration stage", stage, error)
    })?;
    let marker_path = stage.join(MARKER_FILE);
    let mut marker = root.create_new(&marker_path).map_err(|error| {
        SessionMigrationError::mutation(
            "creating migration stage ownership marker",
            &marker_path,
            error,
        )
    })?;
    marker
        .write_all(marker_bytes(kind, source_sha256).as_bytes())
        .map_err(|error| {
            SessionMigrationError::mutation(
                "writing migration stage ownership marker",
                &marker_path,
                error,
            )
        })?;
    marker.sync_all().map_err(|error| {
        SessionMigrationError::mutation(
            "synchronizing migration stage ownership marker",
            &marker_path,
            error,
        )
    })?;
    root.sync_dir(stage).map_err(|error| {
        SessionMigrationError::mutation("synchronizing owned migration stage", stage, error)
    })?;
    root.sync_dir(Path::new("")).map_err(|error| {
        SessionMigrationError::mutation("publishing migration stage ownership", norn_root, error)
    })
}

fn stage_exists(norn_root: &Path, stage: &Path) -> Result<bool, SessionMigrationError> {
    let path = norn_root.join(stage);
    match PrivateRootReader::open(&path) {
        Ok(reader) => {
            drop(reader);
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SessionMigrationError::observation(path, error)),
    }
}

pub(super) fn verify_owned_directory(
    norn_root: &Path,
    stage: &Path,
    kind: StageKind,
    expected_sha256: Option<&str>,
) -> Result<(), SessionMigrationError> {
    let stage_path = norn_root.join(stage);
    let reader = PrivateRootReader::open(&stage_path)
        .map_err(|error| SessionMigrationError::observation(&stage_path, error))?;
    verify_reader_marker(&reader, &stage_path, kind, expected_sha256)
}

pub(super) fn verify_reader_marker(
    reader: &PrivateRootReader,
    directory_path: &Path,
    kind: StageKind,
    expected_sha256: Option<&str>,
) -> Result<(), SessionMigrationError> {
    let marker_relative = Path::new(MARKER_FILE);
    let marker_path = directory_path.join(marker_relative);
    let marker = reader.open_file(marker_relative).map_err(|error| {
        SessionMigrationError::StageOwnershipConflict {
            path: directory_path.to_path_buf(),
            reason: format!("ownership marker could not be opened: {error}"),
        }
    })?;
    let expected_length = MARKER_MAGIC
        .len()
        .checked_add(kind.label().len())
        .and_then(|length| length.checked_add(SHA256_HEX_LENGTH + MARKER_NEWLINE_COUNT))
        .ok_or_else(|| SessionMigrationError::UnrepresentableSource {
            reason: "stage ownership marker length overflowed".to_owned(),
        })?;
    let expected_length = u64::try_from(expected_length).map_err(|error| {
        SessionMigrationError::UnrepresentableSource {
            reason: format!("stage ownership marker length is not representable: {error}"),
        }
    })?;
    let read_limit = expected_length.checked_add(1).ok_or_else(|| {
        SessionMigrationError::UnrepresentableSource {
            reason: "stage ownership marker read length overflowed".to_owned(),
        }
    })?;
    let mut bytes = Vec::new();
    marker
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| SessionMigrationError::observation(&marker_path, error))?;
    let encoded = std::str::from_utf8(&bytes).map_err(|error| {
        SessionMigrationError::StageOwnershipConflict {
            path: directory_path.to_path_buf(),
            reason: format!("ownership marker is not UTF-8: {error}"),
        }
    })?;
    let mut lines = encoded.lines();
    let magic = lines.next();
    let recorded_kind = lines.next();
    let digest = lines.next();
    let exact_shape = lines.next().is_none() && encoded.ends_with('\n');
    let valid_digest = digest.is_some_and(|value| {
        value.len() == 64
            && value
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            && expected_sha256.is_none_or(|expected| value == expected)
    });
    if magic != Some(MARKER_MAGIC)
        || recorded_kind != Some(kind.label())
        || !valid_digest
        || !exact_shape
    {
        return Err(SessionMigrationError::StageOwnershipConflict {
            path: directory_path.to_path_buf(),
            reason: "ownership marker is malformed or names a different stage kind".to_owned(),
        });
    }
    Ok(())
}

fn marker_bytes(kind: StageKind, source_sha256: &str) -> String {
    format!("{MARKER_MAGIC}\n{}\n{source_sha256}\n", kind.label())
}
