//! Lossless canonical production-fact conversion and identity.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::model::{OriginId, identity_digest};
use crate::digest::{CanonicalJsonError, Digest, digest_json};
use crate::facts;
use crate::path::RepositoryPath;
use crate::rust::modules::{ModuleTargetIdentity, ModuleTargetKind};

/// Closed semantic LOC ceiling class.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductionLocClass {
    /// Exact root of a library, proc-macro, or binary target.
    ThinEntrypoint,
    /// Any other production-reachable file, including examples and build roots.
    Other,
}

impl ProductionLocClass {
    const fn token(self) -> &'static str {
        match self {
            Self::ThinEntrypoint => "thin_entrypoint",
            Self::Other => "other",
        }
    }
}

/// One production file with its complete target set and projection facts.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ProductionFileFact {
    origin_id: OriginId,
    path: RepositoryPath,
    targets: Vec<ModuleTargetIdentity>,
    target_set_identity: Digest,
    loc_class: ProductionLocClass,
    production_loc: u32,
    projection_hash: Digest,
}

impl ProductionFileFact {
    /// Convert the canonical shared repository fact without a second adapter.
    ///
    /// # Errors
    ///
    /// Rejects empty, unsorted, duplicate, or non-production targets, target
    /// serialization failures, and LOC values outside the stable u32 ledger.
    pub fn from_canonical(value: &facts::ProductionFileFact) -> Result<Self, ProductionFactError> {
        let production_loc =
            u32::try_from(value.metrics.loc).map_err(ProductionFactError::LocOverflow)?;
        Self::build(
            value.path.clone(),
            value.targets.clone(),
            production_loc,
            value.metrics.projection,
            None,
            None,
        )
    }

    pub(crate) fn from_decoded(
        path: RepositoryPath,
        targets: Vec<ModuleTargetIdentity>,
        target_set_identity: Digest,
        loc_class: ProductionLocClass,
        production_loc: u32,
        projection_hash: Digest,
    ) -> Result<Self, ProductionFactError> {
        Self::build(
            path,
            targets,
            production_loc,
            projection_hash,
            Some(target_set_identity),
            Some(loc_class),
        )
    }

    fn build(
        path: RepositoryPath,
        targets: Vec<ModuleTargetIdentity>,
        production_loc: u32,
        projection_hash: Digest,
        expected_target_identity: Option<Digest>,
        expected_loc_class: Option<ProductionLocClass>,
    ) -> Result<Self, ProductionFactError> {
        validate_targets(&targets)?;
        let target_set_identity = target_set_identity(&targets)?;
        if expected_target_identity.is_some_and(|expected| expected != target_set_identity) {
            return Err(ProductionFactError::TargetSetIdentity);
        }
        let loc_class = derive_loc_class(&path, &targets);
        if expected_loc_class.is_some_and(|expected| expected != loc_class) {
            return Err(ProductionFactError::LocClass);
        }
        let origin_id = production_origin_id(
            &path,
            target_set_identity,
            loc_class,
            production_loc,
            projection_hash,
        );
        Ok(Self {
            origin_id,
            path,
            targets,
            target_set_identity,
            loc_class,
            production_loc,
            projection_hash,
        })
    }

    /// Return the stable fact identity.
    #[must_use]
    pub const fn origin_id(&self) -> OriginId {
        self.origin_id
    }

    /// Return the repository-relative source path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Borrow every sorted unique production target establishing reachability.
    #[must_use]
    pub fn targets(&self) -> &[ModuleTargetIdentity] {
        &self.targets
    }

    /// Return the canonical identity of the complete target vector.
    #[must_use]
    pub const fn target_set_identity(&self) -> Digest {
        self.target_set_identity
    }

    /// Return the semantic hard-limit class.
    #[must_use]
    pub const fn loc_class(&self) -> ProductionLocClass {
        self.loc_class
    }

