//! Strict decoding and normalization of the immutable origin ledger.

mod errors;
mod validate;

pub use errors::{OriginAuthorityError, OriginDigestError, OriginEncodeError, OriginError};

use serde::Deserialize;

use super::exact_base::{ExactP1Base, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY};
use super::items::ItemGroupFact;
use super::model::{
    DebtOriginFact, ORIGIN_SCHEMA_VERSION, OriginAlgorithms, OriginAuthorityDigests, OriginBase,
    OriginLedger, P1_BASE_COMMIT, P1_BASE_TREE, WriterOperationFact, WriterOperationInput,
};
use super::production::{ProductionFileFact, ProductionLocClass};
use crate::config::RepositoryPolicy;
use crate::digest::{Digest, digest_json};
use crate::facts::SourceInventoryEntry;
use crate::path::RepositoryPath;
use crate::phase_lock::GitObjectId;
use crate::rust::modules::{CompileTestFixtureFact, ModuleTargetIdentity};
use crate::strict_json::decode_strict_json;
use crate::version::{ANALYZER_VERSION, DIGEST_VERSION};
use crate::writers::identity::{OperationIdentityInput, operation_id};
use crate::writers::{OperationKind, SinkDiscovery, WriterRole, WriterToken};
use validate::{debt_order, item_group_order, validate_sequences, writer_order};

impl OriginLedger {
    /// Generate the exact P1 origin from one complete sealed reconstruction.
    ///
    /// Arbitrary reconstructed facts are deliberately not accepted:
    ///
    /// ```compile_fail
    /// use norn_policy::baseline::{OriginError, OriginLedger, RepositoryBaselineFacts};
    ///
    /// fn cannot_stamp_arbitrary_facts(
    ///     facts: &RepositoryBaselineFacts,
    /// ) -> Result<OriginLedger, OriginError> {
    ///     OriginLedger::generate_p1(facts)
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Rejects an impossible compiled base identifier or any lost ordering
    /// invariant. Callers cannot supply individual fact-family vectors.
    pub fn generate_p1(base: &ExactP1Base) -> Result<Self, OriginError> {
        let facts = base.facts();
        let repository_policy_digest = RepositoryPolicy::p1_baseline()
            .normalized_digest()
            .map_err(OriginError::RepositoryPolicyDigest)?;
        Self::from_normalized(
            repository_policy_digest,
            facts.source_inventory().to_vec(),
            facts.source_inventory_digest(),
            facts.compile_test_fixtures().to_vec(),
            base.generated_registry_technical_identity(),
            facts.production_files().to_vec(),
            facts.item_groups().to_vec(),
            facts.prohibited_debt().to_vec(),
            facts.writer_operations().to_vec(),
        )
    }

    fn from_normalized(
        repository_policy_digest: Digest,
        mut source_inventory: Vec<SourceInventoryEntry>,
        source_inventory_digest: Digest,
        mut compile_test_fixtures: Vec<CompileTestFixtureFact>,
        generated_include_registry_digest: Digest,
        mut production_files: Vec<ProductionFileFact>,
        mut item_groups: Vec<ItemGroupFact>,
        mut prohibited_debt: Vec<DebtOriginFact>,
        mut writer_operations: Vec<WriterOperationFact>,
    ) -> Result<Self, OriginError> {
        source_inventory.sort();
        compile_test_fixtures.sort();
        production_files.sort_by(|left, right| left.path().cmp(right.path()));
        item_groups.sort_by(item_group_order);
        prohibited_debt.sort_by(debt_order);
        writer_operations.sort_by(writer_order);
        validate_sequences(
            &source_inventory,
            &compile_test_fixtures,
            source_inventory_digest,
            &production_files,
            &item_groups,
            &prohibited_debt,
            &writer_operations,
        )?;

        let commit = GitObjectId::parse(P1_BASE_COMMIT).map_err(OriginError::BaseIdentity)?;
        let tree = GitObjectId::parse(P1_BASE_TREE).map_err(OriginError::BaseIdentity)?;
        Ok(Self {
            schema_version: ORIGIN_SCHEMA_VERSION,
            algorithms: OriginAlgorithms {
                analyzer: ANALYZER_VERSION.to_owned(),
                digest: DIGEST_VERSION.to_owned(),
            },
            base: OriginBase { commit, tree },
            digests: OriginAuthorityDigests {
                repository_policy: repository_policy_digest,
                source_inventory: source_inventory_digest,
                generated_include_registry: generated_include_registry_digest,
            },
            source_inventory,
            compile_test_fixtures,
            production_files,
            item_groups,
            prohibited_debt,
            writer_operations,
        })
    }

