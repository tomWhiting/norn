use std::collections::{BTreeMap, BTreeSet};

use serde::de::DeserializeOwned;

use crate::strict_json::{StrictJsonError, decode_strict_json};
use crate::{Digest, EntryKind, OwnedSnapshot, RepositoryPath, digest_bytes};

use super::error::ResponsesContractError;
use super::fixture::fixture_matches;
use super::identity::authority_identity;
use super::model::{
    ArtifactKind, BACKEND_MATRIX_PATH, BackendMatrix, CODEX_MANIFEST_PATH, CONTRACT_PINS_PATH,
    CONTROL_PATHS, ContractPins, Dialect, DialectManifest, FIXTURE_ROOT, FixtureIndex,
    FixtureRegistration, INDEX_PATH, PUBLIC_CONTRACT_MANIFEST_PATH,
    PUBLIC_CONTRACT_MANIFEST_SHA256, PUBLIC_MANIFEST_PATH, ProtocolEnvelope,
    PublicExtractionManifest,
};
use super::traceability::{TRACEABILITY_PATH, TraceabilityAgreementError, TraceabilityRegistry};

const CONTRACT_PINS_ID: &str = "openai-responses-contract-pins-v1";
const BACKEND_MATRIX_ID: &str = "openai-responses-backend-state-matrix-v1";
const INDEX_ID: &str = "openai-responses-index-v1";
const PUBLIC_MANIFEST_ID: &str = "openai-responses-public-manifest-v1";
const CODEX_MANIFEST_ID: &str = "openai-responses-codex-manifest-v1";

/// Opaque proof that the complete Responses contract corpus was acquired from
/// one immutable repository snapshot and matched every pinned authority.
pub struct ResponsesContractAuthority {
    digest: Digest,
    governed_file_count: usize,
    public_fixture_count: usize,
    codex_fixture_count: usize,
}

