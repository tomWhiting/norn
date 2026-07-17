use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use crate::session::persistence::acquire_private_fs;
use crate::session::persistence::strict::validate_staged_store;
use crate::util::{PrivateRoot, PrivateRootReader};

use super::classify::classify_legacy_store;
use super::cutover::{CUTOVER_RECEIPT_VERSION, verify_cutover_artifacts, write_cutover_receipt};
use super::error::SessionMigrationError;
use super::json::{decode_known_value, parse_unique_json};
use super::manifest_sources::validate_manifest_sources;
use super::stage::build_strict_stage;
use super::stage_ownership::{
    STAGE_OWNERSHIP_VERSION, StageKind, replace_owned_stage, verify_owned_directory,
    verify_reader_marker,
};
use super::tree::{copy_tree, digest_tree};
use super::types::{
    LEGACY_SESSION_DIRECTORY, MIGRATION_MANIFEST_FILE, MIGRATION_MANIFEST_VERSION, MigrationCounts,
    STRICT_SESSION_DIRECTORY, SessionMigrationManifest, SessionMigrationOutcome,
};

const BACKUP_DIRECTORY: &str = "session-migration-backups";
const MIGRATION_LOCK_FILE: &str = "session-migration.lock";
pub(super) const BACKUP_STAGE: &str = ".session-backup-stage";
pub(super) const STRICT_STAGE: &str = ".session-store-stage";

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MigrationCheckpoint {
    BackupPrepared,
    BackupPublished,
    BackupDurable,
    StrictStorePrepared,
    StrictStorePublished,
    StrictStoreDurable,
}

#[cfg(test)]
impl MigrationCheckpoint {
    pub(super) const fn evidence_name(self) -> &'static str {
        match self {
            Self::BackupPrepared => "backup_prepared",
            Self::BackupPublished => "backup_published",
            Self::BackupDurable => "backup_durable",
            Self::StrictStorePrepared => "strict_store_prepared",
            Self::StrictStorePublished => "strict_store_published",
            Self::StrictStoreDurable => "strict_store_durable",
        }
    }
}

/// Explicitly migrate the legacy `sessions` tree into the strict versionless store.
///
/// This entry point never resolves environment variables or default paths. The
/// caller must supply the absolute Norn root and invoke it as an offline
/// operation; normal session reads never call it.
pub fn migrate_legacy_sessions(
    norn_root: &Path,
) -> Result<SessionMigrationOutcome, SessionMigrationError> {
    #[cfg(test)]
    {
        migrate_legacy_sessions_inner(norn_root, &mut |_| Ok(()))
    }
    #[cfg(not(test))]
    {
        migrate_legacy_sessions_inner(norn_root)
    }
}

#[cfg(test)]
pub(super) fn migrate_legacy_sessions_with_hook(
    norn_root: &Path,
    checkpoint: &mut impl FnMut(MigrationCheckpoint) -> Result<(), SessionMigrationError>,
) -> Result<SessionMigrationOutcome, SessionMigrationError> {
    migrate_legacy_sessions_inner(norn_root, checkpoint)
}