    /// Decode strict JSON and verify every immutable identity and ordering rule.
    ///
    /// # Errors
    ///
    /// Rejects ambiguous or unknown JSON, wrong versions/base objects,
    /// unsorted or duplicate facts, invalid spans, and forged origin IDs.
    pub fn decode_p1(bytes: &[u8]) -> Result<Self, OriginError> {
        let document: OriginDocument = decode_strict_json(bytes).map_err(OriginError::Json)?;
        document.validate()
    }

    /// Require the authority digests computed by the caller.
    ///
    /// # Errors
    ///
    /// Returns a closed mismatch when policy or source inventory differs.
    pub fn verify_authorities(
        &self,
        repository_policy: Digest,
        source_inventory: Digest,
        generated_include_registry: Digest,
    ) -> Result<(), OriginAuthorityError> {
        if self.digests.repository_policy != repository_policy {
            return Err(OriginAuthorityError::RepositoryPolicy);
        }
        if self.digests.source_inventory != source_inventory {
            return Err(OriginAuthorityError::SourceInventory);
        }
        if self.digests.generated_include_registry != generated_include_registry {
            return Err(OriginAuthorityError::GeneratedRegistry);
        }
        Ok(())
    }

    /// Hash the normalized closed origin value using canonical JSON.
    ///
    /// # Errors
    ///
    /// Returns an error only if serialization or canonical JSON encoding fails.
    pub fn normalized_digest(&self) -> Result<Digest, OriginDigestError> {
        let value = serde_json::to_value(self).map_err(OriginDigestError::Serialization)?;
        digest_json(&value).map_err(OriginDigestError::Canonical)
    }

