//! Strict semantic inputs supplied after review of the generated inventory.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use thiserror::Error;

use super::{P1ReviewIdentityError, P1ReviewInventory};
use crate::Digest;
use crate::baseline::{
    GovernanceAuthoringError, GovernanceTable, GovernanceToken, LegacyDisposition,
    LegacyGovernance, LegacyKind, LegacyState, OriginId, OriginLedger, P1GovernanceReview,
    ReviewedDebtGovernanceRow, ReviewedLocGovernanceRow,
};
use crate::phase_lock::CampaignPhase;
use crate::writers::classify::validate_classifications_for_operations;
use crate::writers::{
    ClassificationIssue, WriterClassification, WriterFamilyRegistry, WriterFamilyRegistryError,
    WriterOperationId, WriterToken,
};

const REVIEWED_INPUT_SCHEMA_VERSION: u32 = 1;

/// Validated semantic decisions required to author final P1 authorities.
pub struct P1ReviewedInputs {
    inventory_identity: Digest,
    base_commit: String,
    base_tree: String,
    origin_digest: Digest,
    owner_roles: Vec<GovernanceToken>,
    loc_exceptions: Vec<GovernanceMetadata>,
    debt_exceptions: Vec<GovernanceMetadata>,
    writer_registry: WriterFamilyRegistry,
}

impl P1ReviewedInputs {
    /// Decode a closed TOML review document and bind it to the exact generated
    /// review inventory.
    ///
    /// # Errors
    ///
    /// Rejects schema or inventory/base/origin drift, ambiguous owner vocabulary,
    /// incomplete governance metadata, or any missing, stale, duplicate, or
    /// structurally invalid per-occurrence writer classification.
    pub fn decode_p1(
        bytes: &[u8],
        inventory: &P1ReviewInventory,
    ) -> Result<Self, P1ReviewedInputError> {
        let text = std::str::from_utf8(bytes).map_err(P1ReviewedInputError::Utf8)?;
        let document: ReviewedInputDocument =
            toml::from_str(text).map_err(P1ReviewedInputError::Toml)?;
        if document.schema_version != REVIEWED_INPUT_SCHEMA_VERSION {
            return Err(P1ReviewedInputError::SchemaVersion);
        }
        let expected_identity = inventory
            .canonical_identity()
            .map_err(P1ReviewedInputError::GeneratedInventoryIdentity)?;
        if document.inventory_identity != expected_identity
            || document.base_commit.as_str() != inventory.base_commit()
            || document.base_tree.as_str() != inventory.base_tree()
            || document.origin_digest != inventory.origin_digest()
        {
            return Err(P1ReviewedInputError::InventoryBinding);
        }
        if !document
            .owner_roles
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        {
            return Err(P1ReviewedInputError::OwnerRoleOrder);
        }
        let used_roles = document
            .loc_exceptions
            .iter()
            .chain(&document.debt_exceptions)
            .map(|row| row.owner.clone())
            .collect::<BTreeSet<_>>();
        let declared_roles = document
            .owner_roles
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if used_roles != declared_roles {
            return Err(P1ReviewedInputError::OwnerRoleCoverage);
        }

        let expected_operations = inventory
            .writer_operations()
            .iter()
            .map(|row| WriterOperationId::new(row.operation_id));
        let issues = validate_classifications_for_operations(
            expected_operations,
            &document.writer_classifications,
        );
        if !issues.is_empty() {
            return Err(P1ReviewedInputError::WriterClassifications { issues });
        }
        let writer_registry = WriterFamilyRegistry::author_p1(
            document.writer_resolutions,
            document.writer_vocabulary.families,
            document.writer_vocabulary.shared_primitives,
            document.writer_vocabulary.cleanup_reviews,
            document.writer_vocabulary.false_positive_reviews,
            document.writer_classifications,
        )
        .map_err(P1ReviewedInputError::WriterRegistry)?;
        let reviewed = Self {
            inventory_identity: expected_identity,
            base_commit: inventory.base_commit().to_owned(),
            base_tree: inventory.base_tree().to_owned(),
            origin_digest: inventory.origin_digest(),
            owner_roles: document.owner_roles,
            loc_exceptions: document.loc_exceptions,
            debt_exceptions: document.debt_exceptions,
            writer_registry,
        };
        let ids = origin_ids(inventory)?;
        reviewed.validate_governance_inventory(inventory, &ids)?;
        Ok(reviewed)
    }

    /// Borrow the exact normalized owner-role vocabulary.
    #[must_use]
    pub fn owner_roles(&self) -> &[GovernanceToken] {
        &self.owner_roles
    }

    /// Return the exact generated review-inventory identity this value consumed.
    #[must_use]
    pub const fn inventory_identity(&self) -> Digest {
        self.inventory_identity
    }