impl ResponsesContractAuthority {
    /// Acquire and validate every fixed authority, extraction output, dialect
    /// manifest, and transitively declared fixture from one owned snapshot.
    ///
    /// # Errors
    ///
    /// Returns a closed error for a missing or non-regular entry, ambiguous or
    /// out-of-contract JSON, authority drift, fixture mismatch, or an undeclared
    /// entry beneath the governed fixture root.
    pub fn acquire(snapshot: &OwnedSnapshot) -> Result<Self, ResponsesContractError> {
        let mut files = BTreeMap::new();
        let public_contract_bytes =
            bind_fixed(snapshot, PUBLIC_CONTRACT_MANIFEST_PATH, &mut files)?;
        if digest_bytes(public_contract_bytes).to_string() != PUBLIC_CONTRACT_MANIFEST_SHA256 {
            return Err(ResponsesContractError::DigestMismatch);
        }
        let public_contract: PublicExtractionManifest =
            decode(PUBLIC_CONTRACT_MANIFEST_PATH, public_contract_bytes)?;
        if !public_contract.is_valid() {
            return schema_failure(PUBLIC_CONTRACT_MANIFEST_PATH);
        }
        for output in &public_contract.outputs {
            let bytes = bind_reference(snapshot, output, &mut files)?;
            let value: serde_json::Value = decode(output.path.as_str(), bytes)?;
            if !value.is_object() {
                return schema_failure(output.path.as_str());
            }
        }

        let pins = decode_control::<ContractPins>(
            snapshot,
            CONTRACT_PINS_PATH,
            CONTRACT_PINS_ID,
            Dialect::Corpus,
            ArtifactKind::ContractPins,
            &mut files,
        )?;
        if !pins.is_ratified() {
            return schema_failure(CONTRACT_PINS_PATH);
        }
        verify_declared_reference(
            &pins.public_contract.manifest,
            PUBLIC_CONTRACT_MANIFEST_PATH,
            CONTRACT_PINS_PATH,
            public_contract_bytes,
        )?;

        let matrix = decode_control::<BackendMatrix>(
            snapshot,
            BACKEND_MATRIX_PATH,
            BACKEND_MATRIX_ID,
            Dialect::Corpus,
            ArtifactKind::BackendStateMatrix,
            &mut files,
        )?;
        if !matrix.is_complete() {
            return schema_failure(BACKEND_MATRIX_PATH);
        }

        let public_manifest_bytes = bind_fixed(snapshot, PUBLIC_MANIFEST_PATH, &mut files)?;
        let public_manifest = decode_envelope::<DialectManifest>(
            PUBLIC_MANIFEST_PATH,
            public_manifest_bytes,
            PUBLIC_MANIFEST_ID,
            Dialect::Public,
            ArtifactKind::Manifest,
        )?;
        let codex_manifest_bytes = bind_fixed(snapshot, CODEX_MANIFEST_PATH, &mut files)?;
        let codex_manifest = decode_envelope::<DialectManifest>(
            CODEX_MANIFEST_PATH,
            codex_manifest_bytes,
            CODEX_MANIFEST_ID,
            Dialect::Codex,
            ArtifactKind::Manifest,
        )?;

        let index = decode_control::<FixtureIndex>(
            snapshot,
            INDEX_PATH,
            INDEX_ID,
            Dialect::Corpus,
            ArtifactKind::Index,
            &mut files,
        )?;
        verify_declared_reference(
            &index.public_manifest,
            PUBLIC_MANIFEST_PATH,
            INDEX_PATH,
            public_manifest_bytes,
        )?;
        verify_declared_reference(
            &index.codex_manifest,
            CODEX_MANIFEST_PATH,
            INDEX_PATH,
            codex_manifest_bytes,
        )?;

        let traceability_bytes = bind_fixed(snapshot, TRACEABILITY_PATH, &mut files)?;
        fixed_path(TRACEABILITY_PATH)?;
        let traceability = match TraceabilityRegistry::acquire(traceability_bytes) {
            Ok(traceability) => traceability,
            Err(super::traceability::TraceabilityError::Utf8(source)) => {
                return Err(ResponsesContractError::TraceabilityUtf8 { source });
            }
            Err(super::traceability::TraceabilityError::Json) => {
                return Err(ResponsesContractError::TraceabilityJson);
            }
            Err(super::traceability::TraceabilityError::Schema) => {
                return Err(ResponsesContractError::TraceabilitySchema);
            }
        };

        let mut fixture_ids = BTreeSet::new();
        let mut fixture_paths = BTreeSet::new();
        bind_fixtures(
            snapshot,
            &public_manifest.fixtures,
            Dialect::Public,
            &mut files,
            &mut fixture_ids,
            &mut fixture_paths,
        )?;
        bind_fixtures(
            snapshot,
            &codex_manifest.fixtures,
            Dialect::Codex,
            &mut files,
            &mut fixture_ids,
            &mut fixture_paths,
        )?;
        match traceability.verify_fixtures(
            public_manifest
                .fixtures
                .iter()
                .chain(codex_manifest.fixtures.iter()),
        ) {
            Ok(()) => {}
            Err(TraceabilityAgreementError::Mismatch { issue, count }) => {
                return Err(ResponsesContractError::EvidenceTraceability { issue, count });
            }
            Err(TraceabilityAgreementError::Cardinality) => {
                return Err(ResponsesContractError::TraceabilitySchema);
            }
        }
        reject_undeclared_entries(snapshot, &files)?;

        Ok(Self {
            digest: authority_identity(&files),
            governed_file_count: files.len(),
            public_fixture_count: public_manifest.fixtures.len(),
            codex_fixture_count: codex_manifest.fixtures.len(),
        })
    }

    /// Return the domain-separated aggregate identity of every governed byte.
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Return the exact number of fixed, extracted, and fixture files bound.
    #[must_use]
    pub const fn governed_file_count(&self) -> usize {
        self.governed_file_count
    }

