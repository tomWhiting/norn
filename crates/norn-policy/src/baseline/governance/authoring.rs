//! Deterministic construction of reviewed P1 legacy governance.

use thiserror::Error;

use super::{
    GOVERNANCE_SCHEMA_VERSION, GovernanceAlgorithms, GovernanceLinkError, GovernanceTable,
    LegacyGovernance, LegacyGovernanceEntry, LegacyState,
};
use crate::baseline::{LocCeilings, OriginId, OriginLedger};
use crate::phase_lock::CampaignPhase;
use crate::version::{ANALYZER_VERSION, DIGEST_VERSION};

use super::super::token::GovernanceToken;

/// One explicitly reviewed LOC-exception row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewedLocGovernanceRow {
    entry: LegacyGovernanceEntry,
}

impl ReviewedLocGovernanceRow {
    /// Construct a LOC review row without implicit metadata.
    #[must_use]
    pub const fn new(
        origin_id: OriginId,
        owner: GovernanceToken,
        due_phase: CampaignPhase,
        remediation_record: GovernanceToken,
        state: LegacyState,
    ) -> Self {
        Self {
            entry: LegacyGovernanceEntry {
                origin_id,
                owner,
                due_phase,
                remediation_record,
                state,
            },
        }
    }
}

/// One explicitly reviewed prohibited-debt row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewedDebtGovernanceRow {
    entry: LegacyGovernanceEntry,
}

impl ReviewedDebtGovernanceRow {
    /// Construct a prohibited-debt review row without implicit metadata.
    #[must_use]
    pub const fn new(
        origin_id: OriginId,
        owner: GovernanceToken,
        due_phase: CampaignPhase,
        remediation_record: GovernanceToken,
        state: LegacyState,
    ) -> Self {
        Self {
            entry: LegacyGovernanceEntry {
                origin_id,
                owner,
                due_phase,
                remediation_record,
                state,
            },
        }
    }
}

/// Complete reviewed input required to author P1 legacy governance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct P1GovernanceReview {
    loc_exceptions: Vec<ReviewedLocGovernanceRow>,
    debt_exceptions: Vec<ReviewedDebtGovernanceRow>,
}

impl P1GovernanceReview {
    /// Construct the complete review input without default rows or metadata.
    #[must_use]
    pub const fn new(
        loc_exceptions: Vec<ReviewedLocGovernanceRow>,
        debt_exceptions: Vec<ReviewedDebtGovernanceRow>,
    ) -> Self {
        Self {
            loc_exceptions,
            debt_exceptions,
        }
    }
}

impl LegacyGovernance {
    /// Author normalized P1 governance from explicit reviewed metadata.
    ///
    /// # Errors
    ///
    /// Rejects duplicate identities and anything other than exact LOC and debt
    /// coverage of the immutable origin under the compiled P1 baseline.
    pub fn author_p1(
        origin: &OriginLedger,
        review: P1GovernanceReview,
    ) -> Result<Self, GovernanceAuthoringError> {
        let mut loc_exceptions = review
            .loc_exceptions
            .into_iter()
            .map(|row| row.entry)
            .collect::<Vec<_>>();
        let mut debt_exceptions = review
            .debt_exceptions
            .into_iter()
            .map(|row| row.entry)
            .collect::<Vec<_>>();
        loc_exceptions.sort_unstable();
        debt_exceptions.sort_unstable();
        reject_duplicate_ids(GovernanceTable::Loc, &loc_exceptions)?;
        reject_duplicate_ids(GovernanceTable::Debt, &debt_exceptions)?;

        let governance = Self {
            schema_version: GOVERNANCE_SCHEMA_VERSION,
            algorithms: GovernanceAlgorithms {
                analyzer: ANALYZER_VERSION.to_owned(),
                digest: DIGEST_VERSION.to_owned(),
            },
            loc_exceptions,
            debt_exceptions,
        };
        governance.validate_against(origin, LocCeilings::p1_baseline())?;
        Ok(governance)
    }

    /// Encode the normalized value as byte-stable TOML.
    ///
    /// The returned document always has exactly one trailing newline.
    #[must_use]
    pub fn encode_canonical_toml(&self) -> Vec<u8> {
        let mut output = String::from("schema_version = 1\n");
        append_empty_tables(&mut output, self);
        output.push_str("\n[algorithms]\n");
        output.push_str("analyzer = \"");
        output.push_str(ANALYZER_VERSION);
        output.push_str("\"\ndigest = \"");
        output.push_str(DIGEST_VERSION);
        output.push_str("\"\n");
        append_entries(&mut output, GovernanceTable::Loc, &self.loc_exceptions);
        append_entries(&mut output, GovernanceTable::Debt, &self.debt_exceptions);
        output.into_bytes()
    }
}

fn reject_duplicate_ids(
    table: GovernanceTable,
    entries: &[LegacyGovernanceEntry],
) -> Result<(), GovernanceAuthoringError> {
    if entries
        .windows(2)
        .any(|pair| pair[0].origin_id == pair[1].origin_id)
    {
        return Err(GovernanceAuthoringError::DuplicateOrigin { table });
    }
    Ok(())
}

fn append_empty_tables(output: &mut String, governance: &LegacyGovernance) {
    if governance.loc_exceptions.is_empty() {
        output.push_str("loc_exceptions = []\n");
    }
    if governance.debt_exceptions.is_empty() {
        output.push_str("debt_exceptions = []\n");
    }
}

fn append_entries(output: &mut String, table: GovernanceTable, entries: &[LegacyGovernanceEntry]) {
    for entry in entries {
        output.push_str("\n[[");
        output.push_str(match table {
            GovernanceTable::Loc => "loc_exceptions",
            GovernanceTable::Debt => "debt_exceptions",
        });
        output.push_str("]]\norigin_id = \"");
        output.push_str(&entry.origin_id.digest().to_string());
        output.push_str("\"\nowner = \"");
        output.push_str(entry.owner.as_str());
        output.push_str("\"\ndue_phase = \"");
        output.push_str(phase_token(entry.due_phase));
        output.push_str("\"\nremediation_record = \"");
        output.push_str(entry.remediation_record.as_str());
        output.push_str("\"\nstate = \"");
        output.push_str(state_token(entry.state));
        output.push_str("\"\n");
    }
}

const fn phase_token(phase: CampaignPhase) -> &'static str {
    match phase {
        CampaignPhase::P1 => "P1",
        CampaignPhase::P2 => "P2",
        CampaignPhase::P3 => "P3",
        CampaignPhase::P4 => "P4",
        CampaignPhase::P5 => "P5",
        CampaignPhase::P6 => "P6",
        CampaignPhase::P7 => "P7",
        CampaignPhase::P8 => "P8",
        CampaignPhase::P9 => "P9",
    }
}

const fn state_token(state: LegacyState) -> &'static str {
    match state {
        LegacyState::Active => "active",
        LegacyState::Resolved => "resolved",
    }
}

/// Failure to turn reviewed rows into exact P1 governance.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GovernanceAuthoringError {
    /// A reviewed family names one immutable origin more than once.
    #[error("reviewed governance contains a duplicate {table} origin identity")]
    DuplicateOrigin {
        /// The table containing the repeated identity.
        table: GovernanceTable,
    },
    /// The reviewed rows do not exactly cover immutable P1 exceptions.
    #[error("reviewed governance does not exactly cover immutable P1 exceptions")]
    OriginLink(#[from] GovernanceLinkError),
}
