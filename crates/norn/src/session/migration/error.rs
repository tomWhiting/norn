use std::io;
use std::path::PathBuf;

use thiserror::Error;

use crate::session::persistence::strict::StrictStoreError;

/// A fail-closed error from the explicit legacy-session migration.
#[derive(Debug, Error)]
pub enum SessionMigrationError {
    /// The process descriptor governor could not admit the offline transaction.
    #[error("session migration descriptor admission failed: {reason}")]
    DescriptorAdmission {
        /// Self-diagnosing admission failure.
        reason: String,
    },
    /// The caller did not supply an absolute, normalized Norn root.
    #[error("session migration requires an absolute normalized Norn root, found {}", path.display())]
    InvalidNornRoot {
        /// Rejected root.
        path: PathBuf,
    },
    /// The legacy source tree does not exist.
    #[error("legacy session source does not exist at {}", path.display())]
    LegacySourceMissing {
        /// Expected `sessions` directory.
        path: PathBuf,
    },
    /// Descriptor-relative source observation failed.
    #[error("could not observe legacy session source at {}: {source}", path.display())]
    Observation {
        /// Path being observed.
        path: PathBuf,
        /// Underlying observational error.
        #[source]
        source: io::Error,
    },
    /// A source path or count could not be represented by the manifest format.
    #[error("legacy session source cannot be represented safely: {reason}")]
    UnrepresentableSource {
        /// Exact representation failure.
        reason: String,
    },
    /// Filesystem mutation failed after the source was accepted.
    #[error("session migration failed while {operation} at {}: {source}", path.display())]
    Mutation {
        /// Operation being performed.
        operation: &'static str,
        /// Path being changed.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// Encoding a deterministic migration artifact failed.
    #[error("could not encode session migration artifact: {0}")]
    Encoding(#[from] serde_json::Error),
    /// The staged strict store failed its independent fail-closed validator.
    #[error("staged session migration did not validate: {0}")]
    StrictValidation(#[from] StrictStoreError),
    /// The legacy source changed after it was classified and copied.
    #[error(
        "legacy session source changed during migration (initial {initial_sha256}, final {final_sha256}); nothing was published"
    )]
    SourceChanged {
        /// Digest accepted before mutation.
        initial_sha256: String,
        /// Digest observed before publication.
        final_sha256: String,
    },
    /// The retained legacy source no longer matches the published receipt.
    #[error(
        "legacy session source changed after migration (published {published_sha256}, current {current_sha256}); offline migration verification failed"
    )]
    LegacySourceDiverged {
        /// Digest recorded by the published migration manifest.
        published_sha256: String,
        /// Digest observed from the current legacy tree.
        current_sha256: String,
    },
    /// A fixed stage name exists without proof that the migrator owns it.
    #[error(
        "refusing to remove unowned migration stage at {}: {reason}",
        path.display()
    )]
    StageOwnershipConflict {
        /// Preserved stage path.
        path: PathBuf,
        /// Why ownership could not be proven.
        reason: String,
    },
    /// An immutable publication artifact no longer matches the cutover receipt.
    #[error(
        "migration cutover receipt expected {} to hash to {expected_sha256}, found {actual_sha256}",
        path.display()
    )]
    CutoverReceiptConflict {
        /// Changed published metadata file.
        path: PathBuf,
        /// Digest recorded before atomic publication.
        expected_sha256: String,
        /// Digest observed during explicit offline verification.
        actual_sha256: String,
    },
    /// A published destination belongs to different source content.
    #[error(
        "session-store already exists at {} for source digest {existing_sha256}; current legacy source digest is {source_sha256}",
        path.display()
    )]
    DestinationConflict {
        /// Existing versionless strict-store path.
        path: PathBuf,
        /// Digest recorded by the existing store, or a typed marker that it
        /// is not a migration store.
        existing_sha256: String,
        /// Digest of the accepted legacy source.
        source_sha256: String,
    },
    /// A published backup path exists but is not the expected byte-identical tree.
    #[error(
        "legacy-session backup at {} has digest {existing_sha256}, expected {source_sha256}",
        path.display()
    )]
    BackupConflict {
        /// Existing backup path.
        path: PathBuf,
        /// Digest observed for the backup.
        existing_sha256: String,
        /// Digest of the accepted source.
        source_sha256: String,
    },
    /// No inspect-only record has the requested stable catalog identifier.
    #[error("no inspect-only legacy session matches catalog id '{catalog_id}'")]
    LegacyCatalogNotFound {
        /// Requested content-derived catalog identifier.
        catalog_id: String,
    },
}

impl SessionMigrationError {
    pub(super) fn observation(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Observation {
            path: path.into(),
            source,
        }
    }

    pub(super) fn mutation(
        operation: &'static str,
        path: impl Into<PathBuf>,
        source: io::Error,
    ) -> Self {
        Self::Mutation {
            operation,
            path: path.into(),
            source,
        }
    }
}
