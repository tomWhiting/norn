use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::session::persistence::strict::ResumeFidelity;

/// Existing namespace consumed only by the explicit offline migrator.
pub const LEGACY_SESSION_DIRECTORY: &str = "sessions";
/// Versionless namespace used by the strict session runtime after cutover.
pub const STRICT_SESSION_DIRECTORY: &str = "session-store";
/// Deterministic manifest stored inside a migrated strict store.
pub const MIGRATION_MANIFEST_FILE: &str = "migration-manifest.json";

/// Version of the migration manifest itself, independent of session JSONL.
pub const MIGRATION_MANIFEST_VERSION: u32 = 1;

/// Why a legacy timeline cannot be treated as a canonical provider transcript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LegacyClassificationReason {
    /// A typed legacy assistant turn has no canonical Responses item vector.
    FlattenedAssistantTurn,
    /// A legacy timeline contains a provider-epoch marker that only the strict
    /// migrator is authorized to mint.
    SpoofedProviderEpochBoundary,
    /// The timeline predates explicit session headers.
    HeaderlessTimeline,
    /// The timeline contains no session events.
    EmptyTimeline,
    /// The source index metadata disagreed with the observed timeline and was
    /// deterministically recomputed.
    StaleIndexMetadata,
    /// The index row was unsafe, ambiguous, or not losslessly understood.
    InvalidIndexRow {
        /// One-based physical index line, when one existed.
        line: Option<u64>,
        /// Exact fail-closed diagnostic.
        diagnostic: String,
    },
    /// The named timeline was absent.
    MissingTimeline,
    /// The timeline contained syntax, schema, identity, or version ambiguity.
    InvalidTimeline {
        /// One-based physical timeline line, when localised.
        line: Option<u64>,
        /// Exact fail-closed diagnostic.
        diagnostic: String,
    },
    /// More than one index row claimed the same session identifier.
    DuplicateSessionId,
    /// More than one index row claimed the same timeline path.
    DuplicateTimelinePath,
    /// A JSONL timeline existed without a safe unique manifest row.
    OrphanTimeline,
}

/// One source timeline's migration decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacySessionMigrationRecord {
    /// Stable content-derived identifier for inspect/export lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
    /// Safe session id, when one could be recovered from a unique index row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// One-based source index line, when this record came from a row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_index_line: Option<u64>,
    /// Reversible source selector relative to `sessions`. Non-UTF-8 Unix paths
    /// use the `unix-path-hex:` representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// SHA-256 of the exact timeline bytes, when the file was readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    /// Observed legacy session format, when unambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_format: Option<u32>,
    /// Execution fidelity permitted after migration.
    pub fidelity: ResumeFidelity,
    /// Every reason contributing to a degraded or inspect-only decision.
    pub reasons: Vec<LegacyClassificationReason>,
    /// Strict-store timeline path, when a safe index entry could be emitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_path: Option<String>,
}

/// Deterministic receipt embedded in a published migrated store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMigrationManifest {
    /// Manifest schema version.
    pub manifest_version: u32,
    /// Version of the durable ownership receipts retained in published trees.
    pub stage_ownership_version: u32,
    /// Version of the bounded active-store cutover receipt.
    pub cutover_receipt_version: u32,
    /// SHA-256 of the complete source topology and every file byte.
    pub source_tree_sha256: String,
    /// Private backup path relative to the explicit Norn root.
    pub backup_path: String,
    /// Decisions sorted by source path, index line, and session id.
    pub sessions: Vec<LegacySessionMigrationRecord>,
}

impl SessionMigrationManifest {
    /// Construct a deterministically ordered receipt.
    pub(super) fn new(
        source_tree_sha256: String,
        backup_path: String,
        mut sessions: Vec<LegacySessionMigrationRecord>,
    ) -> Self {
        for session in &mut sessions {
            session.catalog_id = if session.fidelity == ResumeFidelity::InspectOnly {
                Some(catalog_id(session))
            } else {
                None
            };
        }
        sessions.sort_by(|left, right| {
            left.source_path
                .cmp(&right.source_path)
                .then_with(|| left.source_index_line.cmp(&right.source_index_line))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        Self {
            manifest_version: MIGRATION_MANIFEST_VERSION,
            stage_ownership_version: super::stage_ownership::STAGE_OWNERSHIP_VERSION,
            cutover_receipt_version: super::cutover::CUTOVER_RECEIPT_VERSION,
            source_tree_sha256,
            backup_path,
            sessions,
        }
    }

    pub(super) fn has_valid_catalog_ids(&self) -> bool {
        self.sessions.iter().all(|session| {
            if session.fidelity == ResumeFidelity::InspectOnly {
                session
                    .catalog_id
                    .as_deref()
                    .is_some_and(|actual| actual == catalog_id(session))
            } else {
                session.catalog_id.is_none()
            }
        })
    }
}

fn catalog_id(record: &LegacySessionMigrationRecord) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"norn-legacy-catalog-id\0");
    update_optional_string(&mut hasher, record.source_path.as_deref());
    update_optional_u64(&mut hasher, record.source_index_line);
    update_optional_string(&mut hasher, record.session_id.as_deref());
    update_optional_string(&mut hasher, record.source_sha256.as_deref());
    format!("legacy-{:x}", hasher.finalize())
}

fn update_optional_string(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update(b"S");
            hasher.update(Sha256::digest(value.as_bytes()));
        }
        None => hasher.update(b"N"),
    }
}

fn update_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update(b"S");
            hasher.update(value.to_be_bytes());
        }
        None => hasher.update(b"N"),
    }
}

/// Counts of the three owner-approved legacy classifications.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MigrationCounts {
    /// Canonically complete timelines.
    pub canonical: usize,
    /// Coherent flattened timelines requiring a fresh provider epoch.
    pub fresh_epoch_projection: usize,
    /// Corrupt or ambiguous timelines retained only for inspection/export.
    pub inspect_only: usize,
}

impl MigrationCounts {
    pub(super) fn from_manifest(manifest: &SessionMigrationManifest) -> Self {
        let mut counts = Self::default();
        for session in &manifest.sessions {
            match session.fidelity {
                ResumeFidelity::Canonical => counts.canonical += 1,
                ResumeFidelity::FreshEpochProjection => counts.fresh_epoch_projection += 1,
                ResumeFidelity::InspectOnly => counts.inspect_only += 1,
            }
        }
        counts
    }
}

/// Result of one explicit offline migration invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionMigrationOutcome {
    /// This invocation atomically published the strict store.
    Migrated {
        /// Accepted full-tree source digest.
        source_tree_sha256: String,
        /// Published versionless strict-store path.
        destination: PathBuf,
        /// Published private byte-identical backup path.
        backup: PathBuf,
        /// Classification counts.
        counts: MigrationCounts,
    },
    /// The same source digest was already published by a prior invocation.
    AlreadyMigrated {
        /// Accepted full-tree source digest.
        source_tree_sha256: String,
        /// Existing versionless strict-store path.
        destination: PathBuf,
        /// Existing private byte-identical backup path.
        backup: PathBuf,
        /// Classification counts from the retained manifest.
        counts: MigrationCounts,
    },
}
