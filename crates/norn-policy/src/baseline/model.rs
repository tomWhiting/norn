//! Validated immutable origin fact types.

use serde::{Deserialize, Serialize};

use crate::digest::{Digest, digest_bytes};
use crate::facts::SourceInventoryEntry;
use crate::path::RepositoryPath;
use crate::phase_lock::GitObjectId;
use crate::rust::modules::CompileTestFixtureFact;
use crate::writers::{OperationKind, SinkDiscovery, WriterOperation, WriterRole, WriterToken};

use super::items::ItemGroupFact;
use super::production::ProductionFileFact;

/// Closed schema for the first immutable origin ledger.
pub const ORIGIN_SCHEMA_VERSION: u32 = 1;

/// Exact commit accepted as the P1 origin authority.
pub const P1_BASE_COMMIT: &str = "2917c8ed10e7a2ec7ac9c4d7283bafbea7f6577d";

/// Exact tree accepted as the P1 origin authority.
pub const P1_BASE_TREE: &str = "9ae969792c53b4e1dfdc61c6d91f7fe62d3ac582";

/// Domain-separated identity for one immutable origin fact.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct OriginId(Digest);

impl OriginId {
    pub(crate) const fn new(digest: Digest) -> Self {
        Self(digest)
    }

    /// Return the complete origin digest.
    #[must_use]
    pub const fn digest(self) -> Digest {
        self.0
    }
}

/// One member of the prohibited-debt origin multiset.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DebtOriginFact {
    origin_id: OriginId,
    path: RepositoryPath,
    fingerprint: Digest,
    ordinal: u32,
}

impl DebtOriginFact {
    /// Construct one collision-preserving multiset member.
    #[must_use]
    pub fn new(path: RepositoryPath, fingerprint: Digest, ordinal: u32) -> Self {
        Self {
            origin_id: debt_origin_id(&path, fingerprint, ordinal),
            path,
            fingerprint,
            ordinal,
        }
    }

    /// Return the domain-separated immutable origin identity.
    #[must_use]
    pub const fn origin_id(&self) -> OriginId {
        self.origin_id
    }

    /// Return the source file containing the occurrence.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the complete analyzer fingerprint.
    #[must_use]
    pub const fn fingerprint(&self) -> Digest {
        self.fingerprint
    }

    /// Return the collision-preserving multiset ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
}

impl From<&crate::debt::DebtOccurrence> for DebtOriginFact {
    fn from(value: &crate::debt::DebtOccurrence) -> Self {
        Self::new(value.path().clone(), value.fingerprint(), value.ordinal())
    }
}

/// One immutable generated writer-operation inventory row.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct WriterOperationFact {
    origin_id: OriginId,
    operation_id: Digest,
    path: RepositoryPath,
    span_start: u64,
    span_end: u64,
    enclosing_item: Digest,
    normalized_call: Digest,
    sink: WriterToken,
    operation_kind: OperationKind,
    role: WriterRole,
    discovery: SinkDiscovery,
    ordinal: u32,
}

pub(super) struct WriterOperationInput {
    pub(super) operation_id: Digest,
    pub(super) path: RepositoryPath,
    pub(super) span_start: u64,
    pub(super) span_end: u64,
    pub(super) enclosing_item: Digest,
    pub(super) normalized_call: Digest,
    pub(super) sink: WriterToken,
    pub(super) operation_kind: OperationKind,
    pub(super) role: WriterRole,
    pub(super) discovery: SinkDiscovery,
    pub(super) ordinal: u32,
}

impl WriterOperationFact {
    /// Convert one canonical resolved writer operation without losing fields.
    #[must_use]
    pub fn from_canonical(value: &WriterOperation) -> Self {
        let span = value.span();
        Self::build(WriterOperationInput {
            operation_id: value.id().digest(),
            path: value.path().clone(),
            span_start: span.start(),
            span_end: span.end(),
            enclosing_item: value.enclosing_item(),
            normalized_call: value.normalized_call(),
            sink: value.sink().clone(),
            operation_kind: value.kind(),
            role: value.role(),
            discovery: value.discovery(),
            ordinal: value.ordinal(),
        })
    }

