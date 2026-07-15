use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::digest::digest_bytes;
use crate::finding::EvidenceTraceabilityIssue;
use crate::strict_json::{StrictJsonError, decode_strict_json};

use super::model::{FixtureRegistration, OwnerPhase};

pub(super) const TRACEABILITY_PATH: &str = "docs/reviews/evidence/p1/finding-traceability.jsonl";
const TRACEABILITY_SHA256: &str =
    "190246d8738a41eb0f3afff7657de71c0d88eeb1bc871cce63d59714b30aa162";
const TRACEABILITY_ROW_COUNT: usize = 62;

pub(super) struct TraceabilityRegistry {
    rows: BTreeMap<String, TraceabilityLink>,
}

struct TraceabilityLink {
    owner_phase: OwnerPhase,
    planned_fixture_ids: Vec<String>,
}

pub(super) enum TraceabilityError {
    Utf8(std::str::Utf8Error),
    Json,
    Schema,
}

pub(super) enum TraceabilityAgreementError {
    Mismatch {
        issue: EvidenceTraceabilityIssue,
        count: u64,
    },
    Cardinality,
}

impl TraceabilityRegistry {
    pub(super) fn acquire(bytes: &[u8]) -> Result<Self, TraceabilityError> {
        let text = std::str::from_utf8(bytes).map_err(TraceabilityError::Utf8)?;
        if !text.ends_with('\n') || digest_bytes(bytes).to_string() != TRACEABILITY_SHA256 {
            return Err(TraceabilityError::Schema);
        }

        let mut decoded = Vec::new();
        let mut row_digests = BTreeSet::new();
        for line in text.lines() {
            if line.is_empty() || !row_digests.insert(digest_bytes(line.as_bytes())) {
                return Err(TraceabilityError::Schema);
            }
            let row = match decode_strict_json(line.as_bytes()) {
                Ok(row) => row,
                Err(StrictJsonError::Document { .. } | StrictJsonError::Schema { .. }) => {
                    return Err(TraceabilityError::Json);
                }
            };
            decoded.push(row);
        }
        if decoded.len() != TRACEABILITY_ROW_COUNT || !inventory_matches(&decoded) {
            return Err(TraceabilityError::Schema);
        }

        let mut rows = BTreeMap::new();
        let mut evidence_ids = BTreeSet::new();
        let mut fixture_ids = BTreeSet::new();
        for row in decoded {
            if !row.is_valid()
                || !evidence_ids.insert(row.planned_evidence_id.clone())
                || row
                    .planned_fixture_ids
                    .iter()
                    .any(|id| !fixture_ids.insert(id.clone()))
            {
                return Err(TraceabilityError::Schema);
            }
            let link = TraceabilityLink {
                owner_phase: row.owner_phase,
                planned_fixture_ids: row.planned_fixture_ids,
            };
            if rows.insert(row.finding_id, link).is_some() {
                return Err(TraceabilityError::Schema);
            }
        }
        Ok(Self { rows })
    }

    pub(super) fn verify_fixtures<'a>(
        &self,
        fixtures: impl Iterator<Item = &'a FixtureRegistration>,
    ) -> Result<(), TraceabilityAgreementError> {
        let mut observed = BTreeMap::<&str, BTreeSet<&str>>::new();
        let mut findings_missing = 0_u64;
        let mut source_mismatches = 0_u64;
        for fixture in fixtures {
            for finding_id in &fixture.finding_ids {
                observed
                    .entry(finding_id.as_str())
                    .or_default()
                    .insert(fixture.id.as_str());
                let Some(row) = self.rows.get(finding_id) else {
                    increment(&mut findings_missing)?;
                    continue;
                };
                if row.owner_phase != fixture.owner_phase
                    || !row
                        .planned_fixture_ids
                        .iter()
                        .any(|fixture_id| fixture_id == &fixture.id)
                {
                    increment(&mut source_mismatches)?;
                }
            }
        }
        if findings_missing != 0 {
            return Err(TraceabilityAgreementError::Mismatch {
                issue: EvidenceTraceabilityIssue::FindingMissing,
                count: findings_missing,
            });
        }
        if source_mismatches != 0 {
            return Err(TraceabilityAgreementError::Mismatch {
                issue: EvidenceTraceabilityIssue::SourceMismatch,
                count: source_mismatches,
            });
        }

        let mut evidence_missing = 0_u64;
        for (finding_id, row) in &self.rows {
            let observed = observed.get(finding_id.as_str());
            for fixture_id in &row.planned_fixture_ids {
                if !observed.is_some_and(|ids| ids.contains(fixture_id.as_str())) {
                    increment(&mut evidence_missing)?;
                }
            }
        }
        if evidence_missing != 0 {
            return Err(TraceabilityAgreementError::Mismatch {
                issue: EvidenceTraceabilityIssue::EvidenceMissing,
                count: evidence_missing,
            });
        }
        Ok(())
    }
}

