//! Closed retained-evidence envelope shared by validation and authoring.

use serde::Deserialize;

use crate::{Digest, RepositoryPath};

use super::{ArtifactFamily, ObservationSource, SentinelClass, SyntheticPurpose};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceDocument {
    pub(super) schema_version: u32,
    pub(super) artifact_family: ArtifactFamily,
    pub(super) artifact_id: String,
    pub(super) synthetic_values: Vec<SyntheticValueDocument>,
    pub(super) observations: Vec<ObservationDocument>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SyntheticValueDocument {
    pub(super) id: String,
    pub(super) value: String,
    pub(super) generator: String,
    pub(super) provenance: RepositoryPath,
    pub(super) purpose: SyntheticPurpose,
    pub(super) sentinel_class: SentinelClass,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservationDocument {
    pub(super) id: String,
    pub(super) referenced_path: RepositoryPath,
    pub(super) referenced_family: ArtifactFamily,
    pub(super) source: ObservationSource,
    pub(super) synthetic_ids: Vec<String>,
    pub(super) digest: Digest,
}
