//! Lossless reconstruction of every baseline-relevant canonical fact family.

use thiserror::Error;

use super::items::{ItemGroupError, ItemGroupFact};
use super::model::{DebtOriginFact, WriterOperationFact};
use super::production::{ProductionFactError, ProductionFileFact};
use crate::digest::Digest;
use crate::facts::{RepositoryFacts, RepositoryFactsError, SourceInventoryEntry};
use crate::rust::modules::CompileTestFixtureFact;

/// Sealed complete baseline projection of one canonical repository fact graph.
///
/// Fields are intentionally private. The only constructor consumes every row
/// from a structurally valid [`RepositoryFacts`], so callers cannot select a
/// convenient subset before origin generation or current-state evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryBaselineFacts {
    source_inventory: Vec<SourceInventoryEntry>,
    source_inventory_digest: Digest,
    compile_test_fixtures: Vec<CompileTestFixtureFact>,
    production_files: Vec<ProductionFileFact>,
    item_groups: Vec<ItemGroupFact>,
    prohibited_debt: Vec<DebtOriginFact>,
    writer_operations: Vec<WriterOperationFact>,
}

impl RepositoryBaselineFacts {
    /// Reconstruct every baseline family from the sealed canonical graph.
    ///
    /// # Errors
    ///
    /// Rejects invalid/incomplete repository facts, failed canonical
    /// conversions, and any duplicate or unstable normalized sequence.
    pub fn try_from_repository(facts: &RepositoryFacts) -> Result<Self, BaselineFactsError> {
        facts
            .validate_integrity()
            .map_err(BaselineFactsError::Repository)?;

        let production_files = facts
            .production_files()
            .iter()
            .enumerate()
            .map(|(index, fact)| {
                ProductionFileFact::try_from(fact)
                    .map_err(|source| BaselineFactsError::Production { index, source })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let item_groups = facts
            .items()
            .iter()
            .enumerate()
            .map(|(index, fact)| {
                ItemGroupFact::try_from(fact)
                    .map_err(|source| BaselineFactsError::Item { index, source })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut prohibited_debt = facts
            .debt()
            .iter()
            .map(DebtOriginFact::from)
            .collect::<Vec<_>>();
        prohibited_debt.sort_by(debt_order);
        let writers = facts
            .writers()
            .ok_or(BaselineFactsError::WriterUnavailable)?;
        let mut writer_operations = writers
            .operations()
            .iter()
            .map(WriterOperationFact::from_canonical)
            .collect::<Vec<_>>();
        writer_operations.sort_by(writer_order);

        validate_order(
            &production_files,
            &item_groups,
            &prohibited_debt,
            &writer_operations,
        )?;
        Ok(Self {
            source_inventory: facts.source_inventory().to_vec(),
            source_inventory_digest: facts.source_inventory_digest(),
            compile_test_fixtures: facts.compile_test_fixtures().to_vec(),
            production_files,
            item_groups,
            prohibited_debt,
            writer_operations,
        })
    }

    /// Return the complete source-inventory digest.
    #[must_use]
    pub const fn source_inventory_digest(&self) -> Digest {
        self.source_inventory_digest
    }

    /// Borrow every exact classified-source row consumed by reconstruction.
    #[must_use]
    pub fn source_inventory(&self) -> &[SourceInventoryEntry] {
        &self.source_inventory
    }

    /// Return the exact number of classified source rows consumed.
    #[must_use]
    pub const fn source_count(&self) -> usize {
        self.source_inventory.len()
    }

    /// Borrow every proven compile-test fixture row.
    #[must_use]
    pub fn compile_test_fixtures(&self) -> &[CompileTestFixtureFact] {
        &self.compile_test_fixtures
    }

    /// Borrow every converted production row.
    #[must_use]
    pub fn production_files(&self) -> &[ProductionFileFact] {
        &self.production_files
    }

    /// Borrow every converted stable item group.
    #[must_use]
    pub fn item_groups(&self) -> &[ItemGroupFact] {
        &self.item_groups
    }

    /// Borrow every converted prohibited-debt occurrence.
    #[must_use]
    pub fn prohibited_debt(&self) -> &[DebtOriginFact] {
        &self.prohibited_debt
    }

    /// Borrow every converted resolved writer operation.
    #[must_use]
    pub fn writer_operations(&self) -> &[WriterOperationFact] {
        &self.writer_operations
    }
}

fn validate_order(
    production: &[ProductionFileFact],
    items: &[ItemGroupFact],
    debt: &[DebtOriginFact],
    writers: &[WriterOperationFact],
) -> Result<(), BaselineFactsError> {
    for (index, pair) in production.windows(2).enumerate() {
        if pair[0].path() >= pair[1].path() {
            return Err(BaselineFactsError::ProductionOrder { index: index + 1 });
        }
    }
    for (index, pair) in items.windows(2).enumerate() {
        if item_order(&pair[0], &pair[1]).is_ge() {
            return Err(BaselineFactsError::ItemOrder { index: index + 1 });
        }
    }
    for (index, pair) in debt.windows(2).enumerate() {
        if debt_order(&pair[0], &pair[1]).is_ge() {
            return Err(BaselineFactsError::DebtOrder { index: index + 1 });
        }
    }
    for (index, pair) in writers.windows(2).enumerate() {
        if writer_order(&pair[0], &pair[1]).is_ge() {
            return Err(BaselineFactsError::WriterOrder { index: index + 1 });
        }
    }
    Ok(())
}

pub(super) fn item_order(left: &ItemGroupFact, right: &ItemGroupFact) -> std::cmp::Ordering {
    (left.path(), left.base_identity(), left.content()).cmp(&(
        right.path(),
        right.base_identity(),
        right.content(),
    ))
}

pub(super) fn debt_order(left: &DebtOriginFact, right: &DebtOriginFact) -> std::cmp::Ordering {
    (left.fingerprint(), left.ordinal(), left.path()).cmp(&(
        right.fingerprint(),
        right.ordinal(),
        right.path(),
    ))
}

pub(super) fn writer_order(
    left: &WriterOperationFact,
    right: &WriterOperationFact,
) -> std::cmp::Ordering {
    (left.operation_id(), left.path(), left.span()).cmp(&(
        right.operation_id(),
        right.path(),
        right.span(),
    ))
}

/// Canonical-to-baseline reconstruction failure.
#[derive(Debug, Error)]
pub enum BaselineFactsError {
    /// The sealed repository graph is structurally invalid or incomplete.
    #[error("repository facts are not complete enough for baseline reconstruction")]
    Repository(#[source] RepositoryFactsError),
    /// A production row could not be represented losslessly.
    #[error("canonical production fact at row {index} is invalid")]
    Production {
        /// Invalid canonical row.
        index: usize,
        /// Lossless conversion failure.
        #[source]
        source: ProductionFactError,
    },
    /// An item aggregate could not be represented losslessly.
    #[error("canonical item group at row {index} is invalid")]
    Item {
        /// Invalid canonical row.
        index: usize,
        /// Lossless conversion failure.
        #[source]
        source: ItemGroupError,
    },
    /// Writer inventory disappeared after integrity validation.
    #[error("canonical writer inventory is unavailable")]
    WriterUnavailable,
    /// Production rows were not strictly path sorted and unique.
    #[error("converted production rows are not strictly sorted at row {index}")]
    ProductionOrder {
        /// First invalid row.
        index: usize,
    },
    /// Item rows were not strictly stable-key sorted and unique.
    #[error("converted item groups are not strictly sorted at row {index}")]
    ItemOrder {
        /// First invalid row.
        index: usize,
    },
    /// Debt rows were not strictly sorted and unique.
    #[error("converted debt rows are not strictly sorted at row {index}")]
    DebtOrder {
        /// First invalid row.
        index: usize,
    },
    /// Writer rows were not strictly sorted and unique.
    #[error("converted writer rows are not strictly sorted at row {index}")]
    WriterOrder {
        /// First invalid row.
        index: usize,
    },
}
