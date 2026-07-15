//! Validated current fact input and hard LOC ceilings.

use super::super::items::ItemGroupFact;
use super::super::model::{DebtOriginFact, WriterOperationFact};
use super::super::production::{ProductionFileFact, ProductionLocClass};
use super::super::reconstruct::{BaselineFactsError, RepositoryBaselineFacts};
use super::errors::LocCeilingsError;
use crate::config::{BUILTIN_ENTRYPOINT_LOC_MAX, BUILTIN_OTHER_RUST_LOC_MAX};
use crate::digest::Digest;
use crate::facts::RepositoryFacts;

/// Validated cfg-aware hard LOC ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocCeilings {
    thin_entrypoint: u32,
    other_rust: u32,
}

impl LocCeilings {
    pub(crate) const fn p1_baseline() -> Self {
        Self {
            thin_entrypoint: BUILTIN_ENTRYPOINT_LOC_MAX,
            other_rust: BUILTIN_OTHER_RUST_LOC_MAX,
        }
    }

    /// Construct positive hard ceilings.
    ///
    /// # Errors
    ///
    /// Rejects zero, which would not describe a meaningful repository limit.
    pub const fn new(thin_entrypoint: u32, other_rust: u32) -> Result<Self, LocCeilingsError> {
        if thin_entrypoint == 0 {
            return Err(LocCeilingsError::ThinEntrypointZero);
        }
        if other_rust == 0 {
            return Err(LocCeilingsError::OtherRustZero);
        }
        Ok(Self {
            thin_entrypoint,
            other_rust,
        })
    }

    pub(crate) fn exceeded(self, fact: &ProductionFileFact) -> bool {
        fact.production_loc() > self.limit_for(fact)
    }

    /// Return the library/proc-macro/binary root ceiling.
    #[must_use]
    pub const fn thin_entrypoint(self) -> u32 {
        self.thin_entrypoint
    }

    /// Return the ceiling for other production Rust files.
    #[must_use]
    pub const fn other_rust(self) -> u32 {
        self.other_rust
    }

    /// Return the semantic ceiling for one canonical production fact.
    #[must_use]
    pub fn limit_for(self, fact: &ProductionFileFact) -> u32 {
        match fact.loc_class() {
            ProductionLocClass::ThinEntrypoint => self.thin_entrypoint,
            ProductionLocClass::Other => self.other_rust,
        }
    }
}

/// Complete current fact projection consumed by legacy evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurrentRepositoryFacts {
    source_inventory_digest: Digest,
    pub(super) production_files: Vec<ProductionFileFact>,
    pub(super) item_groups: Vec<ItemGroupFact>,
    pub(super) prohibited_debt: Vec<DebtOriginFact>,
    writer_operations: Vec<WriterOperationFact>,
}

impl CurrentRepositoryFacts {
    /// Clone one complete sealed reconstruction for current evaluation.
    #[must_use]
    pub fn from_baseline(facts: &RepositoryBaselineFacts) -> Self {
        Self {
            source_inventory_digest: facts.source_inventory_digest(),
            production_files: facts.production_files().to_vec(),
            item_groups: facts.item_groups().to_vec(),
            prohibited_debt: facts.prohibited_debt().to_vec(),
            writer_operations: facts.writer_operations().to_vec(),
        }
    }

    /// Reconstruct complete current facts directly from canonical analysis.
    ///
    /// # Errors
    ///
    /// Rejects any structurally invalid, incomplete, or lossy fact graph.
    pub fn try_from_repository(facts: &RepositoryFacts) -> Result<Self, BaselineFactsError> {
        let baseline = RepositoryBaselineFacts::try_from_repository(facts)?;
        Ok(Self::from_baseline(&baseline))
    }

    /// Return the complete current source-inventory digest.
    #[must_use]
    pub const fn source_inventory_digest(&self) -> Digest {
        self.source_inventory_digest
    }

    /// Borrow current production facts.
    #[must_use]
    pub fn production_files(&self) -> &[ProductionFileFact] {
        &self.production_files
    }

    /// Borrow current stable item aggregates.
    #[must_use]
    pub fn item_groups(&self) -> &[ItemGroupFact] {
        &self.item_groups
    }

    /// Borrow the current prohibited-debt multiset.
    #[must_use]
    pub fn prohibited_debt(&self) -> &[DebtOriginFact] {
        &self.prohibited_debt
    }

    /// Borrow every current resolved writer operation.
    #[must_use]
    pub fn writer_operations(&self) -> &[WriterOperationFact] {
        &self.writer_operations
    }
}
