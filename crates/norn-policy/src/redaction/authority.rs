use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::digest::{Digest, digest_bytes};
use crate::path::RepositoryPath;
use crate::snapshot::OwnedSnapshot;

use super::model::{
    ArtifactFamily, ArtifactRegistration, ObservationSource, PublicUrl, RegistrationError,
    SentinelClass, SyntheticPurpose, SyntheticRegistration,
};
use super::path_policy::{
    GOVERNED_CONTRACT_PATHS, GOVERNED_EVIDENCE_TOOL_PATHS, GOVERNED_ROOTS, GOVERNED_SCRIPT_PATHS,
    is_evidence_tool_candidate,
};
use super::{contract, traceability};

const SCHEMA_DOMAIN: &[u8] = b"norn.redaction.schema.v1\0";
const REGISTRY_DOMAIN: &[u8] = b"norn.redaction.registry.v1\0";

/// Complete reviewed authority for the fixed P1 retained-artifact roots.
#[derive(Clone, Eq, PartialEq)]
pub struct RedactionRegistry {
    artifacts: BTreeMap<RepositoryPath, ArtifactRegistration>,
    synthetics: BTreeMap<String, SyntheticRegistration>,
    values: BTreeMap<String, String>,
    digest: Digest,
}

impl RedactionRegistry {
    /// Derive deterministic checked-tree P1 authority from one complete snapshot.
    ///
    /// Corpus sentinel metadata uses the fixed checked generator convention and
    /// provenance path. This authoring step does not execute that generator;
    /// the caller must separately establish generator reproduction evidence.
    ///
    /// Promoted gate evidence is derived from the same checked schema and
    /// tuple rules as ignored target evidence. Unpromoted target runs remain
    /// deliberately outside this authoring path.
    ///
    /// # Errors
    ///
    /// Rejects non-regular or unclassified governed entries, malformed protocol
    /// fixtures, target-local evidence, empty authority, and invalid registrations.
    pub fn author_checked_tree_p1(
        snapshot: &OwnedSnapshot,
    ) -> Result<Self, super::RedactionAuthoringError> {
        super::authoring::author_checked_tree(snapshot)
    }

    /// Derive one ephemeral authority for an exact local-gate evidence snapshot.
    ///
    /// This authority is deliberately separate from the phase-lock-pinned
    /// checked-tree registry. The checked registry must validate the complete
    /// checked snapshot before it can authorize one exact ignored gate run.
    ///
    /// # Errors
    ///
    /// Rejects invalid checked authority, mixed run roots, malformed gate
    /// evidence, unsafe content, and registration drift.
    pub fn author_run_local_p1(
        checked_registry: &Self,
        checked_snapshot: &OwnedSnapshot,
        run_snapshot: &OwnedSnapshot,
    ) -> Result<Self, super::RedactionAuthoringError> {
        super::run_local::author(checked_registry, checked_snapshot, run_snapshot)
    }

    /// Decode the strict checked-in P1 registry document.
    ///
    /// # Errors
    ///
    /// Rejects duplicate or unknown JSON fields, unsupported schema versions,
    /// and any invalid, unordered, duplicated, or inconsistent registration.
    pub fn decode_p1(bytes: &[u8]) -> Result<Self, super::RegistryDocumentError> {
        super::registry_document::decode(bytes)
    }

    /// Encode the deterministic checked-in P1 registry as pretty JSON.
    ///
    /// The result contains exactly one trailing newline.
    ///
    /// # Errors
    ///
    /// Returns an error if the closed registry cannot be represented as JSON.
    pub fn encode_p1(&self) -> Result<Vec<u8>, super::RegistryEncodeError> {
        super::registry_document::encode(self)
    }

