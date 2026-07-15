//! Strict reviewed governance over immutable origin identities.

mod anchor;
mod authoring;
mod transition;

pub use anchor::{GovernanceAnchorError, P1_GOVERNANCE_ANCHOR_IDENTITY, ReviewedGovernanceAnchor};
pub use authoring::{
    GovernanceAuthoringError, P1GovernanceReview, ReviewedDebtGovernanceRow,
    ReviewedLocGovernanceRow,
};
pub use transition::GovernanceTransitionError;

use std::collections::{BTreeMap, BTreeSet};
use std::str::Utf8Error;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::evaluate::LocCeilings;
use super::model::{OriginId, OriginLedger};
use super::production::ProductionFileFact;
use super::token::GovernanceToken;
use crate::digest::{CanonicalJsonError, Digest, digest_json};
use crate::phase_lock::CampaignPhase;
use crate::version::{ANALYZER_VERSION, DIGEST_VERSION};

const GOVERNANCE_SCHEMA_VERSION: u32 = 1;

/// Persistent reviewed state for one immutable legacy exception.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyState {
    /// The exact unchanged origin exception remains temporarily active.
    Active,
    /// The exception has been removed or brought within the hard limit.
    Resolved,
}

/// Reviewed metadata for one legacy LOC or prohibited-debt origin fact.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyGovernanceEntry {
    origin_id: OriginId,
    owner: GovernanceToken,
    due_phase: CampaignPhase,
    remediation_record: GovernanceToken,
    state: LegacyState,
}

impl LegacyGovernanceEntry {
    /// Return the referenced immutable origin identity.
    #[must_use]
    pub const fn origin_id(&self) -> OriginId {
        self.origin_id
    }

    /// Return the reviewed owner token.
    #[must_use]
    pub const fn owner(&self) -> &GovernanceToken {
        &self.owner
    }

    /// Return the phase at which an active exception stops being valid.
    #[must_use]
    pub const fn due_phase(&self) -> CampaignPhase {
        self.due_phase
    }

    /// Return the machine remediation-record token.
    #[must_use]
    pub const fn remediation_record(&self) -> &GovernanceToken {
        &self.remediation_record
    }

    /// Return the persistent monotonic exception state.
    #[must_use]
    pub const fn state(&self) -> LegacyState {
        self.state
    }
}

/// Strict normalized human-governance document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LegacyGovernance {
    schema_version: u32,
    algorithms: GovernanceAlgorithms,
    loc_exceptions: Vec<LegacyGovernanceEntry>,
    debt_exceptions: Vec<LegacyGovernanceEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GovernanceAlgorithms {
    analyzer: String,
    digest: String,
}

impl LegacyGovernance {
    /// Decode strict closed-schema TOML governance.
    ///
    /// # Errors
    ///
    /// Rejects non-UTF-8, malformed, duplicate, unknown, unsorted, repeated,
    /// or algorithm-incompatible governance data.
    pub fn decode(bytes: &[u8]) -> Result<Self, GovernanceError> {
        let text = std::str::from_utf8(bytes).map_err(GovernanceError::Utf8)?;
        let document: GovernanceDocument = toml::from_str(text).map_err(GovernanceError::Toml)?;
        document.validate()
    }

    /// Borrow LOC exception metadata in immutable-origin order.
    #[must_use]
    pub fn loc_exceptions(&self) -> &[LegacyGovernanceEntry] {
        &self.loc_exceptions
    }

    /// Borrow prohibited-debt exception metadata in immutable-origin order.
    #[must_use]
    pub fn debt_exceptions(&self) -> &[LegacyGovernanceEntry] {
        &self.debt_exceptions
    }

    /// Validate exact legacy-exception references and coverage.
    ///
    /// # Errors
    ///
    /// Rejects missing, stale, or wrong-family legacy references.
    pub fn validate_against(
        &self,
        origin: &OriginLedger,
        limits: LocCeilings,
    ) -> Result<(), GovernanceLinkError> {
        validate_legacy_links(origin, limits, self)
    }

    /// Hash normalized governance rather than TOML formatting.
    ///
    /// # Errors
    ///
    /// Returns an error only if serialization or canonical encoding fails.
    pub fn normalized_digest(&self) -> Result<Digest, GovernanceDigestError> {
        let value = serde_json::to_value(self).map_err(GovernanceDigestError::Serialization)?;
        digest_json(&value).map_err(GovernanceDigestError::Canonical)
    }

    pub(crate) fn loc_map(&self) -> BTreeMap<OriginId, &LegacyGovernanceEntry> {
        self.loc_exceptions
            .iter()
            .map(|entry| (entry.origin_id, entry))
            .collect()
    }