    /// Return cfg-aware production LOC.
    #[must_use]
    pub const fn production_loc(&self) -> u32 {
        self.production_loc
    }

    /// Return the complete path-bound production projection hash.
    #[must_use]
    pub const fn projection_hash(&self) -> Digest {
        self.projection_hash
    }

    /// Return the production projection used in the origin identity.
    #[must_use]
    pub const fn projection_identity(&self) -> Digest {
        self.projection_hash
    }
}

impl TryFrom<&facts::ProductionFileFact> for ProductionFileFact {
    type Error = ProductionFactError;

    fn try_from(value: &facts::ProductionFileFact) -> Result<Self, Self::Error> {
        Self::from_canonical(value)
    }
}

fn validate_targets(targets: &[ModuleTargetIdentity]) -> Result<(), ProductionFactError> {
    if targets.is_empty() {
        return Err(ProductionFactError::EmptyTargets);
    }
    for (index, target) in targets.iter().enumerate() {
        if matches!(
            target.kind,
            ModuleTargetKind::IntegrationTest | ModuleTargetKind::Benchmark
        ) {
            return Err(ProductionFactError::NonProductionTarget { index });
        }
    }
    for (index, pair) in targets.windows(2).enumerate() {
        if pair[0] >= pair[1] {
            return Err(ProductionFactError::TargetOrder { index: index + 1 });
        }
    }
    Ok(())
}

fn target_set_identity(targets: &[ModuleTargetIdentity]) -> Result<Digest, ProductionFactError> {
    let value = serde_json::to_value(targets).map_err(ProductionFactError::TargetSerialization)?;
    digest_json(&value).map_err(ProductionFactError::TargetDigest)
}

fn derive_loc_class(path: &RepositoryPath, targets: &[ModuleTargetIdentity]) -> ProductionLocClass {
    if targets.iter().any(|target| {
        target.root == *path
            && matches!(
                target.kind,
                ModuleTargetKind::Library | ModuleTargetKind::ProcMacro | ModuleTargetKind::Binary
            )
    }) {
        ProductionLocClass::ThinEntrypoint
    } else {
        ProductionLocClass::Other
    }
}

fn production_origin_id(
    path: &RepositoryPath,
    target_set_identity: Digest,
    loc_class: ProductionLocClass,
    production_loc: u32,
    projection_identity: Digest,
) -> OriginId {
    identity_digest(
        b"production-file",
        &[
            path.as_str().as_bytes(),
            target_set_identity.as_bytes(),
            loc_class.token().as_bytes(),
            &production_loc.to_be_bytes(),
            projection_identity.as_bytes(),
        ],
    )
}

/// Canonical production-fact conversion or decoded-ledger validation failure.
#[derive(Debug, Error)]
pub enum ProductionFactError {
    /// A production fact had no production target.
    #[error("production fact has no production target")]
    EmptyTargets,
    /// A test or benchmark target appeared in a production target set.
    #[error("production fact target at row {index} is not a production target")]
    NonProductionTarget {
        /// Invalid target index.
        index: usize,
    },
    /// Targets were not strictly sorted and unique.
    #[error("production fact targets are not strictly sorted at row {index}")]
    TargetOrder {
        /// First invalid target index.
        index: usize,
    },
    /// Canonical LOC did not fit the stable ledger integer.
    #[error("production LOC exceeds u32")]
    LocOverflow(#[source] std::num::TryFromIntError),
    /// The target vector could not be represented as JSON.
    #[error("production target set could not be serialized")]
    TargetSerialization(#[source] serde_json::Error),
    /// Canonical target-set encoding failed.
    #[error("production target set could not be encoded canonically")]
    TargetDigest(#[source] CanonicalJsonError),
    /// Stored target-set identity did not match the complete target vector.
    #[error("production target-set identity does not match")]
    TargetSetIdentity,
    /// Stored LOC class did not match thin-entrypoint semantics.
    #[error("production LOC class does not match thin-entrypoint semantics")]
    LocClass,
}