    /// Reconstruct one serialized writer row for strict identity validation.
    ///
    /// # Errors
    ///
    /// Returns `WriterSpanError` when the half-open span is empty or reversed.
    pub(super) fn from_decoded(input: WriterOperationInput) -> Result<Self, WriterSpanError> {
        if input.span_end <= input.span_start {
            return Err(WriterSpanError {
                start: input.span_start,
                end: input.span_end,
            });
        }
        Ok(Self::build(input))
    }

    fn build(input: WriterOperationInput) -> Self {
        let origin_id = writer_origin_id(&input);
        Self {
            origin_id,
            operation_id: input.operation_id,
            path: input.path,
            span_start: input.span_start,
            span_end: input.span_end,
            enclosing_item: input.enclosing_item,
            normalized_call: input.normalized_call,
            sink: input.sink,
            operation_kind: input.operation_kind,
            role: input.role,
            discovery: input.discovery,
            ordinal: input.ordinal,
        }
    }

    /// Return the domain-separated origin identity.
    #[must_use]
    pub const fn origin_id(&self) -> OriginId {
        self.origin_id
    }

    /// Return the stable writer analyzer identity.
    #[must_use]
    pub const fn operation_id(&self) -> Digest {
        self.operation_id
    }

    /// Return the source path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the half-open source span.
    #[must_use]
    pub const fn span(&self) -> (u64, u64) {
        (self.span_start, self.span_end)
    }

    /// Return the operation class.
    #[must_use]
    pub const fn operation_kind(&self) -> OperationKind {
        self.operation_kind
    }

    /// Return the inventory role.
    #[must_use]
    pub const fn role(&self) -> WriterRole {
        self.role
    }

    /// Return the enclosing-item identity.
    #[must_use]
    pub const fn enclosing_item(&self) -> Digest {
        self.enclosing_item
    }

    /// Return the normalized call digest.
    #[must_use]
    pub const fn normalized_call(&self) -> Digest {
        self.normalized_call
    }

    /// Return the registered sink token.
    #[must_use]
    pub const fn sink(&self) -> &WriterToken {
        &self.sink
    }

    /// Return how the operation was discovered.
    #[must_use]
    pub const fn discovery(&self) -> SinkDiscovery {
        self.discovery
    }

    /// Return the multiset ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
}

/// An empty or reversed writer source span.
#[derive(Clone, Copy, Debug, Eq, thiserror::Error, PartialEq)]
#[error("writer span end {end} does not follow start {start}")]
pub struct WriterSpanError {
    start: u64,
    end: u64,
}