fn increment(value: &mut u64) -> Result<(), TraceabilityAgreementError> {
    *value = value
        .checked_add(1)
        .ok_or(TraceabilityAgreementError::Cardinality)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceabilityRow {
    finding_id: String,
    source_severity: SourceSeverity,
    owner_phase: OwnerPhase,
    evidence_class: EvidenceClass,
    fixture_category: String,
    current_seams: Vec<String>,
    closure_status: ClosureStatus,
    expectation_class: TraceExpectation,
    evidence_method: EvidenceMethod,
    planned_evidence_id: String,
    source_evidence: String,
    target_assertion: String,
    fixture_applicability: FixtureApplicability,
    planned_fixture_ids: Vec<String>,
}

impl TraceabilityRow {
    fn is_valid(&self) -> bool {
        let strings_present = meaningful(&self.finding_id)
            && meaningful(&self.fixture_category)
            && nonempty_meaningful(&self.current_seams)
            && meaningful(&self.planned_evidence_id)
            && meaningful(&self.source_evidence)
            && meaningful(&self.target_assertion)
            && unique_meaningful(&self.planned_fixture_ids);
        strings_present
            && match self.closure_status {
                ClosureStatus::AcceptedP0 => {
                    self.owner_phase == OwnerPhase::P0
                        && self.expectation_class == TraceExpectation::AcceptedEvidence
                        && self.fixture_applicability
                            == FixtureApplicability::NotApplicableAcceptedP0
                        && self.planned_fixture_ids.is_empty()
                        && matches!(
                            (self.evidence_class, self.evidence_method),
                            (
                                EvidenceClass::AcceptedLimitation,
                                EvidenceMethod::AcceptedLimitation
                            ) | (
                                EvidenceClass::ConfirmedDefect | EvidenceClass::GateFinding,
                                EvidenceMethod::AcceptedPhaseEvidence
                            )
                        )
                }
                ClosureStatus::Open => {
                    self.owner_phase != OwnerPhase::P0
                        && self.fixture_applicability == FixtureApplicability::Planned
                        && self.planned_fixture_ids.len() == 1
                        && matches!(
                            (
                                self.evidence_class,
                                self.expectation_class,
                                self.evidence_method
                            ),
                            (
                                EvidenceClass::ConfirmedDefect,
                                TraceExpectation::BaselineRed,
                                EvidenceMethod::DefectRegression
                            ) | (
                                EvidenceClass::Design,
                                TraceExpectation::ContractTarget,
                                EvidenceMethod::DesignContract
                            ) | (
                                EvidenceClass::Enhancement,
                                TraceExpectation::ContractTarget,
                                EvidenceMethod::EnhancementContract
                            ) | (
                                EvidenceClass::Measurement,
                                TraceExpectation::ContractTarget,
                                EvidenceMethod::MeasurementExperiment
                            )
                        )
                }
            }
    }
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SourceSeverity {
    Critical,
    Design,
    Enhancement,
    High,
    Informational,
    Low,
    LowMedium,
    Medium,
    MediumGate,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum EvidenceClass {
    AcceptedLimitation,
    ConfirmedDefect,
    Design,
    Enhancement,
    GateFinding,
    Measurement,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ClosureStatus {
    AcceptedP0,
    Open,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum TraceExpectation {
    AcceptedEvidence,
    BaselineRed,
    ContractTarget,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum EvidenceMethod {
    AcceptedLimitation,
    AcceptedPhaseEvidence,
    DefectRegression,
    DesignContract,
    EnhancementContract,
    MeasurementExperiment,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum FixtureApplicability {
    NotApplicableAcceptedP0,
    Planned,
}

fn inventory_matches(rows: &[TraceabilityRow]) -> bool {
    let mut owners = [0_usize; 10];
    let mut severities = [0_usize; 9];
    let mut evidence = [0_usize; 6];
    let mut closures = [0_usize; 2];
    let mut expectations = [0_usize; 3];
    let mut methods = [0_usize; 6];
    let mut applicability = [0_usize; 2];
    for row in rows {
        owners[owner_index(row.owner_phase)] += 1;
        severities[severity_index(row.source_severity)] += 1;
        evidence[evidence_index(row.evidence_class)] += 1;
        closures[closure_index(row.closure_status)] += 1;
        expectations[expectation_index(row.expectation_class)] += 1;
        methods[method_index(row.evidence_method)] += 1;
        applicability[applicability_index(row.fixture_applicability)] += 1;
    }
    owners == [23, 0, 9, 0, 8, 6, 5, 6, 5, 0]
        && severities == [5, 1, 1, 25, 1, 2, 2, 24, 1]
        && evidence == [1, 55, 2, 1, 2, 1]
        && closures == [23, 39]
        && expectations == [23, 35, 4]
        && methods == [1, 22, 35, 2, 1, 1]
        && applicability == [23, 39]
}

const fn owner_index(value: OwnerPhase) -> usize {
    match value {
        OwnerPhase::P0 => 0,
        OwnerPhase::P1 => 1,
        OwnerPhase::P2 => 2,
        OwnerPhase::P3 => 3,
        OwnerPhase::P4 => 4,
        OwnerPhase::P5 => 5,
        OwnerPhase::P6 => 6,
        OwnerPhase::P7 => 7,
        OwnerPhase::P8 => 8,
        OwnerPhase::P9 => 9,
    }
}

const fn severity_index(value: SourceSeverity) -> usize {
    match value {
        SourceSeverity::Critical => 0,
        SourceSeverity::Design => 1,
        SourceSeverity::Enhancement => 2,
        SourceSeverity::High => 3,
        SourceSeverity::Informational => 4,
        SourceSeverity::Low => 5,
        SourceSeverity::LowMedium => 6,
        SourceSeverity::Medium => 7,
        SourceSeverity::MediumGate => 8,
    }
}

const fn evidence_index(value: EvidenceClass) -> usize {
    match value {
        EvidenceClass::AcceptedLimitation => 0,
        EvidenceClass::ConfirmedDefect => 1,
        EvidenceClass::Design => 2,
        EvidenceClass::Enhancement => 3,
        EvidenceClass::GateFinding => 4,
        EvidenceClass::Measurement => 5,
    }
}

const fn closure_index(value: ClosureStatus) -> usize {
    match value {
        ClosureStatus::AcceptedP0 => 0,
        ClosureStatus::Open => 1,
    }
}

const fn expectation_index(value: TraceExpectation) -> usize {
    match value {
        TraceExpectation::AcceptedEvidence => 0,
        TraceExpectation::BaselineRed => 1,
        TraceExpectation::ContractTarget => 2,
    }
}

const fn method_index(value: EvidenceMethod) -> usize {
    match value {
        EvidenceMethod::AcceptedLimitation => 0,
        EvidenceMethod::AcceptedPhaseEvidence => 1,
        EvidenceMethod::DefectRegression => 2,
        EvidenceMethod::DesignContract => 3,
        EvidenceMethod::EnhancementContract => 4,
        EvidenceMethod::MeasurementExperiment => 5,
    }
}

const fn applicability_index(value: FixtureApplicability) -> usize {
    match value {
        FixtureApplicability::NotApplicableAcceptedP0 => 0,
        FixtureApplicability::Planned => 1,
    }
}

fn meaningful(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

fn nonempty_meaningful(values: &[String]) -> bool {
    !values.is_empty() && values.iter().all(|value| meaningful(value))
}

fn unique_meaningful(values: &[String]) -> bool {
    values.iter().all(|value| meaningful(value))
        && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}
