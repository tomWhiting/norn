use std::collections::BTreeSet;

use serde::Deserialize;

use crate::{Digest, RepositoryPath};

pub(super) const FIXTURE_ROOT: &str = "crates/norn/testdata/openai_responses";
pub(super) const CONTRACT_PINS_PATH: &str =
    "crates/norn/testdata/openai_responses/contract-pins.json";
pub(super) const BACKEND_MATRIX_PATH: &str =
    "crates/norn/testdata/openai_responses/backend-state-matrix.json";
pub(super) const INDEX_PATH: &str = "crates/norn/testdata/openai_responses/index.json";
pub(super) const PUBLIC_MANIFEST_PATH: &str =
    "crates/norn/testdata/openai_responses/public/manifest.json";
pub(super) const CODEX_MANIFEST_PATH: &str =
    "crates/norn/testdata/openai_responses/codex/manifest.json";
pub(super) const PUBLIC_CONTRACT_MANIFEST_PATH: &str =
    "policy/contracts/openai-responses-v1/manifest.json";
pub(super) const PUBLIC_CONTRACT_ROOT: &str = "policy/contracts/openai-responses-v1";
pub(super) const PUBLIC_CONTRACT_MANIFEST_SHA256: &str =
    "b430fa4c864b68c99b8b0dd3fe1e31c60ec68142cc92aa72ca2e1696f956e98d";
pub(super) const CODEX_COMMIT: &str = "0396f99cf1a27fc87dd12d23403b25e840b6ecbd";

pub(super) const CONTROL_PATHS: [&str; 5] = [
    CONTRACT_PINS_PATH,
    BACKEND_MATRIX_PATH,
    INDEX_PATH,
    PUBLIC_MANIFEST_PATH,
    CODEX_MANIFEST_PATH,
];