/// Complete immutable computed-origin ledger.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OriginLedger {
    pub(crate) schema_version: u32,
    pub(crate) algorithms: OriginAlgorithms,
    pub(crate) base: OriginBase,
    pub(crate) digests: OriginAuthorityDigests,
    pub(crate) source_inventory: Vec<SourceInventoryEntry>,
    pub(crate) compile_test_fixtures: Vec<CompileTestFixtureFact>,
    pub(crate) production_files: Vec<ProductionFileFact>,
    pub(crate) item_groups: Vec<ItemGroupFact>,
    pub(crate) prohibited_debt: Vec<DebtOriginFact>,
    pub(crate) writer_operations: Vec<WriterOperationFact>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct OriginAlgorithms {
    pub(crate) analyzer: String,
    pub(crate) digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct OriginBase {
    pub(crate) commit: GitObjectId,
    pub(crate) tree: GitObjectId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct OriginAuthorityDigests {
    pub(crate) repository_policy: Digest,
    pub(crate) source_inventory: Digest,
    pub(crate) generated_include_registry: Digest,
}

impl OriginLedger {
    /// Return the closed origin schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Return the exact analyzer identity.
    #[must_use]
    pub fn analyzer_version(&self) -> &str {
        &self.algorithms.analyzer
    }

    /// Return the canonical digest identity.
    #[must_use]
    pub fn digest_version(&self) -> &str {
        &self.algorithms.digest
    }

    /// Return the exact accepted base commit.
    #[must_use]
    pub const fn base_commit(&self) -> &GitObjectId {
        &self.base.commit
    }

    /// Return the exact accepted base tree.
    #[must_use]
    pub const fn base_tree(&self) -> &GitObjectId {
        &self.base.tree
    }

    /// Return the normalized hard-policy digest.
    #[must_use]
    pub const fn repository_policy_digest(&self) -> Digest {
        self.digests.repository_policy
    }

    /// Return the complete source-inventory digest.
    #[must_use]
    pub const fn source_inventory_digest(&self) -> Digest {
        self.digests.source_inventory
    }

    /// Return the exact generated-include technical registry identity.
    #[must_use]
    pub const fn generated_include_registry_digest(&self) -> Digest {
        self.digests.generated_include_registry
    }

    /// Borrow every exact classified-source row in normalized order.
    #[must_use]
    pub fn source_inventory(&self) -> &[SourceInventoryEntry] {
        &self.source_inventory
    }

    /// Borrow every exact compile-test fixture row in normalized order.
    #[must_use]
    pub fn compile_test_fixtures(&self) -> &[CompileTestFixtureFact] {
        &self.compile_test_fixtures
    }

    /// Borrow every base production fact in normalized order.
    #[must_use]
    pub fn production_files(&self) -> &[ProductionFileFact] {
        &self.production_files
    }

    /// Borrow every stable item aggregate in normalized order.
    #[must_use]
    pub fn item_groups(&self) -> &[ItemGroupFact] {
        &self.item_groups
    }

    /// Borrow the prohibited-debt multiset in normalized order.
    #[must_use]
    pub fn prohibited_debt(&self) -> &[DebtOriginFact] {
        &self.prohibited_debt
    }

    /// Borrow the generated writer inventory in normalized order.
    #[must_use]
    pub fn writer_operations(&self) -> &[WriterOperationFact] {
        &self.writer_operations
    }
}

fn debt_origin_id(path: &RepositoryPath, fingerprint: Digest, ordinal: u32) -> OriginId {
    identity_digest(
        b"prohibited-debt",
        &[
            path.as_str().as_bytes(),
            fingerprint.as_bytes(),
            &ordinal.to_be_bytes(),
        ],
    )
}

fn writer_origin_id(input: &WriterOperationInput) -> OriginId {
    identity_digest(
        b"writer-operation",
        &[
            input.operation_id.as_bytes(),
            input.path.as_str().as_bytes(),
            &input.span_start.to_be_bytes(),
            &input.span_end.to_be_bytes(),
            input.enclosing_item.as_bytes(),
            input.normalized_call.as_bytes(),
            input.sink.as_str().as_bytes(),
            operation_kind_token(input.operation_kind),
            writer_role_token(input.role),
            discovery_token(input.discovery),
            &input.ordinal.to_be_bytes(),
        ],
    )
}

const fn operation_kind_token(kind: OperationKind) -> &'static [u8] {
    match kind {
        OperationKind::Open => b"open",
        OperationKind::Create => b"create",
        OperationKind::Truncate => b"truncate",
        OperationKind::Append => b"append",
        OperationKind::Write => b"write",
        OperationKind::SetLength => b"set_length",
        OperationKind::Permissions => b"permissions",
        OperationKind::Flush => b"flush",
        OperationKind::Sync => b"sync",
        OperationKind::Persist => b"persist",
        OperationKind::Rename => b"rename",
        OperationKind::Link => b"link",
        OperationKind::Remove => b"remove",
    }
}

const fn writer_role_token(role: WriterRole) -> &'static [u8] {
    match role {
        WriterRole::RootOpen => b"root_open",
        WriterRole::HandleMutation => b"handle_mutation",
        WriterRole::Publication => b"publication",
        WriterRole::Permissions => b"permissions",
        WriterRole::Durability => b"durability",
        WriterRole::Cleanup => b"cleanup",
        WriterRole::SharedPrimitive => b"shared_primitive",
        WriterRole::FalsePositive => b"false_positive",
    }
}

const fn discovery_token(discovery: SinkDiscovery) -> &'static [u8] {
    match discovery {
        SinkDiscovery::Function => b"function",
        SinkDiscovery::Method => b"method",
        SinkDiscovery::MacroInvocation => b"macro_invocation",
        SinkDiscovery::MacroToken => b"macro_token",
    }
}

pub(super) fn identity_digest(domain: &[u8], fields: &[&[u8]]) -> OriginId {
    let mut encoded = b"norn-origin-id-1".to_vec();
    append_field(&mut encoded, domain);
    for field in fields {
        append_field(&mut encoded, field);
    }
    OriginId::new(digest_bytes(&encoded))
}

fn append_field(output: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().to_be_bytes();
    output.extend_from_slice(&[0_u8; 16][length.len()..]);
    output.extend_from_slice(&length);
    output.extend_from_slice(value);
}
