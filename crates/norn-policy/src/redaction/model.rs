use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::digest::Digest;
use crate::path::RepositoryPath;

use super::path_policy::{
    PROVENANCE_ROOTS, is_authority_path, is_machine_token, validate_artifact_path,
    validate_machine_id, validate_sorted_ids,
};

/// A closed retained-artifact family.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactFamily {
    /// Public or Codex request, response, stream, manifest, or pin fixture.
    ProtocolFixture,
    /// Digest-pinned reviewed traceability source encoded as strict JSON Lines.
    TraceabilityJsonl,
    /// A digest-pinned reviewed contract, inventory, or schema document.
    ContractSchema,
    /// One closed local-gate run descriptor.
    GateDescriptor,
    /// One closed repeated-test distribution.
    Distribution,
    /// A pre-sanitized, digest-pinned command or diagnostic log.
    SanitizedLog,
    /// Digest-pinned reviewed source that deterministically produces evidence.
    EvidenceToolSource,
}

impl ArtifactFamily {
    /// Return the immutable schema version for this family.
    #[must_use]
    pub const fn schema_version(self) -> u32 {
        1
    }

    /// Return the machine spelling used by retained documents and digests.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolFixture => "protocol_fixture",
            Self::TraceabilityJsonl => "traceability_jsonl",
            Self::ContractSchema => "contract_schema",
            Self::GateDescriptor => "gate_descriptor",
            Self::Distribution => "distribution",
            Self::SanitizedLog => "sanitized_log",
            Self::EvidenceToolSource => "evidence_tool_source",
        }
    }
}

/// Fixed public URLs admitted by observation tuples.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PublicUrl {
    /// Public Responses create endpoint source.
    OpenAiResponsesEndpoint,
    /// Public Responses compact endpoint source.
    OpenAiCompactEndpoint,
    /// `OpenAI` Responses streaming-events reference.
    OpenAiStreamingEvents,
    /// `OpenAI` Responses WebSocket-events reference.
    OpenAiWebsocketEvents,
}

impl PublicUrl {
    /// Return the immutable URL represented by this value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponsesEndpoint => "https://api.openai.com/v1/responses",
            Self::OpenAiCompactEndpoint => "https://api.openai.com/v1/responses/compact",
            Self::OpenAiStreamingEvents => {
                "https://developers.openai.com/api/reference/resources/responses/streaming-events"
            }
            Self::OpenAiWebsocketEvents => {
                "https://developers.openai.com/api/reference/resources/responses/websocket-events"
            }
        }
    }
}

impl Serialize for PublicUrl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PublicUrl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "https://api.openai.com/v1/responses" => Ok(Self::OpenAiResponsesEndpoint),
            "https://api.openai.com/v1/responses/compact" => Ok(Self::OpenAiCompactEndpoint),
            "https://developers.openai.com/api/reference/resources/responses/streaming-events" => {
                Ok(Self::OpenAiStreamingEvents)
            }
            "https://developers.openai.com/api/reference/resources/responses/websocket-events" => {
                Ok(Self::OpenAiWebsocketEvents)
            }
            _ => Err(serde::de::Error::custom("unregistered public URL")),
        }
    }
}

/// Closed provenance kind for an observation tuple.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// An exact immutable public documentation URL.
    PublicUrl(PublicUrl),
    /// A separately registered Codex repository/blob pin artifact.
    CodexSourcePin,
    /// A local-gate observation with no external URL.
    LocalGate,
}

impl ObservationSource {
    pub(crate) fn digest_name(self) -> &'static str {
        match self {
            Self::PublicUrl(PublicUrl::OpenAiResponsesEndpoint) => "public_responses_endpoint",
            Self::PublicUrl(PublicUrl::OpenAiCompactEndpoint) => "public_compact_endpoint",
            Self::PublicUrl(PublicUrl::OpenAiStreamingEvents) => "public_streaming_events",
            Self::PublicUrl(PublicUrl::OpenAiWebsocketEvents) => "public_websocket_events",
            Self::CodexSourcePin => "codex_source_pin",
            Self::LocalGate => "local_gate",
        }
    }
}