fn migrate_legacy_sessions_inner(
    norn_root: &Path,
    #[cfg(test)] checkpoint: &mut impl FnMut(MigrationCheckpoint) -> Result<(), SessionMigrationError>,
) -> Result<SessionMigrationOutcome, SessionMigrationError> {
    validate_norn_root(norn_root)?;
    let descriptor_permit =
        acquire_private_fs().map_err(|error| SessionMigrationError::DescriptorAdmission {
            reason: error.to_string(),
        })?;
    let source_path = norn_root.join(LEGACY_SESSION_DIRECTORY);
    let source = open_source(&source_path)?;
    let source_tree = source
        .read_tree()
        .map_err(|error| SessionMigrationError::observation(&source_path, error))?;
    let source_sha256 = digest_tree(&source, &source_tree)?;
    let backup_relative = backup_tree_relative(&source_sha256);

    if let Some(outcome) = existing_outcome(norn_root, &source_sha256)? {
        drop(descriptor_permit);
        return Ok(outcome);
    }

    // Acceptance boundary: every live-source read/classification above was
    // observational. Only now may migration create, harden, or remove entries.
    let root = PrivateRoot::open(norn_root).map_err(|error| {
        SessionMigrationError::mutation("opening accepted Norn root", norn_root, error)
    })?;
    let migration_lock = root
        .open_lock(Path::new(MIGRATION_LOCK_FILE))
        .map_err(|error| {
            SessionMigrationError::mutation("opening migration lock", MIGRATION_LOCK_FILE, error)
        })?;
    migration_lock.lock().map_err(|error| {
        SessionMigrationError::mutation("acquiring migration lock", MIGRATION_LOCK_FILE, error)
    })?;
    if let Some(outcome) = existing_outcome(norn_root, &source_sha256)? {
        drop(migration_lock);
        drop(descriptor_permit);
        return Ok(outcome);
    }

    let backup_stage = PathBuf::from(BACKUP_STAGE);
    let strict_stage = PathBuf::from(STRICT_STAGE);

    let final_backup_path = norn_root.join(&backup_relative);
    let final_backup = if let Some(reader) = open_verified_tree(&final_backup_path, &source_sha256)?
    {
        verify_owned_directory(
            norn_root,
            &backup_container_relative(&source_sha256),
            StageKind::Backup,
            Some(&source_sha256),
        )?;
        BackupSnapshot::Published(reader)
    } else {
        replace_owned_stage(
            &root,
            norn_root,
            &backup_stage,
            StageKind::Backup,
            &source_sha256,
        )?;
        copy_tree(
            &source,
            &source_tree,
            &root,
            &backup_stage.join(LEGACY_SESSION_DIRECTORY),
        )?;
        let staged_path = norn_root.join(&backup_stage).join(LEGACY_SESSION_DIRECTORY);
        let staged = open_verified_tree(&staged_path, &source_sha256)?.ok_or_else(|| {
            SessionMigrationError::BackupConflict {
                path: staged_path,
                existing_sha256: "missing-after-copy".to_owned(),
                source_sha256: source_sha256.clone(),
            }
        })?;
        verify_owned_directory(
            norn_root,
            &backup_stage,
            StageKind::Backup,
            Some(&source_sha256),
        )?;
        BackupSnapshot::Staged(staged)
    };

    let backup_tree = final_backup
        .reader()
        .read_tree()
        .map_err(|error| SessionMigrationError::observation(&final_backup_path, error))?;
    let classified = classify_legacy_store(final_backup.reader(), &backup_tree)?;
    let manifest = SessionMigrationManifest::new(
        source_sha256.clone(),
        path_to_manifest_string(&backup_relative)?,
        classified.records.clone(),
    );
    validate_manifest_sources(&manifest, final_backup.reader())?;

    replace_owned_stage(
        &root,
        norn_root,
        &strict_stage,
        StageKind::StrictStore,
        &source_sha256,
    )?;
    build_strict_stage(
        final_backup.reader(),
        &backup_tree,
        &classified,
        &root,
        &strict_stage,
        &manifest,
    )?;
    let strict_stage_path = norn_root.join(&strict_stage);
    write_cutover_receipt(&root, &strict_stage, &source_sha256)?;
    let strict_stage_reader = PrivateRootReader::open(&strict_stage_path)
        .map_err(|error| SessionMigrationError::observation(&strict_stage_path, error))?;
    let staged_manifest = read_manifest(&strict_stage_reader, &strict_stage_path)?;
    if staged_manifest != manifest {
        return Err(SessionMigrationError::UnrepresentableSource {
            reason: "staged migration manifest changed before validation".to_owned(),
        });
    }
    validate_staged_store(&strict_stage_path)?;

    let current_source = open_source(&source_path)?;
    let current_tree = current_source
        .read_tree()
        .map_err(|error| SessionMigrationError::observation(&source_path, error))?;
    let current_sha256 = digest_tree(&current_source, &current_tree)?;
    if current_sha256 != source_sha256 {
        return Err(SessionMigrationError::SourceChanged {
            initial_sha256: source_sha256,
            final_sha256: current_sha256,
        });
    }

    if matches!(final_backup, BackupSnapshot::Staged(_)) {
        #[cfg(test)]
        checkpoint(MigrationCheckpoint::BackupPrepared)?;
        let backup_container = backup_container_relative(&source_sha256);
        root.create_dir_all(Path::new(BACKUP_DIRECTORY))
            .map_err(|error| {
                SessionMigrationError::mutation(
                    "creating migration backup namespace",
                    BACKUP_DIRECTORY,
                    error,
                )
            })?;
        root.publish_new_dir(&backup_stage, &backup_container)
            .map_err(|error| {
                SessionMigrationError::mutation(
                    "publishing immutable legacy backup",
                    &backup_container,
                    error,
                )
            })?;
        #[cfg(test)]
        checkpoint(MigrationCheckpoint::BackupPublished)?;
        root.sync_dir(Path::new(BACKUP_DIRECTORY))
            .map_err(|error| {
                SessionMigrationError::mutation(
                    "synchronizing migration backup namespace",
                    BACKUP_DIRECTORY,
                    error,
                )
            })?;
        #[cfg(test)]
        checkpoint(MigrationCheckpoint::BackupDurable)?;
    }
    #[cfg(test)]
    checkpoint(MigrationCheckpoint::StrictStorePrepared)?;
    root.publish_new_dir(&strict_stage, Path::new(STRICT_SESSION_DIRECTORY))
        .map_err(|error| {
            SessionMigrationError::mutation(
                "publishing strict session store",
                STRICT_SESSION_DIRECTORY,
                error,
            )
        })?;
    #[cfg(test)]
    checkpoint(MigrationCheckpoint::StrictStorePublished)?;
    root.sync_dir(Path::new("")).map_err(|error| {
        SessionMigrationError::mutation("synchronizing Norn root", norn_root, error)
    })?;
    #[cfg(test)]
    checkpoint(MigrationCheckpoint::StrictStoreDurable)?;

    let counts = MigrationCounts::from_manifest(&manifest);
    drop(migration_lock);
    drop(descriptor_permit);
    Ok(SessionMigrationOutcome::Migrated {
        source_tree_sha256: source_sha256,
        destination: norn_root.join(STRICT_SESSION_DIRECTORY),
        backup: final_backup_path,
        counts,
    })
}