    /// Validate and construct deterministic closed authority.
    pub fn new(
        artifacts: Vec<ArtifactRegistration>,
        synthetics: Vec<SyntheticRegistration>,
    ) -> Result<Self, RegistrationError> {
        if !artifacts
            .windows(2)
            .all(|pair| pair[0].path() < pair[1].path())
            || !synthetics
                .windows(2)
                .all(|pair| pair[0].id() < pair[1].id())
        {
            return Err(RegistrationError::UnstableAuthorityOrder);
        }

        let mut artifact_ids = BTreeSet::new();
        let mut artifact_map = BTreeMap::new();
        for artifact in artifacts {
            if !artifact_ids.insert(artifact.id().to_owned())
                || artifact_map
                    .insert(artifact.path().clone(), artifact)
                    .is_some()
            {
                return Err(RegistrationError::DuplicateAuthority);
            }
        }

        let mut synthetic_map = BTreeMap::new();
        let mut value_map = BTreeMap::new();
        for synthetic in synthetics {
            if value_map
                .insert(synthetic.value().to_owned(), synthetic.id().to_owned())
                .is_some()
                || synthetic_map
                    .insert(synthetic.id().to_owned(), synthetic)
                    .is_some()
            {
                return Err(RegistrationError::DuplicateAuthority);
            }
        }

        validate_references(&artifact_map, &synthetic_map)?;
        let digest = registry_digest(&artifact_map, &synthetic_map);
        Ok(Self {
            artifacts: artifact_map,
            synthetics: synthetic_map,
            values: value_map,
            digest,
        })
    }

    /// Return the normalized authority digest for phase-lock pinning.
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Return the normalized fixed-schema digest for phase-lock pinning.
    #[must_use]
    pub fn schema_digest() -> Digest {
        redaction_schema_digest()
    }

    pub(crate) fn artifacts(
        &self,
    ) -> impl ExactSizeIterator<Item = (&RepositoryPath, &ArtifactRegistration)> {
        self.artifacts.iter()
    }

    pub(crate) fn synthetics(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &SyntheticRegistration)> {
        self.synthetics
            .iter()
            .map(|(id, registration)| (id.as_str(), registration))
    }

    pub(crate) fn is_document_nonempty(&self) -> bool {
        !self.artifacts.is_empty() && !self.synthetics.is_empty()
    }

    /// Iterate over registered artifact paths in canonical order.
    pub fn registered_paths(&self) -> impl ExactSizeIterator<Item = &RepositoryPath> {
        self.artifacts.keys()
    }

    pub(crate) fn artifact_with_ordinal(
        &self,
        path: &RepositoryPath,
    ) -> Option<(u64, &ArtifactRegistration)> {
        self.artifacts
            .iter()
            .zip(0_u64..)
            .find_map(|((candidate, artifact), ordinal)| {
                (candidate == path).then_some((ordinal, artifact))
            })
    }

    pub(crate) fn synthetic(&self, id: &str) -> Option<&SyntheticRegistration> {
        self.synthetics.get(id)
    }

    pub(crate) fn synthetic_for_value(&self, value: &str) -> Option<&SyntheticRegistration> {
        let id = self.values.get(value)?;
        self.synthetics.get(id)
    }

    pub(crate) fn combined(&self, other: &Self) -> Result<Self, RegistrationError> {
        let mut artifacts = self
            .artifacts
            .values()
            .chain(other.artifacts.values())
            .cloned()
            .collect::<Vec<_>>();
        artifacts.sort_by(|left, right| left.path().cmp(right.path()));
        let mut synthetics = self
            .synthetics
            .values()
            .chain(other.synthetics.values())
            .cloned()
            .collect::<Vec<_>>();
        synthetics.sort_by(|left, right| left.id().cmp(right.id()));
        Self::new(artifacts, synthetics)
    }
}

impl fmt::Debug for RedactionRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactionRegistry")
            .field("artifact_count", &self.artifacts.len())
            .field("synthetic_count", &self.synthetics.len())
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