    pub(crate) fn debt_map(&self) -> BTreeMap<OriginId, &LegacyGovernanceEntry> {
        self.debt_exceptions
            .iter()
            .map(|entry| (entry.origin_id, entry))
            .collect()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernanceDocument {
    schema_version: u32,
    algorithms: GovernanceAlgorithms,
    loc_exceptions: Vec<LegacyGovernanceEntry>,
    debt_exceptions: Vec<LegacyGovernanceEntry>,
}

impl GovernanceDocument {
    fn validate(self) -> Result<LegacyGovernance, GovernanceError> {
        if self.schema_version != GOVERNANCE_SCHEMA_VERSION {
            return Err(GovernanceError::SchemaVersion {
                actual: self.schema_version,
            });
        }
        if self.algorithms.analyzer != ANALYZER_VERSION {
            return Err(GovernanceError::AnalyzerVersion);
        }
        if self.algorithms.digest != DIGEST_VERSION {
            return Err(GovernanceError::DigestVersion);
        }
        validate_legacy_order(GovernanceTable::Loc, &self.loc_exceptions)?;
        validate_legacy_order(GovernanceTable::Debt, &self.debt_exceptions)?;
        Ok(LegacyGovernance {
            schema_version: self.schema_version,
            algorithms: self.algorithms,
            loc_exceptions: self.loc_exceptions,
            debt_exceptions: self.debt_exceptions,
        })
    }
}

fn validate_legacy_order(
    table: GovernanceTable,
    entries: &[LegacyGovernanceEntry],
) -> Result<(), GovernanceError> {
    for (index, pair) in entries.windows(2).enumerate() {
        if pair[0].origin_id >= pair[1].origin_id {
            return Err(GovernanceError::Order {
                table,
                index: index + 1,
            });
        }
    }
    Ok(())
}

fn validate_legacy_links(
    origin: &OriginLedger,
    limits: LocCeilings,
    governance: &LegacyGovernance,
) -> Result<(), GovernanceLinkError> {
    let expected_loc: BTreeSet<OriginId> = origin
        .production_files()
        .iter()
        .filter(|fact| limits.exceeded(fact))
        .map(ProductionFileFact::origin_id)
        .collect();
    let actual_loc: BTreeSet<OriginId> = governance
        .loc_exceptions
        .iter()
        .map(|entry| entry.origin_id)
        .collect();
    if expected_loc != actual_loc {
        return Err(GovernanceLinkError::LocCoverage);
    }

    let expected_debt: BTreeSet<OriginId> = origin
        .prohibited_debt()
        .iter()
        .map(super::model::DebtOriginFact::origin_id)
        .collect();
    let actual_debt: BTreeSet<OriginId> = governance
        .debt_exceptions
        .iter()
        .map(|entry| entry.origin_id)
        .collect();
    if expected_debt != actual_debt {
        return Err(GovernanceLinkError::DebtCoverage);
    }
    Ok(())
}

/// Governance table classes used by structural failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GovernanceTable {
    /// Legacy over-limit file metadata.
    #[error("loc_exceptions")]
    Loc,
    /// Legacy prohibited-debt metadata.
    #[error("debt_exceptions")]
    Debt,
}

/// Strict governance decoding failure.
#[derive(Debug, Error)]
pub enum GovernanceError {
    /// Bytes are not UTF-8.
    #[error("legacy governance is not UTF-8")]
    Utf8(#[source] Utf8Error),
    /// TOML is malformed, duplicate, unknown, or type-invalid.
    #[error("legacy governance is not valid closed-schema TOML")]
    Toml(#[source] toml::de::Error),
    /// Schema version is unsupported.
    #[error("legacy governance schema version {actual} is unsupported")]
    SchemaVersion {
        /// Observed schema version.
        actual: u32,
    },
    /// Analyzer identity differs from the evaluator.
    #[error("legacy governance analyzer identity does not match")]
    AnalyzerVersion,
    /// Digest identity differs from the evaluator.
    #[error("legacy governance digest identity does not match")]
    DigestVersion,
    /// A table is unsorted or contains a duplicate origin ID.
    #[error("legacy governance {table} is not strictly sorted at row {index}")]
    Order {
        /// Invalid table.
        table: GovernanceTable,
        /// First invalid row.
        index: usize,
    },
}

/// Governance-to-origin integrity failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GovernanceLinkError {
    /// Legacy over-limit governance is missing or stale.
    #[error("legacy LOC governance does not exactly cover origin exceptions")]
    LocCoverage,
    /// Prohibited-debt governance is missing or stale.
    #[error("legacy debt governance does not exactly cover origin exceptions")]
    DebtCoverage,
}

/// Normalized-governance digest failure.
#[derive(Debug, Error)]
pub enum GovernanceDigestError {
    /// The closed Rust value could not be represented as JSON.
    #[error("governance value could not be serialized")]
    Serialization(#[source] serde_json::Error),
    /// Canonical JSON encoding failed.
    #[error("governance value could not be encoded canonically")]
    Canonical(#[source] CanonicalJsonError),
}