enum BackupSnapshot {
    Published(PrivateRootReader),
    Staged(PrivateRootReader),
}

impl BackupSnapshot {
    fn reader(&self) -> &PrivateRootReader {
        match self {
            Self::Published(reader) | Self::Staged(reader) => reader,
        }
    }
}

fn existing_outcome(
    norn_root: &Path,
    source_sha256: &str,
) -> Result<Option<SessionMigrationOutcome>, SessionMigrationError> {
    let destination = norn_root.join(STRICT_SESSION_DIRECTORY);
    let reader = match PrivateRootReader::open(&destination) {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SessionMigrationError::observation(&destination, error)),
    };
    let manifest = read_manifest(&reader, &destination).map_err(|error| {
        let existing_sha256 = match error {
            SessionMigrationError::Observation { .. } => "unreadable-store",
            _ => "unrecognized-store",
        };
        SessionMigrationError::DestinationConflict {
            path: destination.clone(),
            existing_sha256: existing_sha256.to_owned(),
            source_sha256: source_sha256.to_owned(),
        }
    })?;
    if manifest.source_tree_sha256 != source_sha256 {
        return Err(SessionMigrationError::DestinationConflict {
            path: destination,
            existing_sha256: manifest.source_tree_sha256,
            source_sha256: source_sha256.to_owned(),
        });
    }
    let expected_backup = backup_tree_relative(source_sha256);
    if manifest.backup_path != path_to_manifest_string(&expected_backup)? {
        return Err(SessionMigrationError::DestinationConflict {
            path: destination,
            existing_sha256: manifest.source_tree_sha256,
            source_sha256: source_sha256.to_owned(),
        });
    }
    validate_staged_store(&destination)?;
    let backup_path = norn_root.join(&expected_backup);
    let backup = open_verified_tree(&backup_path, source_sha256)?.ok_or_else(|| {
        SessionMigrationError::BackupConflict {
            path: backup_path.clone(),
            existing_sha256: "missing".to_owned(),
            source_sha256: source_sha256.to_owned(),
        }
    })?;
    verify_owned_directory(
        norn_root,
        &backup_container_relative(source_sha256),
        StageKind::Backup,
        Some(source_sha256),
    )?;
    validate_manifest_sources(&manifest, &backup)?;
    Ok(Some(SessionMigrationOutcome::AlreadyMigrated {
        source_tree_sha256: source_sha256.to_owned(),
        destination,
        backup: backup_path,
        counts: MigrationCounts::from_manifest(&manifest),
    }))
}