/// Return the normalized digest of fixed roots, families, and grammar version.
#[must_use]
pub fn redaction_schema_digest() -> Digest {
    let mut bytes = Vec::from(SCHEMA_DOMAIN);
    for root in GOVERNED_ROOTS {
        push_field(&mut bytes, root);
    }
    for path in GOVERNED_SCRIPT_PATHS {
        push_field(&mut bytes, path);
    }
    for path in GOVERNED_EVIDENCE_TOOL_PATHS {
        push_field(&mut bytes, path);
    }
    for path in GOVERNED_CONTRACT_PATHS {
        push_field(&mut bytes, path);
    }
    for family in [
        ArtifactFamily::ProtocolFixture,
        ArtifactFamily::TraceabilityJsonl,
        ArtifactFamily::ContractSchema,
        ArtifactFamily::GateDescriptor,
        ArtifactFamily::Distribution,
        ArtifactFamily::SanitizedLog,
        ArtifactFamily::EvidenceToolSource,
    ] {
        push_field(&mut bytes, family.as_str());
        push_field(&mut bytes, "1");
    }
    push_field(&mut bytes, "machine-grammar-v1");
    push_field(&mut bytes, "danger-scan-v1");
    push_field(&mut bytes, "protocol-envelope-v2");
    push_field(&mut bytes, "protocol-context-grammar-v1");
    push_field(&mut bytes, "protocol-literals-from-contract-pins-v1");
    push_field(&mut bytes, "json-schema-grammar-v1");
    push_field(&mut bytes, "reviewed-source-exception-v1");
    push_field(&mut bytes, "sanitized-log-grammar-v1");
    push_field(&mut bytes, "gate-run-schema-binding-v1");
    push_field(&mut bytes, "gate-run-structured-summary-v1");
    push_field(&mut bytes, "run-local-checked-authority-v1");
    push_field(&mut bytes, "evidence-tool-inventory-join-v1");
    push_field(&mut bytes, "contract-singleton-authority-v1");
    for (path, digest) in contract::authorities() {
        push_field(&mut bytes, path);
        push_field(&mut bytes, digest);
    }
    let (traceability_path, traceability_digest) = traceability::authority();
    push_field(&mut bytes, "traceability-registry-v1");
    push_field(&mut bytes, traceability_path);
    push_field(&mut bytes, traceability_digest);
    for url in [
        PublicUrl::OpenAiResponsesEndpoint,
        PublicUrl::OpenAiCompactEndpoint,
        PublicUrl::OpenAiStreamingEvents,
        PublicUrl::OpenAiWebsocketEvents,
    ] {
        push_field(&mut bytes, url.as_str());
    }
    for source in [
        ObservationSource::CodexSourcePin,
        ObservationSource::LocalGate,
        ObservationSource::PublicUrl(PublicUrl::OpenAiResponsesEndpoint),
        ObservationSource::PublicUrl(PublicUrl::OpenAiCompactEndpoint),
        ObservationSource::PublicUrl(PublicUrl::OpenAiStreamingEvents),
        ObservationSource::PublicUrl(PublicUrl::OpenAiWebsocketEvents),
    ] {
        push_field(&mut bytes, source.digest_name());
    }
    for purpose in [
        SyntheticPurpose::Generic,
        SyntheticPurpose::AccountId,
        SyntheticPurpose::Credential,
        SyntheticPurpose::PromptContent,
        SyntheticPurpose::TurnState,
        SyntheticPurpose::CacheKey,
    ] {
        push_field(&mut bytes, purpose_name(purpose));
    }
    for root in [
        "docs/reviews/evidence/p1/gate/descriptors",
        "docs/reviews/evidence/p1/gate/distributions",
        "docs/reviews/evidence/p1/gate/logs",
    ] {
        push_field(&mut bytes, root);
    }
    digest_bytes(&bytes)
}

pub(crate) fn is_governed_path(path: &RepositoryPath) -> bool {
    GOVERNED_ROOTS
        .iter()
        .any(|root| path.as_str() == *root || path_is_beneath(path.as_str(), root))
        || GOVERNED_SCRIPT_PATHS.contains(&path.as_str())
        || GOVERNED_CONTRACT_PATHS.contains(&path.as_str())
        || is_evidence_tool_candidate(path)
}