/// Non-reusable provenance class required for every opaque sentinel.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SentinelClass {
    /// A deterministic sentinel invalid outside its generating fixture.
    NonReusableFixtureV1,
}

/// The sensitive protocol role served by a synthetic sentinel.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntheticPurpose {
    /// Opaque fixture prose with no sensitive protocol meaning.
    Generic,
    /// A deliberately invalid synthetic account identity.
    AccountId,
    /// A deliberately invalid synthetic credential value.
    Credential,
    /// Synthetic prompt, response, refusal, or delta content.
    PromptContent,
    /// Synthetic reusable-state-shaped protocol content.
    TurnState,
    /// Synthetic cache-key-shaped protocol content.
    CacheKey,
}

/// Reviewed authority for one exact opaque synthetic value.
#[derive(Clone, Eq, PartialEq)]
pub struct SyntheticRegistration {
    id: String,
    value: String,
    generator: String,
    provenance: RepositoryPath,
    purpose: SyntheticPurpose,
    sentinel_class: SentinelClass,
}

impl SyntheticRegistration {
    /// Register an exact non-reusable synthetic value and provenance.
    pub fn new(
        id: impl Into<String>,
        value: impl Into<String>,
        generator: impl Into<String>,
        provenance: RepositoryPath,
        purpose: SyntheticPurpose,
        sentinel_class: SentinelClass,
    ) -> Result<Self, RegistrationError> {
        let id = id.into();
        let value = value.into();
        let generator = generator.into();
        validate_machine_id(&id)?;
        validate_machine_id(&generator)?;
        if !value.starts_with("norn-synthetic-") || !is_machine_token(&value, 256) {
            return Err(RegistrationError::InvalidSyntheticValue);
        }
        if !is_authority_path(&provenance, PROVENANCE_ROOTS) {
            return Err(RegistrationError::InvalidProvenancePath);
        }
        Ok(Self {
            id,
            value,
            generator,
            provenance,
            purpose,
            sentinel_class,
        })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn generator(&self) -> &str {
        &self.generator
    }

    pub(crate) fn provenance(&self) -> &RepositoryPath {
        &self.provenance
    }

    pub(crate) const fn purpose(&self) -> SyntheticPurpose {
        self.purpose
    }

    pub(crate) const fn sentinel_class(&self) -> SentinelClass {
        self.sentinel_class
    }

    pub(crate) fn matches_document(
        &self,
        value: &str,
        generator: &str,
        provenance: &RepositoryPath,
        purpose: SyntheticPurpose,
        sentinel_class: SentinelClass,
    ) -> bool {
        self.value == value
            && self.generator == generator
            && self.provenance == *provenance
            && self.purpose == purpose
            && self.sentinel_class == sentinel_class
    }
}

impl fmt::Debug for SyntheticRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyntheticRegistration")
            .field("id", &self.id)
            .field("purpose", &self.purpose)
            .field("value", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// One exact observation-to-artifact binding.
#[derive(Clone, Eq, PartialEq)]
pub struct ObservationRegistration {
    id: String,
    referenced_path: RepositoryPath,
    referenced_family: ArtifactFamily,
    source: ObservationSource,
    synthetic_ids: Vec<String>,
    digest: Digest,
}

impl ObservationRegistration {
    /// Register one indivisible observation tuple.
    pub fn new(
        id: impl Into<String>,
        referenced_path: RepositoryPath,
        referenced_family: ArtifactFamily,
        source: ObservationSource,
        synthetic_ids: Vec<String>,
        digest: Digest,
    ) -> Result<Self, RegistrationError> {
        let id = id.into();
        validate_machine_id(&id)?;
        validate_artifact_path(&referenced_path, referenced_family)?;
        validate_sorted_ids(&synthetic_ids)?;
        Ok(Self {
            id,
            referenced_path,
            referenced_family,
            source,
            synthetic_ids,
            digest,
        })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn referenced_path(&self) -> &RepositoryPath {
        &self.referenced_path
    }

    pub(crate) const fn referenced_family(&self) -> ArtifactFamily {
        self.referenced_family
    }

    pub(crate) const fn source(&self) -> ObservationSource {
        self.source
    }

    pub(crate) fn synthetic_ids(&self) -> &[String] {
        &self.synthetic_ids
    }

    pub(crate) const fn digest(&self) -> Digest {
        self.digest
    }
}

impl fmt::Debug for ObservationRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservationRegistration")
            .field("id", &self.id)
            .field("referenced_family", &self.referenced_family)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

/// Closed authority for one exact retained artifact.
#[derive(Clone, Eq, PartialEq)]
pub struct ArtifactRegistration {
    id: String,
    path: RepositoryPath,
    family: ArtifactFamily,
    digest: Digest,
    synthetic_ids: Vec<String>,
    observations: Vec<ObservationRegistration>,
}

impl ArtifactRegistration {
    /// Register exact identity, path, family, digest, sentinels, and tuples.
    pub fn new(
        id: impl Into<String>,
        path: RepositoryPath,
        family: ArtifactFamily,
        digest: Digest,
        synthetic_ids: Vec<String>,
        observations: Vec<ObservationRegistration>,
    ) -> Result<Self, RegistrationError> {
        let id = id.into();
        validate_machine_id(&id)?;
        validate_artifact_path(&path, family)?;
        validate_sorted_ids(&synthetic_ids)?;
        if !observations.windows(2).all(|pair| pair[0].id < pair[1].id) {
            return Err(RegistrationError::UnstableAuthorityOrder);
        }
        let tuple_family = matches!(
            family,
            ArtifactFamily::GateDescriptor | ArtifactFamily::Distribution
        );
        if tuple_family == observations.is_empty() {
            return Err(RegistrationError::InvalidFamilyAuthority);
        }
        Ok(Self {
            id,
            path,
            family,
            digest,
            synthetic_ids,
            observations,
        })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn path(&self) -> &RepositoryPath {
        &self.path
    }

    pub(crate) const fn family(&self) -> ArtifactFamily {
        self.family
    }

    pub(crate) const fn digest(&self) -> Digest {
        self.digest
    }

    pub(crate) fn synthetic_ids(&self) -> &[String] {
        &self.synthetic_ids
    }

    pub(crate) fn observations(&self) -> &[ObservationRegistration] {
        &self.observations
    }
}

impl fmt::Debug for ArtifactRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactRegistration")
            .field("id", &self.id)
            .field("family", &self.family)
            .field("digest", &self.digest)
            .field("synthetic_count", &self.synthetic_ids.len())
            .field("observation_count", &self.observations.len())
            .finish_non_exhaustive()
    }
}

/// Structural failure while constructing reviewed redaction authority.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RegistrationError {
    /// A machine identifier is empty, oversized, or malformed.
    #[error("invalid machine identifier")]
    InvalidMachineId,
    /// A synthetic value is not an inert Norn sentinel.
    #[error("invalid synthetic sentinel")]
    InvalidSyntheticValue,
    /// A registered artifact path is outside its fixed family root.
    #[error("invalid artifact authority path")]
    InvalidArtifactPath,
    /// Synthetic provenance is outside a fixed source root.
    #[error("invalid synthetic provenance path")]
    InvalidProvenancePath,
    /// Authority contains a duplicate path, ID, value, or tuple.
    #[error("duplicate redaction authority")]
    DuplicateAuthority,
    /// Authority rows are not in their required stable order.
    #[error("unstable redaction authority order")]
    UnstableAuthorityOrder,
    /// A family has an invalid synthetic or observation authority shape.
    #[error("invalid authority for artifact family")]
    InvalidFamilyAuthority,
    /// An authority row references an absent artifact or sentinel.
    #[error("unknown redaction authority reference")]
    UnknownAuthorityReference,
    /// An observation tuple disagrees with its referenced artifact.
    #[error("observation tuple does not bind referenced artifact")]
    ObservationBindingMismatch,
    /// A synthetic authority row is not used by any artifact.
    #[error("unused synthetic authority")]
    UnusedSyntheticAuthority,
}
