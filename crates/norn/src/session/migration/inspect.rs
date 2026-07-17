use std::io::Seek as _;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::session::persistence::acquire_private_fs;
use crate::session::persistence::strict::{ResumeFidelity, validate_staged_store};
use crate::util::PrivateRootReader;

use super::classify::decode_relative_path;
use super::cutover::verify_published_cutover;
use super::error::SessionMigrationError;
use super::manifest_sources::validate_manifest_sources;
use super::stage_ownership::{StageKind, verify_owned_directory};
use super::transaction::{
    backup_tree_relative, open_verified_tree, path_to_manifest_string, read_manifest,
    validate_norn_root,
};
use super::tree::digest_tree;
use super::types::{
    LEGACY_SESSION_DIRECTORY, LegacySessionMigrationRecord, MIGRATION_MANIFEST_FILE,
    STRICT_SESSION_DIRECTORY, SessionMigrationManifest,
};

/// Read and verify the published migration receipt used for legacy inspection.
///
/// This is observational: it never creates, hardens, repairs, or removes a
/// filesystem entry. The complete immutable backup digest and every manifest
/// source selector are verified before the receipt is returned.
pub fn read_legacy_migration_manifest(
    norn_root: &Path,
) -> Result<SessionMigrationManifest, SessionMigrationError> {
    let _permit =
        acquire_private_fs().map_err(|error| SessionMigrationError::DescriptorAdmission {
            reason: error.to_string(),
        })?;
    let (manifest, _) = load_verified_manifest(norn_root)?;
    Ok(manifest)
}

/// Verify the bounded cutover proof entirely inside the strict active store.
///
/// This strictly decodes the fixed-size versioned receipt and ownership marker,
/// then checks that the required regular files exist. It never hashes or decodes
/// migration evidence, opens legacy or backup storage, or reads a session
/// timeline, so its work is bounded and safe on normal startup.
pub fn verify_legacy_session_cutover(norn_root: &Path) -> Result<(), SessionMigrationError> {
    validate_norn_root(norn_root)?;
    let _permit =
        acquire_private_fs().map_err(|error| SessionMigrationError::DescriptorAdmission {
            reason: error.to_string(),
        })?;
    verify_published_cutover(norn_root)
}

/// Verify that the live legacy namespace has one complete published migration.
///
/// This explicit offline audit validates the versioned migration manifest and
/// ownership receipts, every strict index/timeline row, the complete immutable
/// backup, every manifest source selector, and the current legacy source tree.
/// Its work is proportional to retained history. It never creates, repairs,
/// hardens, or removes a filesystem entry.
pub fn verify_legacy_session_migration(
    norn_root: &Path,
) -> Result<SessionMigrationManifest, SessionMigrationError> {
    let _permit =
        acquire_private_fs().map_err(|error| SessionMigrationError::DescriptorAdmission {
            reason: error.to_string(),
        })?;
    let (manifest, backup) = load_verified_manifest(norn_root)?;
    drop(backup);
    let store_path = norn_root.join(STRICT_SESSION_DIRECTORY);
    let _validated_store = validate_staged_store(&store_path)?;

    let source_path = norn_root.join(LEGACY_SESSION_DIRECTORY);
    let source = match PrivateRootReader::open(&source_path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SessionMigrationError::LegacySourceMissing { path: source_path });
        }
        Err(error) => return Err(SessionMigrationError::observation(source_path, error)),
    };
    let tree = source
        .read_tree()
        .map_err(|error| SessionMigrationError::observation(&source_path, error))?;
    let current_sha256 = digest_tree(&source, &tree)?;
    if current_sha256 != manifest.source_tree_sha256 {
        return Err(SessionMigrationError::LegacySourceDiverged {
            published_sha256: manifest.source_tree_sha256,
            current_sha256,
        });
    }
    Ok(manifest)
}