fn validate_references(
    artifacts: &BTreeMap<RepositoryPath, ArtifactRegistration>,
    synthetics: &BTreeMap<String, SyntheticRegistration>,
) -> Result<(), RegistrationError> {
    let mut used_synthetics = BTreeSet::new();
    for artifact in artifacts.values() {
        for id in artifact.synthetic_ids() {
            if !synthetics.contains_key(id) {
                return Err(RegistrationError::UnknownAuthorityReference);
            }
            used_synthetics.insert(id.as_str());
        }
        for observation in artifact.observations() {
            let Some(reference) = artifacts.get(observation.referenced_path()) else {
                return Err(RegistrationError::UnknownAuthorityReference);
            };
            if observation.referenced_path() == artifact.path()
                || reference.family() != observation.referenced_family()
                || reference.digest() != observation.digest()
            {
                return Err(RegistrationError::ObservationBindingMismatch);
            }
            for id in observation.synthetic_ids() {
                if !artifact.synthetic_ids().contains(id) || !synthetics.contains_key(id) {
                    return Err(RegistrationError::UnknownAuthorityReference);
                }
            }
        }
    }
    if synthetics
        .keys()
        .any(|id| !used_synthetics.contains(id.as_str()))
    {
        return Err(RegistrationError::UnusedSyntheticAuthority);
    }
    Ok(())
}

fn registry_digest(
    artifacts: &BTreeMap<RepositoryPath, ArtifactRegistration>,
    synthetics: &BTreeMap<String, SyntheticRegistration>,
) -> Digest {
    let mut bytes = Vec::from(REGISTRY_DOMAIN);
    bytes.extend_from_slice(redaction_schema_digest().as_bytes());
    for artifact in artifacts.values() {
        push_field(&mut bytes, artifact.id());
        push_field(&mut bytes, artifact.path().as_str());
        push_field(&mut bytes, artifact.family().as_str());
        bytes.extend_from_slice(artifact.digest().as_bytes());
        for id in artifact.synthetic_ids() {
            push_field(&mut bytes, id);
        }
        bytes.push(0xff);
        for observation in artifact.observations() {
            push_field(&mut bytes, observation.id());
            push_field(&mut bytes, observation.referenced_path().as_str());
            push_field(&mut bytes, observation.referenced_family().as_str());
            push_field(&mut bytes, observation.source().digest_name());
            bytes.extend_from_slice(observation.digest().as_bytes());
            for id in observation.synthetic_ids() {
                push_field(&mut bytes, id);
            }
            bytes.push(0xfe);
        }
        bytes.push(0xfd);
    }
    for synthetic in synthetics.values() {
        push_field(&mut bytes, synthetic.id());
        push_field(&mut bytes, synthetic.value());
        push_field(&mut bytes, synthetic.generator());
        push_field(&mut bytes, synthetic.provenance().as_str());
        push_field(&mut bytes, purpose_name(synthetic.purpose()));
        push_field(
            &mut bytes,
            match synthetic.sentinel_class() {
                SentinelClass::NonReusableFixtureV1 => "non_reusable_fixture_v1",
            },
        );
        bytes.push(0xfc);
    }
    digest_bytes(&bytes)
}

fn purpose_name(purpose: SyntheticPurpose) -> &'static str {
    match purpose {
        SyntheticPurpose::Generic => "generic",
        SyntheticPurpose::AccountId => "account_id",
        SyntheticPurpose::Credential => "credential",
        SyntheticPurpose::PromptContent => "prompt_content",
        SyntheticPurpose::TurnState => "turn_state",
        SyntheticPurpose::CacheKey => "cache_key",
    }
}

fn path_is_beneath(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|suffix| suffix.starts_with('/') && suffix.len() > 1)
}

fn push_field(output: &mut Vec<u8>, value: &str) {
    output.extend_from_slice(value.as_bytes());
    output.push(0);
}