    /// Return the number of fixtures declared by the public manifest.
    #[must_use]
    pub const fn public_fixture_count(&self) -> usize {
        self.public_fixture_count
    }

    /// Return the number of fixtures declared by the Codex manifest.
    #[must_use]
    pub const fn codex_fixture_count(&self) -> usize {
        self.codex_fixture_count
    }
}

fn decode_control<'a, T>(
    snapshot: &'a OwnedSnapshot,
    path: &str,
    fixture_id: &str,
    dialect: Dialect,
    kind: ArtifactKind,
    files: &mut BTreeMap<RepositoryPath, &'a [u8]>,
) -> Result<T, ResponsesContractError>
where
    T: DeserializeOwned,
{
    let bytes = bind_fixed(snapshot, path, files)?;
    decode_envelope(path, bytes, fixture_id, dialect, kind)
}

fn decode_envelope<T>(
    path: &str,
    bytes: &[u8],
    fixture_id: &str,
    dialect: Dialect,
    kind: ArtifactKind,
) -> Result<T, ResponsesContractError>
where
    T: DeserializeOwned,
{
    let envelope: ProtocolEnvelope<T> = decode(path, bytes)?;
    if !envelope.has_identity(fixture_id, dialect, kind) {
        return schema_failure(path);
    }
    Ok(envelope.payload)
}

fn decode<T>(path: &str, bytes: &[u8]) -> Result<T, ResponsesContractError>
where
    T: DeserializeOwned,
{
    fixed_path(path)?;
    match decode_strict_json(bytes) {
        Ok(value) => Ok(value),
        Err(StrictJsonError::Document { .. } | StrictJsonError::Schema { .. }) => {
            Err(ResponsesContractError::Json)
        }
    }
}

fn bind_fixtures<'a>(
    snapshot: &'a OwnedSnapshot,
    registrations: &[FixtureRegistration],
    dialect: Dialect,
    files: &mut BTreeMap<RepositoryPath, &'a [u8]>,
    fixture_ids: &mut BTreeSet<String>,
    fixture_paths: &mut BTreeSet<RepositoryPath>,
) -> Result<(), ResponsesContractError> {
    if registrations.is_empty()
        || registrations
            .windows(2)
            .any(|pair| pair[0].id >= pair[1].id)
    {
        return schema_failure(match dialect {
            Dialect::Public => PUBLIC_MANIFEST_PATH,
            Dialect::Codex => CODEX_MANIFEST_PATH,
            Dialect::Corpus => INDEX_PATH,
        });
    }
    for registration in registrations {
        if !registration.is_valid_for(dialect) {
            return Err(ResponsesContractError::FixtureSchema);
        }
        if !fixture_ids.insert(registration.id.clone()) {
            return Err(ResponsesContractError::DuplicateFixtureId);
        }
        if !fixture_paths.insert(registration.fixture_path.clone()) {
            return Err(ResponsesContractError::DuplicateFixturePath);
        }
        let bytes = bind_reference(snapshot, registration, files)?;
        if !fixture_matches(registration, bytes) {
            return Err(ResponsesContractError::FixtureSchema);
        }
    }
    Ok(())
}

trait PinnedFile {
    fn path(&self) -> &RepositoryPath;
    fn byte_length(&self) -> u64;
    fn sha256(&self) -> Digest;
}

impl PinnedFile for super::model::FileReference {
    fn path(&self) -> &RepositoryPath {
        &self.path
    }

    fn byte_length(&self) -> u64 {
        self.bytes
    }

    fn sha256(&self) -> Digest {
        self.sha256
    }
}

impl PinnedFile for FixtureRegistration {
    fn path(&self) -> &RepositoryPath {
        &self.fixture_path
    }

    fn byte_length(&self) -> u64 {
        self.bytes
    }

    fn sha256(&self) -> Digest {
        self.sha256
    }
}

