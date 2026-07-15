//! Strict checked-in redaction-registry document.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::digest::Digest;
use crate::path::RepositoryPath;
use crate::strict_json::{StrictJsonError, decode_strict_json};

use super::authority::RedactionRegistry;
use super::model::{
    ArtifactFamily, ArtifactRegistration, ObservationRegistration, ObservationSource, PublicUrl,
    RegistrationError, SentinelClass, SyntheticPurpose, SyntheticRegistration,
};

const REGISTRY_DOCUMENT_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    schema_version: u32,
    artifacts: Vec<ArtifactDocument>,
    synthetics: Vec<SyntheticDocument>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactDocument {
    id: String,
    path: RepositoryPath,
    family: ArtifactFamily,
    sha256: Digest,
    synthetic_ids: Vec<String>,
    observations: Vec<ObservationDocument>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObservationDocument {
    id: String,
    referenced_path: RepositoryPath,
    referenced_family: ArtifactFamily,
    source: ObservationSourceDocument,
    synthetic_ids: Vec<String>,
    sha256: Digest,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ObservationSourceDocument {
    PublicUrl { url: PublicUrl },
    CodexSourcePin,
    LocalGate,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SyntheticDocument {
    id: String,
    value: String,
    generator: String,
    provenance: RepositoryPath,
    purpose: SyntheticPurpose,
    sentinel_class: SentinelClass,
}

pub(super) fn decode(bytes: &[u8]) -> Result<RedactionRegistry, RegistryDocumentError> {
    let document: RegistryDocument =
        decode_strict_json(bytes).map_err(RegistryDocumentError::Json)?;
    if document.schema_version != REGISTRY_DOCUMENT_SCHEMA_VERSION {
        return Err(RegistryDocumentError::SchemaVersion);
    }
    if document.artifacts.is_empty() || document.synthetics.is_empty() {
        return Err(RegistryDocumentError::EmptyAuthority);
    }
    let artifacts = document
        .artifacts
        .into_iter()
        .map(ArtifactDocument::into_registration)
        .collect::<Result<Vec<_>, _>>()?;
    let synthetics = document
        .synthetics
        .into_iter()
        .map(SyntheticDocument::into_registration)
        .collect::<Result<Vec<_>, _>>()?;
    RedactionRegistry::new(artifacts, synthetics).map_err(Into::into)
}

pub(super) fn encode(registry: &RedactionRegistry) -> Result<Vec<u8>, RegistryEncodeError> {
    if !registry.is_document_nonempty() {
        return Err(RegistryEncodeError::EmptyAuthority);
    }
    let document = RegistryDocument::from_registry(registry);
    let mut bytes =
        serde_json::to_vec_pretty(&document).map_err(RegistryEncodeError::Serialization)?;
    bytes.push(b'\n');
    Ok(bytes)
}

impl RegistryDocument {
    fn from_registry(registry: &RedactionRegistry) -> Self {
        let artifacts = registry
            .artifacts()
            .map(|(_, registration)| ArtifactDocument::from_registration(registration))
            .collect();
        let synthetics = registry
            .synthetics()
            .map(|(_, registration)| SyntheticDocument::from_registration(registration))
            .collect();
        Self {
            schema_version: REGISTRY_DOCUMENT_SCHEMA_VERSION,
            artifacts,
            synthetics,
        }
    }
}

impl ArtifactDocument {
    fn from_registration(registration: &ArtifactRegistration) -> Self {
        Self {
            id: registration.id().to_owned(),
            path: registration.path().clone(),
            family: registration.family(),
            sha256: registration.digest(),
            synthetic_ids: registration.synthetic_ids().to_vec(),
            observations: registration
                .observations()
                .iter()
                .map(ObservationDocument::from_registration)
                .collect(),
        }
    }

    fn into_registration(self) -> Result<ArtifactRegistration, RegistrationError> {
        let observations = self
            .observations
            .into_iter()
            .map(ObservationDocument::into_registration)
            .collect::<Result<Vec<_>, _>>()?;
        ArtifactRegistration::new(
            self.id,
            self.path,
            self.family,
            self.sha256,
            self.synthetic_ids,
            observations,
        )
    }
}

impl ObservationDocument {
    fn from_registration(registration: &ObservationRegistration) -> Self {
        Self {
            id: registration.id().to_owned(),
            referenced_path: registration.referenced_path().clone(),
            referenced_family: registration.referenced_family(),
            source: ObservationSourceDocument::from_source(registration.source()),
            synthetic_ids: registration.synthetic_ids().to_vec(),
            sha256: registration.digest(),
        }
    }

    fn into_registration(self) -> Result<ObservationRegistration, RegistrationError> {
        ObservationRegistration::new(
            self.id,
            self.referenced_path,
            self.referenced_family,
            self.source.into_source(),
            self.synthetic_ids,
            self.sha256,
        )
    }
}

impl ObservationSourceDocument {
    const fn from_source(source: ObservationSource) -> Self {
        match source {
            ObservationSource::PublicUrl(url) => Self::PublicUrl { url },
            ObservationSource::CodexSourcePin => Self::CodexSourcePin,
            ObservationSource::LocalGate => Self::LocalGate,
        }
    }

    const fn into_source(self) -> ObservationSource {
        match self {
            Self::PublicUrl { url } => ObservationSource::PublicUrl(url),
            Self::CodexSourcePin => ObservationSource::CodexSourcePin,
            Self::LocalGate => ObservationSource::LocalGate,
        }
    }
}

impl SyntheticDocument {
    fn from_registration(registration: &SyntheticRegistration) -> Self {
        Self {
            id: registration.id().to_owned(),
            value: registration.value().to_owned(),
            generator: registration.generator().to_owned(),
            provenance: registration.provenance().clone(),
            purpose: registration.purpose(),
            sentinel_class: registration.sentinel_class(),
        }
    }

    fn into_registration(self) -> Result<SyntheticRegistration, RegistrationError> {
        SyntheticRegistration::new(
            self.id,
            self.value,
            self.generator,
            self.provenance,
            self.purpose,
            self.sentinel_class,
        )
    }
}

/// Closed failure while decoding the checked-in P1 redaction registry.
#[derive(Debug, Error)]
pub enum RegistryDocumentError {
    /// JSON was malformed, duplicated a member, or violated the closed shape.
    #[error("redaction registry JSON is invalid")]
    Json(#[source] StrictJsonError),
    /// The document schema version is unsupported.
    #[error("redaction registry schema version is unsupported")]
    SchemaVersion,
    /// A checked-in P1 authority omitted every artifact or every sentinel.
    #[error("redaction registry authority is empty")]
    EmptyAuthority,
    /// A typed registration violated the closed authority contract.
    #[error("redaction registry authority is invalid")]
    Registration(#[from] RegistrationError),
}

/// Failure to encode deterministic checked-in P1 redaction authority.
#[derive(Debug, Error)]
pub enum RegistryEncodeError {
    /// A checked-in P1 authority cannot omit every artifact or every sentinel.
    #[error("redaction registry authority is empty")]
    EmptyAuthority,
    /// The closed registry could not be represented as JSON.
    #[error("redaction registry authority could not be encoded")]
    Serialization(#[source] serde_json::Error),
}
