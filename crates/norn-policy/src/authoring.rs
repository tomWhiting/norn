//! Deterministic review inventories for P1 authority authoring.

mod inventory;
mod reviewed;
#[cfg(test)]
mod reviewed_tests;

pub use inventory::{
    DebtReviewRequirement, LocReviewRequirement, P1ReviewEncodeError, P1ReviewIdentityError,
    P1ReviewInventory, ReviewSpan, WriterReviewRequirement,
};
pub use reviewed::{P1ReviewedInputError, P1ReviewedInputs};

use std::collections::BTreeMap;

use thiserror::Error;

use crate::baseline::{
    ExactP1Base, ExactP1BaseError, LocCeilings, OriginEncodeError, OriginError, OriginLedger,
};
use crate::facts::{RepositoryFactsError, analyze_facts};
use crate::rust::modules::GeneratedIncludeRegistry;
use crate::strict_json::{StrictJsonError, decode_strict_json};
use crate::writers::{OperationKind, SinkDiscovery, WriterRole, WriterToken};
use crate::{
    CompleteCurrentSnapshot, Digest, EntryKind, P1BaseSnapshot, RepositoryPath, RepositoryPathError,
};

const GENERATED_INCLUDES_PATH: &str = "policy/generated-includes.json";
use inventory::REVIEW_INVENTORY_SCHEMA_VERSION;

/// Exact machine-derived inputs requiring semantic review before P1 can lock.
pub struct P1ReviewCandidate {
    origin: OriginLedger,
    inventory: P1ReviewInventory,
}

impl P1ReviewCandidate {
    /// Derive the immutable origin and unresolved review inventory from sealed
    /// current/base roles.
    ///
    /// # Errors
    ///
    /// Fails closed when a fixed authority is missing, the exact base cannot be
    /// reconstructed, current facts are incomplete, or a writer identity maps
    /// to inconsistent semantics.
    pub fn derive(
        base: &P1BaseSnapshot,
        current: &CompleteCurrentSnapshot,
    ) -> Result<Self, P1ReviewError> {
        let generated = generated_registry(current)?;
        let exact_base = ExactP1Base::acquire(base, &generated)?;
        let origin = OriginLedger::generate_p1(&exact_base)?;
        let current_facts = analyze_facts(current.snapshot(), &generated);
        current_facts.validate_integrity()?;
        let Some(current_writers) = current_facts.writers() else {
            return Err(P1ReviewError::CurrentWriters);
        };
        if !current_writers.is_registry_complete() {
            return Err(P1ReviewError::CurrentWriters);
        }
        if exact_base.compile_test_fixtures() != current_facts.compile_test_fixtures() {
            return Err(P1ReviewError::CompileTestFixtureDrift);
        }

        let limits = LocCeilings::p1_baseline();
        let loc_exceptions = origin
            .production_files()
            .iter()
            .filter(|fact| limits.exceeded(fact))
            .map(|fact| LocReviewRequirement {
                origin_id: fact.origin_id().digest(),
                path: fact.path().clone(),
                loc_class: fact.loc_class(),
                production_loc: fact.production_loc(),
                baseline_limit: limits.limit_for(fact),
            })
            .collect();
        let debt_exceptions = origin
            .prohibited_debt()
            .iter()
            .map(|fact| DebtReviewRequirement {
                origin_id: fact.origin_id().digest(),
                path: fact.path().clone(),
                fingerprint: fact.fingerprint(),
                ordinal: fact.ordinal(),
            })
            .collect();
        let writer_operations = writer_review_rows(&origin, current_writers.operations())?;
        let inventory = P1ReviewInventory {
            schema_version: REVIEW_INVENTORY_SCHEMA_VERSION,
            base_commit: origin.base_commit().as_str().to_owned(),
            base_tree: origin.base_tree().as_str().to_owned(),
            origin_digest: origin
                .normalized_digest()
                .map_err(P1ReviewError::OriginDigest)?,
            base_source_inventory: exact_base.source_inventory().to_vec(),
            current_source_inventory: current_facts.source_inventory().to_vec(),
            base_compile_test_fixtures: exact_base.compile_test_fixtures().to_vec(),
            current_compile_test_fixtures: current_facts.compile_test_fixtures().to_vec(),
            loc_exceptions,
            debt_exceptions,
            writer_operations,
        };
        Ok(Self { origin, inventory })
    }

    /// Borrow the exact immutable origin generated from the ratified base.
    #[must_use]
    pub const fn origin(&self) -> &OriginLedger {
        &self.origin
    }

    /// Borrow the deterministic unresolved review inventory.
    #[must_use]
    pub const fn inventory(&self) -> &P1ReviewInventory {
        &self.inventory
    }

    /// Encode the exact immutable origin as checked-in candidate bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON serialization fails.
    pub fn encode_origin(&self) -> Result<Vec<u8>, OriginEncodeError> {
        self.origin.encode_p1()
    }