fn bind_reference<'a, T>(
    snapshot: &'a OwnedSnapshot,
    reference: &T,
    files: &mut BTreeMap<RepositoryPath, &'a [u8]>,
) -> Result<&'a [u8], ResponsesContractError>
where
    T: PinnedFile,
{
    let path = reference.path();
    if files.contains_key(path) {
        return Err(ResponsesContractError::DuplicateAuthorityPath);
    }
    let bytes = regular_bytes(snapshot, path)?;
    verify_reference(reference, bytes)?;
    files.insert(path.clone(), bytes);
    Ok(bytes)
}

fn verify_reference<T>(reference: &T, bytes: &[u8]) -> Result<(), ResponsesContractError>
where
    T: PinnedFile,
{
    let actual_length = match u64::try_from(bytes.len()) {
        Ok(length) => length,
        Err(source) => {
            return Err(ResponsesContractError::LengthOverflow { source });
        }
    };
    if actual_length != reference.byte_length() {
        return Err(ResponsesContractError::LengthMismatch);
    }
    if digest_bytes(bytes) != reference.sha256() {
        return Err(ResponsesContractError::DigestMismatch);
    }
    Ok(())
}

fn verify_declared_reference<T>(
    reference: &T,
    expected_path: &str,
    declaring_path: &str,
    bytes: &[u8],
) -> Result<(), ResponsesContractError>
where
    T: PinnedFile,
{
    if reference.path().as_str() != expected_path {
        return schema_failure(declaring_path);
    }
    verify_reference(reference, bytes)
}

fn bind_fixed<'a>(
    snapshot: &'a OwnedSnapshot,
    value: &str,
    files: &mut BTreeMap<RepositoryPath, &'a [u8]>,
) -> Result<&'a [u8], ResponsesContractError> {
    let path = fixed_path(value)?;
    let bytes = regular_bytes(snapshot, &path)?;
    if files.insert(path.clone(), bytes).is_some() {
        return Err(ResponsesContractError::DuplicateAuthorityPath);
    }
    Ok(bytes)
}

fn regular_bytes<'a>(
    snapshot: &'a OwnedSnapshot,
    path: &RepositoryPath,
) -> Result<&'a [u8], ResponsesContractError> {
    let Some(entry) = snapshot.get(path) else {
        return Err(ResponsesContractError::Missing);
    };
    if entry.kind() != EntryKind::Regular {
        return Err(ResponsesContractError::NotRegular { kind: entry.kind() });
    }
    Ok(entry.bytes())
}

fn reject_undeclared_entries(
    snapshot: &OwnedSnapshot,
    files: &BTreeMap<RepositoryPath, &[u8]>,
) -> Result<(), ResponsesContractError> {
    for (path, _) in snapshot.iter() {
        if beneath_fixture_root(path) && !files.contains_key(path) {
            return Err(ResponsesContractError::UndeclaredFixture);
        }
        if beneath_root(path, super::model::PUBLIC_CONTRACT_ROOT) && !files.contains_key(path) {
            return Err(ResponsesContractError::UndeclaredPublicContract);
        }
    }
    for control in CONTROL_PATHS {
        let path = fixed_path(control)?;
        if !files.contains_key(&path) {
            return Err(ResponsesContractError::Missing);
        }
    }
    Ok(())
}

fn beneath_fixture_root(path: &RepositoryPath) -> bool {
    beneath_root(path, FIXTURE_ROOT)
}

fn beneath_root(path: &RepositoryPath, root: &str) -> bool {
    path.as_str() == root
        || path
            .as_str()
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn fixed_path(value: &str) -> Result<RepositoryPath, ResponsesContractError> {
    RepositoryPath::parse(value).map_err(ResponsesContractError::FixedPath)
}

fn schema_failure<T>(value: &str) -> Result<T, ResponsesContractError> {
    fixed_path(value)?;
    Err(ResponsesContractError::Schema)
}