    /// Borrow the validated writer-family authority.
    #[must_use]
    pub const fn writer_registry(&self) -> &WriterFamilyRegistry {
        &self.writer_registry
    }

    /// Author the initial reviewed governance anchor with every immutable base
    /// exception active.
    ///
    /// # Errors
    ///
    /// Rejects metadata that does not exactly cover the supplied origin.
    pub fn author_anchor_for_origin(
        &self,
        origin: &OriginLedger,
    ) -> Result<LegacyGovernance, P1ReviewedInputError> {
        self.validate_origin_binding(origin)?;
        self.author_with_states(origin, &BTreeMap::new(), true)
    }

    /// Author current governance from mechanically derived dispositions while
    /// preserving every reviewed metadata field.
    ///
    /// # Errors
    ///
    /// Rejects missing, extra, or wrong-family dispositions.
    pub fn author_current_for_origin(
        &self,
        origin: &OriginLedger,
        dispositions: &[LegacyDisposition],
    ) -> Result<LegacyGovernance, P1ReviewedInputError> {
        self.validate_origin_binding(origin)?;
        let states = dispositions
            .iter()
            .map(|row| (row.origin_id(), (row.kind(), row.state())))
            .collect::<BTreeMap<_, _>>();
        if states.len() != dispositions.len() {
            return Err(P1ReviewedInputError::GovernanceDisposition);
        }
        self.author_with_states(origin, &states, false)
    }

    fn validate_origin_binding(&self, origin: &OriginLedger) -> Result<(), P1ReviewedInputError> {
        let Ok(origin_digest) = origin.normalized_digest() else {
            return Err(P1ReviewedInputError::OriginBinding);
        };
        if origin.base_commit().as_str() != self.base_commit.as_str()
            || origin.base_tree().as_str() != self.base_tree.as_str()
            || origin_digest != self.origin_digest
        {
            return Err(P1ReviewedInputError::OriginBinding);
        }
        Ok(())
    }

    fn validate_governance_inventory(
        &self,
        inventory: &P1ReviewInventory,
        ids: &(BTreeSet<Digest>, BTreeSet<Digest>),
    ) -> Result<(), P1ReviewedInputError> {
        let actual_loc = self
            .loc_exceptions
            .iter()
            .map(|row| row.origin_id)
            .collect::<BTreeSet<_>>();
        let actual_debt = self
            .debt_exceptions
            .iter()
            .map(|row| row.origin_id)
            .collect::<BTreeSet<_>>();
        if actual_loc.len() != self.loc_exceptions.len() {
            return Err(P1ReviewedInputError::DuplicateGovernance {
                table: GovernanceTable::Loc,
            });
        }
        if actual_debt.len() != self.debt_exceptions.len() {
            return Err(P1ReviewedInputError::DuplicateGovernance {
                table: GovernanceTable::Debt,
            });
        }
        if actual_loc != ids.0 || actual_debt != ids.1 {
            return Err(P1ReviewedInputError::GovernanceCoverage);
        }
        if inventory.loc_exceptions().len() != actual_loc.len()
            || inventory.debt_exceptions().len() != actual_debt.len()
        {
            return Err(P1ReviewedInputError::GovernanceCoverage);
        }
        Ok(())
    }