    /// Encode one deterministic checked-in P1 origin document.
    ///
    /// # Errors
    ///
    /// Returns an error if the closed ledger cannot be represented as JSON.
    pub fn encode_p1(&self) -> Result<Vec<u8>, OriginEncodeError> {
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(OriginEncodeError::Serialization)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginDocument {
    schema_version: u32,
    algorithms: AlgorithmDocument,
    base: BaseDocument,
    digests: AuthorityDigestDocument,
    source_inventory: Vec<SourceInventoryEntry>,
    compile_test_fixtures: Vec<CompileTestFixtureFact>,
    production_files: Vec<ProductionFactDocument>,
    item_groups: Vec<ItemGroupDocument>,
    prohibited_debt: Vec<DebtFactDocument>,
    writer_operations: Vec<WriterFactDocument>,
}

impl OriginDocument {
    fn validate(self) -> Result<OriginLedger, OriginError> {
        if self.schema_version != ORIGIN_SCHEMA_VERSION {
            return Err(OriginError::SchemaVersion {
                actual: self.schema_version,
            });
        }
        if self.algorithms.analyzer != ANALYZER_VERSION {
            return Err(OriginError::AnalyzerVersion);
        }
        if self.algorithms.digest != DIGEST_VERSION {
            return Err(OriginError::DigestVersion);
        }
        if self.base.commit.as_str() != P1_BASE_COMMIT {
            return Err(OriginError::BaseCommit);
        }
        if self.base.tree.as_str() != P1_BASE_TREE {
            return Err(OriginError::BaseTree);
        }
        if self.digests.generated_include_registry != P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY {
            return Err(OriginError::GeneratedRegistry);
        }

        let production_files = self
            .production_files
            .into_iter()
            .enumerate()
            .map(|(index, fact)| fact.validate(index))
            .collect::<Result<Vec<_>, _>>()?;
        let item_groups = self
            .item_groups
            .into_iter()
            .enumerate()
            .map(|(index, fact)| fact.validate(index))
            .collect::<Result<Vec<_>, _>>()?;
        let prohibited_debt = self
            .prohibited_debt
            .into_iter()
            .enumerate()
            .map(|(index, fact)| fact.validate(index))
            .collect::<Result<Vec<_>, _>>()?;
        let writer_operations = self
            .writer_operations
            .into_iter()
            .enumerate()
            .map(|(index, fact)| fact.validate(index))
            .collect::<Result<Vec<_>, _>>()?;
        validate_sequences(
            &self.source_inventory,
            &self.compile_test_fixtures,
            self.digests.source_inventory,
            &production_files,
            &item_groups,
            &prohibited_debt,
            &writer_operations,
        )?;

        Ok(OriginLedger {
            schema_version: self.schema_version,
            algorithms: OriginAlgorithms {
                analyzer: self.algorithms.analyzer,
                digest: self.algorithms.digest,
            },
            base: OriginBase {
                commit: self.base.commit,
                tree: self.base.tree,
            },
            digests: OriginAuthorityDigests {
                repository_policy: self.digests.repository_policy,
                source_inventory: self.digests.source_inventory,
                generated_include_registry: self.digests.generated_include_registry,
            },
            source_inventory: self.source_inventory,
            compile_test_fixtures: self.compile_test_fixtures,
            production_files,
            item_groups,
            prohibited_debt,
            writer_operations,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlgorithmDocument {
    analyzer: String,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BaseDocument {
    commit: GitObjectId,
    tree: GitObjectId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityDigestDocument {
    repository_policy: Digest,
    source_inventory: Digest,
    generated_include_registry: Digest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductionFactDocument {
    origin_id: Digest,
    path: RepositoryPath,
    targets: Vec<ModuleTargetIdentity>,
    target_set_identity: Digest,
    loc_class: ProductionLocClass,
    production_loc: u32,
    projection_hash: Digest,
}

impl ProductionFactDocument {
    fn validate(self, index: usize) -> Result<ProductionFileFact, OriginError> {
        let fact = ProductionFileFact::from_decoded(
            self.path,
            self.targets,
            self.target_set_identity,
            self.loc_class,
            self.production_loc,
            self.projection_hash,
        )
        .map_err(|source| OriginError::ProductionFact { index, source })?;
        if fact.origin_id().digest() != self.origin_id {
            return Err(OriginError::ProductionId { index });
        }
        Ok(fact)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemGroupDocument {
    origin_id: Digest,
    path: RepositoryPath,
    base_identity: Digest,
    content: Digest,
    production_count: u32,
    test_only_count: u32,
}

impl ItemGroupDocument {
    fn validate(self, index: usize) -> Result<ItemGroupFact, OriginError> {
        let fact = ItemGroupFact::new(
            self.path,
            self.base_identity,
            self.content,
            self.production_count,
            self.test_only_count,
        )
        .map_err(|source| OriginError::ItemGroup { index, source })?;
        if fact.origin_id().digest() != self.origin_id {
            return Err(OriginError::ItemGroupId { index });
        }
        Ok(fact)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DebtFactDocument {
    origin_id: Digest,
    path: RepositoryPath,
    fingerprint: Digest,
    ordinal: u32,
}

impl DebtFactDocument {
    fn validate(self, index: usize) -> Result<DebtOriginFact, OriginError> {
        let fact = DebtOriginFact::new(self.path, self.fingerprint, self.ordinal);
        if fact.origin_id().digest() != self.origin_id {
            return Err(OriginError::DebtId { index });
        }
        Ok(fact)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriterFactDocument {
    origin_id: Digest,
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

impl WriterFactDocument {
    fn validate(self, index: usize) -> Result<WriterOperationFact, OriginError> {
        let expected = operation_id(
            &OperationIdentityInput {
                path: &self.path,
                enclosing_item: self.enclosing_item,
                normalized_call: self.normalized_call,
                sink: &self.sink,
                kind: self.operation_kind,
                role: self.role,
                discovery: self.discovery,
            },
            self.ordinal,
        );
        if expected.digest() != self.operation_id {
            return Err(OriginError::WriterOperationId { index });
        }
        let fact = WriterOperationFact::from_decoded(WriterOperationInput {
            operation_id: self.operation_id,
            path: self.path,
            span_start: self.span_start,
            span_end: self.span_end,
            enclosing_item: self.enclosing_item,
            normalized_call: self.normalized_call,
            sink: self.sink,
            operation_kind: self.operation_kind,
            role: self.role,
            discovery: self.discovery,
            ordinal: self.ordinal,
        })
        .map_err(|source| OriginError::WriterSpan { index, source })?;
        if fact.origin_id().digest() != self.origin_id {
            return Err(OriginError::WriterId { index });
        }
        Ok(fact)
    }
}