const CODEX_SOURCES: [(&str, &str); 5] = [
    (
        "codex-rs/core/src/client.rs",
        "f5896595c6fe1ec1b477096e5a41548039f673c7",
    ),
    (
        "codex-rs/codex-api/src/sse/responses.rs",
        "70f96cb855005d577c57fd768062d035cc919b12",
    ),
    (
        "codex-rs/codex-api/src/common.rs",
        "e4600e26aab62a8495248346cd78ab3cb52b7191",
    ),
    (
        "codex-rs/protocol/src/models.rs",
        "91fd42a5558a3836343ffb94ffef3a7f4050b332",
    ),
    (
        "codex-rs/login/src/server.rs",
        "804d05434e231049ffa63709728a5ed8b004e247",
    ),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProtocolEnvelope<T> {
    pub(super) schema_version: u32,
    pub(super) artifact_family: ArtifactFamily,
    pub(super) fixture_id: String,
    pub(super) dialect: Dialect,
    pub(super) artifact_kind: ArtifactKind,
    pub(super) payload: T,
}

impl<T> ProtocolEnvelope<T> {
    pub(super) fn has_identity(
        &self,
        fixture_id: &str,
        dialect: Dialect,
        artifact_kind: ArtifactKind,
    ) -> bool {
        self.schema_version == 1
            && self.artifact_family == ArtifactFamily::ProtocolFixture
            && self.fixture_id == fixture_id
            && self.dialect == dialect
            && self.artifact_kind == artifact_kind
    }
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ArtifactFamily {
    ProtocolFixture,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Dialect {
    Corpus,
    Public,
    Codex,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ArtifactKind {
    BackendStateMatrix,
    ContractPins,
    Index,
    Manifest,
    Request,
    Stream,
    Transport,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContractPins {
    pub(super) public_contract: PublicContractPin,
    pub(super) codex_source: CodexSourceAuthority,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublicContractPin {
    pub(super) manifest: FileReference,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexSourceAuthority {
    pub(super) commit: String,
    pub(super) sources: Vec<CodexSourcePin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexSourcePin {
    path: RepositoryPath,
    blob: String,
}

impl ContractPins {
    pub(super) fn is_ratified(&self) -> bool {
        self.codex_source.commit == CODEX_COMMIT
            && self.codex_source.sources.len() == CODEX_SOURCES.len()
            && self
                .codex_source
                .sources
                .iter()
                .zip(CODEX_SOURCES)
                .all(|(actual, expected)| {
                    actual.path.as_str() == expected.0 && actual.blob == expected.1
                })
            && self.public_contract.manifest.path.as_str() == PUBLIC_CONTRACT_MANIFEST_PATH
            && self.public_contract.manifest.sha256.to_string() == PUBLIC_CONTRACT_MANIFEST_SHA256
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileReference {
    pub(super) path: RepositoryPath,
    pub(super) bytes: u64,
    pub(super) sha256: Digest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BackendMatrix {
    entries: Vec<BackendMatrixEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendMatrixEntry {
    concern: BackendConcern,
    public_contract: String,
    codex_overlay: String,
    p1_treatment: String,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum BackendConcern {
    RequestAuthority,
    StoredContinuation,
    StatelessContinuation,
    AssistantPhase,
    TurnState,
    Completion,
    Metadata,
    Compaction,
    ErrorRetrySemantics,
    CacheReporting,
}

const BACKEND_CONCERNS: [BackendConcern; 10] = [
    BackendConcern::RequestAuthority,
    BackendConcern::StoredContinuation,
    BackendConcern::StatelessContinuation,
    BackendConcern::AssistantPhase,
    BackendConcern::TurnState,
    BackendConcern::Completion,
    BackendConcern::Metadata,
    BackendConcern::Compaction,
    BackendConcern::ErrorRetrySemantics,
    BackendConcern::CacheReporting,
];

impl BackendMatrix {
    pub(super) fn is_complete(&self) -> bool {
        self.entries.len() == BACKEND_CONCERNS.len()
            && self
                .entries
                .iter()
                .zip(BACKEND_CONCERNS)
                .all(|(entry, expected)| {
                    entry.concern == expected
                        && meaningful(&entry.public_contract)
                        && meaningful(&entry.codex_overlay)
                        && meaningful(&entry.p1_treatment)
                })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FixtureIndex {
    pub(super) public_manifest: FileReference,
    pub(super) codex_manifest: FileReference,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DialectManifest {
    pub(super) fixtures: Vec<FixtureRegistration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FixtureRegistration {
    pub(super) id: String,
    pub(super) dialect: Dialect,
    pub(super) artifact_kind: ArtifactKind,
    pub(super) fixture_path: RepositoryPath,
    pub(super) bytes: u64,
    pub(super) sha256: Digest,
    source_references: Vec<String>,
    categories: Vec<String>,
    pub(super) finding_ids: Vec<String>,
    pub(super) owner_phase: OwnerPhase,
    expectation_class: ExpectationClass,
    current_observation: String,
    target_assertions: Vec<String>,
    secret_profile: SecretProfile,
}

impl FixtureRegistration {
    pub(super) fn is_valid_for(&self, dialect: Dialect) -> bool {
        self.dialect == dialect
            && valid_identifier(&self.id)
            && fixture_location(self.fixture_path.as_str(), dialect, self.artifact_kind)
            && nonempty_unique(&self.source_references)
            && self
                .source_references
                .iter()
                .all(|reference| super::sources::is_fixture_source(reference))
            && nonempty_unique(&self.categories)
            && self
                .categories
                .iter()
                .all(|category| valid_category(category))
            && nonempty_unique(&self.finding_ids)
            && meaningful(&self.current_observation)
            && nonempty_meaningful(&self.target_assertions)
            && self.expectation_class.is_valid_for(self.owner_phase)
            && self.secret_profile.permits_registered_synthetic()
    }
}

#[derive(Clone, Copy, Deserialize, Ord, PartialOrd, Eq, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub(super) enum OwnerPhase {
    P0,
    P1,
    P2,
    P3,
    P4,
    P5,
    P6,
    P7,
    P8,
    P9,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExpectationClass {
    SupportedGreen,
    BaselineRed,
    ContractTarget,
    DialectOnly,
}

impl ExpectationClass {
    fn is_valid_for(self, owner: OwnerPhase) -> bool {
        !matches!(self, Self::BaselineRed) || owner > OwnerPhase::P1
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SecretProfile {
    None,
    RegisteredSynthetic,
}

impl SecretProfile {
    const fn permits_registered_synthetic(self) -> bool {
        matches!(self, Self::RegisteredSynthetic)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublicExtractionManifest {
    api_description_version: String,
    extractor_version: String,
    kind: String,
    openapi_version: String,
    pub(super) outputs: Vec<FileReference>,
    retrieved_on: String,
    schema_version: u32,
    sources: Vec<PublicSource>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicSource {
    arguments: SourceArguments,
    normalized_bytes: u64,
    normalized_sha256: Digest,
    raw_bytes: u64,
    raw_sha256: Digest,
    source_id: String,
    tool: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceArguments {
    url: String,
}

impl PublicExtractionManifest {
    pub(super) fn is_valid(&self) -> bool {
        self.schema_version == 1
            && self.kind == "public_responses_contract_manifest"
            && self.extractor_version == "norn-openai-responses-contract-v1"
            && meaningful(&self.api_description_version)
            && meaningful(&self.openapi_version)
            && valid_date(&self.retrieved_on)
            && !self.outputs.is_empty()
            && strictly_increasing_paths(&self.outputs)
            && self.outputs.iter().all(|output| {
                output
                    .path
                    .as_str()
                    .strip_prefix(PUBLIC_CONTRACT_ROOT)
                    .is_some_and(|suffix| suffix.starts_with('/'))
                    && output.path.as_str() != PUBLIC_CONTRACT_MANIFEST_PATH
            })
            && !self.sources.is_empty()
            && unique_public_sources(&self.sources)
            && self.sources.iter().all(PublicSource::is_valid)
    }
}

impl PublicSource {
    fn is_valid(&self) -> bool {
        valid_identifier(&self.source_id)
            && meaningful(&self.tool)
            && super::sources::is_public_extraction_source(&self.arguments.url)
            && self.raw_bytes != 0
            && self.normalized_bytes != 0
            && self.raw_sha256 == self.normalized_sha256
    }
}

fn fixture_location(path: &str, dialect: Dialect, kind: ArtifactKind) -> bool {
    let (prefix, extension) = match (dialect, kind) {
        (Dialect::Public, ArtifactKind::Request) => ("public/requests/", ".json"),
        (Dialect::Public, ArtifactKind::Stream) => ("public/streams/", ".sse"),
        (Dialect::Codex, ArtifactKind::Request) => ("codex/requests/", ".json"),
        (Dialect::Codex, ArtifactKind::Stream) => ("codex/streams/", ".sse"),
        (Dialect::Codex, ArtifactKind::Transport) => ("codex/transport/", ".json"),
        _ => return false,
    };
    path.strip_prefix(FIXTURE_ROOT)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .is_some_and(|suffix| suffix.starts_with(prefix) && suffix.ends_with(extension))
}

fn meaningful(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn valid_category(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'/' | b'-' | b'_')
        })
}

fn nonempty_unique(values: &[String]) -> bool {
    !values.is_empty() && unique_meaningful(values)
}

fn nonempty_meaningful(values: &[String]) -> bool {
    !values.is_empty() && values.iter().all(|value| meaningful(value))
}

fn unique_meaningful(values: &[String]) -> bool {
    values.iter().all(|value| meaningful(value))
        && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

fn strictly_increasing_paths(values: &[FileReference]) -> bool {
    values.windows(2).all(|pair| pair[0].path < pair[1].path)
}

fn unique_public_sources(values: &[PublicSource]) -> bool {
    values
        .iter()
        .map(|source| source.source_id.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        == values.len()
}

fn valid_date(value: &str) -> bool {
    value.len() == 10
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}