    /// Encode the unresolved review inventory deterministically.
    ///
    /// # Errors
    ///
    /// Returns an error if identity generation or JSON serialization fails.
    pub fn encode_inventory(&self) -> Result<Vec<u8>, P1ReviewEncodeError> {
        self.inventory.encode_document()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WriterSemantics {
    path: RepositoryPath,
    sink: WriterToken,
    operation_kind: OperationKind,
    role: WriterRole,
    discovery: SinkDiscovery,
    ordinal: u32,
    enclosing_item: Digest,
    normalized_call: Digest,
}

fn generated_registry(
    current: &CompleteCurrentSnapshot,
) -> Result<GeneratedIncludeRegistry, P1ReviewError> {
    let path =
        RepositoryPath::parse(GENERATED_INCLUDES_PATH).map_err(P1ReviewError::CompiledPath)?;
    let Some(entry) = current.snapshot().get(&path) else {
        return Err(P1ReviewError::GeneratedRegistryMissing);
    };
    if entry.kind() != EntryKind::Regular {
        return Err(P1ReviewError::GeneratedRegistryKind);
    }
    decode_strict_json(entry.bytes()).map_err(P1ReviewError::GeneratedRegistry)
}

fn writer_review_rows(
    origin: &OriginLedger,
    current: &[crate::writers::WriterOperation],
) -> Result<Vec<WriterReviewRequirement>, P1ReviewError> {
    let mut rows = BTreeMap::new();
    for operation in origin.writer_operations() {
        let semantics = WriterSemantics {
            path: operation.path().clone(),
            sink: operation.sink().clone(),
            operation_kind: operation.operation_kind(),
            role: operation.role(),
            discovery: operation.discovery(),
            ordinal: operation.ordinal(),
            enclosing_item: operation.enclosing_item(),
            normalized_call: operation.normalized_call(),
        };
        let (start, end) = operation.span();
        let row = row_from_semantics(
            operation.operation_id(),
            &semantics,
            Some(ReviewSpan { start, end }),
            None,
        );
        if rows
            .insert(operation.operation_id(), (semantics, row))
            .is_some()
        {
            return Err(P1ReviewError::WriterIdentityCollision);
        }
    }

    for operation in current {
        let semantics = WriterSemantics {
            path: operation.path().clone(),
            sink: operation.sink().clone(),
            operation_kind: operation.kind(),
            role: operation.role(),
            discovery: operation.discovery(),
            ordinal: operation.ordinal(),
            enclosing_item: operation.enclosing_item(),
            normalized_call: operation.normalized_call(),
        };
        let span = ReviewSpan {
            start: operation.span().start(),
            end: operation.span().end(),
        };
        if let Some((base_semantics, row)) = rows.get_mut(&operation.id().digest()) {
            if *base_semantics != semantics {
                return Err(P1ReviewError::WriterIdentityCollision);
            }
            row.current_span = Some(span);
        } else {
            let row = row_from_semantics(operation.id().digest(), &semantics, None, Some(span));
            rows.insert(operation.id().digest(), (semantics, row));
        }
    }
    Ok(rows.into_values().map(|(_, row)| row).collect())
}

fn row_from_semantics(
    operation_id: Digest,
    semantics: &WriterSemantics,
    base_span: Option<ReviewSpan>,
    current_span: Option<ReviewSpan>,
) -> WriterReviewRequirement {
    WriterReviewRequirement {
        operation_id,
        path: semantics.path.clone(),
        sink: semantics.sink.clone(),
        operation_kind: semantics.operation_kind,
        role: semantics.role,
        discovery: semantics.discovery,
        ordinal: semantics.ordinal,
        base_span,
        current_span,
    }
}

/// Failure to derive a complete P1 review candidate.
#[derive(Debug, Error)]
pub enum P1ReviewError {
    /// A compiled fixed authoring path did not satisfy repository path rules.
    #[error("compiled authoring path is invalid")]
    CompiledPath(#[source] RepositoryPathError),
    /// The complete current snapshot omitted the generated-include authority.
    #[error("generated-include authority is missing")]
    GeneratedRegistryMissing,
    /// The generated-include authority was not an ordinary file.
    #[error("generated-include authority is not a regular file")]
    GeneratedRegistryKind,
    /// The generated-include authority failed strict decoding.
    #[error("generated-include authority is invalid")]
    GeneratedRegistry(#[source] StrictJsonError),
    /// The base snapshot did not establish the ratified P1 authority.
    #[error("exact P1 base could not be established")]
    ExactBase(#[from] ExactP1BaseError),
    /// The immutable origin could not be generated from the exact base.
    #[error("immutable P1 origin could not be generated")]
    Origin(#[from] OriginError),
    /// The generated origin could not be normalized for review binding.
    #[error("immutable P1 origin digest could not be generated")]
    OriginDigest(#[source] crate::baseline::OriginDigestError),
    /// The current repository fact graph was incomplete or inconsistent.
    #[error("current repository facts are incomplete")]
    CurrentFacts(#[from] RepositoryFactsError),
    /// Current writer analysis did not produce a complete registry-bound inventory.
    #[error("current writer inventory is incomplete")]
    CurrentWriters,
    /// Current compile-test provenance differs from the immutable base authority.
    #[error("current compile-test fixture provenance differs from the immutable base")]
    CompileTestFixtureDrift,
    /// One operation identity mapped to inconsistent base/current semantics.
    #[error("one writer identity maps to inconsistent semantics")]
    WriterIdentityCollision,
}