/// Stream one inspect-only legacy source exactly as retained in its immutable backup.
///
/// No JSON, Markdown, or synthetic event projection is offered for an ambiguous
/// source. The exact source file is hash-verified first, rewound on the same
/// descriptor, and then copied byte-for-byte to `output`.
pub fn export_legacy_session_raw(
    norn_root: &Path,
    catalog_id: &str,
    mut output: impl std::io::Write,
) -> Result<LegacySessionMigrationRecord, SessionMigrationError> {
    let _permit =
        acquire_private_fs().map_err(|error| SessionMigrationError::DescriptorAdmission {
            reason: error.to_string(),
        })?;
    let (manifest, backup) = load_verified_manifest(norn_root)?;
    let record = manifest
        .sessions
        .iter()
        .find(|record| {
            record.fidelity == ResumeFidelity::InspectOnly
                && record.catalog_id.as_deref() == Some(catalog_id)
        })
        .cloned()
        .ok_or_else(|| SessionMigrationError::LegacyCatalogNotFound {
            catalog_id: catalog_id.to_owned(),
        })?;
    let source_path = record.source_path.as_deref().ok_or_else(|| {
        SessionMigrationError::UnrepresentableSource {
            reason: format!("legacy catalog record '{catalog_id}' has no source path"),
        }
    })?;
    let expected_sha256 = record.source_sha256.as_deref().ok_or_else(|| {
        SessionMigrationError::UnrepresentableSource {
            reason: format!("legacy catalog record '{catalog_id}' has no source digest"),
        }
    })?;
    let relative = decode_relative_path(source_path)?;
    let mut source = backup
        .open_file(&relative)
        .map_err(|error| SessionMigrationError::observation(&relative, error))?;
    let mut sink = Sha256Sink::default();
    std::io::copy(&mut source, &mut sink)
        .map_err(|error| SessionMigrationError::observation(&relative, error))?;
    let actual_sha256 = sink.finish();
    if actual_sha256 != expected_sha256 {
        return Err(SessionMigrationError::BackupConflict {
            path: relative,
            existing_sha256: actual_sha256,
            source_sha256: expected_sha256.to_owned(),
        });
    }
    source
        .rewind()
        .map_err(|error| SessionMigrationError::observation(&relative, error))?;
    std::io::copy(&mut source, &mut output)
        .map_err(|error| SessionMigrationError::observation(&relative, error))?;
    output.flush().map_err(|error| {
        SessionMigrationError::observation(PathBuf::from("legacy export output"), error)
    })?;
    Ok(record)
}

fn load_verified_manifest(
    norn_root: &Path,
) -> Result<(SessionMigrationManifest, PrivateRootReader), SessionMigrationError> {
    validate_norn_root(norn_root)?;
    let store_path = norn_root.join(STRICT_SESSION_DIRECTORY);
    let store = PrivateRootReader::open(&store_path)
        .map_err(|error| SessionMigrationError::observation(&store_path, error))?;
    let manifest = read_manifest(&store, &store_path)?;
    let backup_relative = backup_tree_relative(&manifest.source_tree_sha256);
    let expected_path = path_to_manifest_string(&backup_relative)?;
    if manifest.backup_path != expected_path {
        return Err(SessionMigrationError::UnrepresentableSource {
            reason: format!(
                "{} names backup '{}' instead of '{expected_path}'",
                MIGRATION_MANIFEST_FILE, manifest.backup_path,
            ),
        });
    }
    let backup_path = norn_root.join(&backup_relative);
    let backup =
        open_verified_tree(&backup_path, &manifest.source_tree_sha256)?.ok_or_else(|| {
            SessionMigrationError::BackupConflict {
                path: backup_path,
                existing_sha256: "missing".to_owned(),
                source_sha256: manifest.source_tree_sha256.clone(),
            }
        })?;
    let backup_container =
        backup_relative
            .parent()
            .ok_or_else(|| SessionMigrationError::UnrepresentableSource {
                reason: "migration backup selector has no container".to_owned(),
            })?;
    verify_owned_directory(
        norn_root,
        backup_container,
        StageKind::Backup,
        Some(&manifest.source_tree_sha256),
    )?;
    validate_manifest_sources(&manifest, &backup)?;
    Ok((manifest, backup))
}

#[derive(Default)]
struct Sha256Sink(Sha256);

impl Sha256Sink {
    fn finish(self) -> String {
        format!("{:x}", self.0.finalize())
    }
}

impl std::io::Write for Sha256Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