    fn author_with_states(
        &self,
        origin: &OriginLedger,
        states: &BTreeMap<OriginId, (LegacyKind, LegacyState)>,
        anchor: bool,
    ) -> Result<LegacyGovernance, P1ReviewedInputError> {
        let loc = self
            .loc_exceptions
            .iter()
            .map(|row| {
                let state = state_for(row.origin_id, LegacyKind::ProductionLoc, states, anchor)?;
                Ok(ReviewedLocGovernanceRow::new(
                    OriginId::new(row.origin_id),
                    row.owner.clone(),
                    row.due_phase,
                    row.remediation_record.clone(),
                    state,
                ))
            })
            .collect::<Result<Vec<_>, P1ReviewedInputError>>()?;
        let debt = self
            .debt_exceptions
            .iter()
            .map(|row| {
                let state = state_for(row.origin_id, LegacyKind::ProhibitedDebt, states, anchor)?;
                Ok(ReviewedDebtGovernanceRow::new(
                    OriginId::new(row.origin_id),
                    row.owner.clone(),
                    row.due_phase,
                    row.remediation_record.clone(),
                    state,
                ))
            })
            .collect::<Result<Vec<_>, P1ReviewedInputError>>()?;
        if !anchor && states.len() != loc.len() + debt.len() {
            return Err(P1ReviewedInputError::GovernanceDisposition);
        }
        LegacyGovernance::author_p1(origin, P1GovernanceReview::new(loc, debt))
            .map_err(P1ReviewedInputError::Governance)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewedInputDocument {
    schema_version: u32,
    inventory_identity: Digest,
    base_commit: String,
    base_tree: String,
    origin_digest: Digest,
    owner_roles: Vec<GovernanceToken>,
    loc_exceptions: Vec<GovernanceMetadata>,
    debt_exceptions: Vec<GovernanceMetadata>,
    writer_resolutions: Digest,
    writer_vocabulary: ReviewedWriterVocabulary,
    writer_classifications: Vec<WriterClassification>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewedWriterVocabulary {
    families: Vec<WriterToken>,
    shared_primitives: Vec<WriterToken>,
    cleanup_reviews: Vec<WriterToken>,
    false_positive_reviews: Vec<WriterToken>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernanceMetadata {
    origin_id: Digest,
    owner: GovernanceToken,
    due_phase: CampaignPhase,
    remediation_record: GovernanceToken,
}

fn origin_ids(
    inventory: &P1ReviewInventory,
) -> Result<(BTreeSet<Digest>, BTreeSet<Digest>), P1ReviewedInputError> {
    let loc = inventory
        .loc_exceptions()
        .iter()
        .map(|row| row.origin_id)
        .collect::<BTreeSet<_>>();
    let debt = inventory
        .debt_exceptions()
        .iter()
        .map(|row| row.origin_id)
        .collect::<BTreeSet<_>>();
    if loc.len() != inventory.loc_exceptions().len()
        || debt.len() != inventory.debt_exceptions().len()
    {
        return Err(P1ReviewedInputError::GeneratedInventory);
    }
    Ok((loc, debt))
}

fn state_for(
    digest: Digest,
    expected_kind: LegacyKind,
    states: &BTreeMap<OriginId, (LegacyKind, LegacyState)>,
    anchor: bool,
) -> Result<LegacyState, P1ReviewedInputError> {
    if anchor {
        return Ok(LegacyState::Active);
    }
    let Some((kind, state)) = states.get(&OriginId::new(digest)) else {
        return Err(P1ReviewedInputError::GovernanceDisposition);
    };
    if *kind != expected_kind {
        return Err(P1ReviewedInputError::GovernanceDisposition);
    }
    Ok(*state)
}

/// Strict reviewed-input acquisition failure.
#[derive(Debug, Error)]
pub enum P1ReviewedInputError {
    /// Reviewed input bytes were not complete UTF-8.
    #[error("reviewed P1 input is not UTF-8")]
    Utf8(#[source] std::str::Utf8Error),
    /// Reviewed input did not satisfy the closed TOML grammar.
    #[error("reviewed P1 input is not valid closed-schema TOML")]
    Toml(#[source] toml::de::Error),
    /// The reviewed-input schema version is unsupported.
    #[error("reviewed P1 input schema is unsupported")]
    SchemaVersion,
    /// The generated inventory could not produce its normalized identity.
    #[error("generated P1 review inventory identity is invalid")]
    GeneratedInventoryIdentity(#[source] P1ReviewIdentityError),
    /// The reviewed document names a different inventory, base, or origin.
    #[error("reviewed P1 input does not bind the exact generated inventory")]
    InventoryBinding,
    /// Owner roles were duplicated or not strictly sorted.
    #[error("reviewed owner roles are not strictly sorted")]
    OwnerRoleOrder,
    /// Declared owner roles did not exactly equal roles used by governance rows.
    #[error("reviewed owner-role vocabulary does not exactly cover governance")]
    OwnerRoleCoverage,
    /// Governance metadata did not cover the generated exception inventory.
    #[error("reviewed governance does not exactly cover the generated inventory")]
    GovernanceCoverage,
    /// Reviewed governance repeated one immutable origin identity.
    #[error("reviewed governance contains a duplicate {table} origin identity")]
    DuplicateGovernance {
        /// Table containing the repeated identity.
        table: GovernanceTable,
    },
    /// The generated inventory itself contained duplicate governance identities.
    #[error("generated review inventory contains duplicate governance identities")]
    GeneratedInventory,
    /// Reviewed metadata could not construct exact origin-linked governance.
    #[error("reviewed governance could not be authored")]
    Governance(#[source] GovernanceAuthoringError),
    /// The supplied origin was not the exact inventory-bound origin reviewed.
    #[error("reviewed P1 input does not authorize this immutable origin")]
    OriginBinding,
    /// Derived current state did not cover the exact governance inventory.
    #[error("derived current governance dispositions are incomplete")]
    GovernanceDisposition,
    /// Writer classifications were missing, stale, duplicated, or structurally invalid.
    #[error("reviewed writer classifications do not exactly cover the generated inventory")]
    WriterClassifications {
        /// Complete sorted structural issues without source snippets.
        issues: Vec<ClassificationIssue>,
    },
    /// Reviewed classifications could not form the strict registry authority.
    #[error("reviewed writer-family authority is invalid")]
    WriterRegistry(#[source] WriterFamilyRegistryError),
}