pub(super) fn read_manifest(
    reader: &PrivateRootReader,
    root: &Path,
) -> Result<SessionMigrationManifest, SessionMigrationError> {
    let path = Path::new(MIGRATION_MANIFEST_FILE);
    let mut file = reader
        .open_file(path)
        .map_err(|error| SessionMigrationError::observation(root.join(path), error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| SessionMigrationError::observation(root.join(path), error))?;
    let value = parse_unique_json(&bytes)
        .map_err(|reason| SessionMigrationError::UnrepresentableSource { reason })?;
    let manifest: SessionMigrationManifest = decode_known_value(value)
        .map_err(|reason| SessionMigrationError::UnrepresentableSource { reason })?;
    if manifest.manifest_version != MIGRATION_MANIFEST_VERSION || !manifest.has_valid_catalog_ids()
    {
        return Err(SessionMigrationError::UnrepresentableSource {
            reason: format!(
                "unsupported migration manifest version {}",
                manifest.manifest_version
            ),
        });
    }
    if manifest.stage_ownership_version != STAGE_OWNERSHIP_VERSION {
        return Err(SessionMigrationError::UnrepresentableSource {
            reason: format!(
                "unsupported migration stage ownership version {}",
                manifest.stage_ownership_version
            ),
        });
    }
    if manifest.cutover_receipt_version != CUTOVER_RECEIPT_VERSION {
        return Err(SessionMigrationError::UnrepresentableSource {
            reason: format!(
                "unsupported migration cutover receipt version {}",
                manifest.cutover_receipt_version
            ),
        });
    }
    verify_reader_marker(
        reader,
        root,
        StageKind::StrictStore,
        Some(&manifest.source_tree_sha256),
    )?;
    verify_cutover_artifacts(reader, root)?;
    Ok(manifest)
}

pub(super) fn open_verified_tree(
    path: &Path,
    expected_sha256: &str,
) -> Result<Option<PrivateRootReader>, SessionMigrationError> {
    let reader = match PrivateRootReader::open(path) {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SessionMigrationError::observation(path, error)),
    };
    let tree = reader
        .read_tree()
        .map_err(|error| SessionMigrationError::observation(path, error))?;
    let actual = digest_tree(&reader, &tree)?;
    if actual != expected_sha256 {
        return Err(SessionMigrationError::BackupConflict {
            path: path.to_path_buf(),
            existing_sha256: actual,
            source_sha256: expected_sha256.to_owned(),
        });
    }
    Ok(Some(reader))
}

fn open_source(path: &Path) -> Result<PrivateRootReader, SessionMigrationError> {
    match PrivateRootReader::open(path) {
        Ok(reader) => Ok(reader),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(SessionMigrationError::LegacySourceMissing {
                path: path.to_path_buf(),
            })
        }
        Err(error) => Err(SessionMigrationError::observation(path, error)),
    }
}

fn backup_container_relative(digest: &str) -> PathBuf {
    PathBuf::from(BACKUP_DIRECTORY).join(digest)
}

pub(super) fn backup_tree_relative(digest: &str) -> PathBuf {
    backup_container_relative(digest).join(LEGACY_SESSION_DIRECTORY)
}

pub(super) fn validate_norn_root(path: &Path) -> Result<(), SessionMigrationError> {
    if !path.is_absolute() {
        return Err(SessionMigrationError::InvalidNornRoot {
            path: path.to_path_buf(),
        });
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => normalized.push(component.as_os_str()),
            Component::CurDir | Component::ParentDir => {
                return Err(SessionMigrationError::InvalidNornRoot {
                    path: path.to_path_buf(),
                });
            }
        }
    }
    if normalized != path {
        return Err(SessionMigrationError::InvalidNornRoot {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

pub(super) fn path_to_manifest_string(path: &Path) -> Result<String, SessionMigrationError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| SessionMigrationError::UnrepresentableSource {
            reason: format!(
                "migration-owned path is not valid UTF-8: {}",
                path.display()
            ),
        })
}
