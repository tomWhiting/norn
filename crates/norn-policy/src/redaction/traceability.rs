use std::collections::BTreeSet;

use serde::Deserialize;

use crate::digest::{Digest, digest_bytes};
use crate::strict_json::decode_strict_json;

use super::model::ArtifactRegistration;
use super::scan::decoded_structural_violation;
use super::validate::{ArtifactIssue, RedactionCode};

const TRACEABILITY_PATH: &str = "docs/reviews/evidence/p1/finding-traceability.jsonl";
const TRACEABILITY_SHA256: &str =
    "190246d8738a41eb0f3afff7657de71c0d88eeb1bc871cce63d59714b30aa162";
const EXPECTED_FINDING_IDS: &[&str] = &[
    "SEC-01",
    "SEC-02",
    "SEC-07",
    "SEC-12",
    "SEC-13",
    "SEC-03",
    "SEC-04",
    "SEC-05",
    "SEC-09",
    "SEC-11",
    "SEC-14",
    "SEC-15",
    "SEC-16",
    "STATE-01",
    "STATE-02",
    "STATE-03",
    "ROLE-01",
    "EVT-01",
    "EVT-02",
    "EVT-03",
    "EVT-04",
    "EVT-06",
    "EVT-07",
    "REQ-01",
    "CODEX-01",
    "CODEX-02",
    "TRANS-01",
    "TRANS-02",
    "CACHE-01",
    "CACHE-02",
    "EVT-05",
    "CACHE-03",
    "CACHE-04",
    "CACHE-05",
    "MODEL-01",
    "ROLE-02",
    "TOOL-01",
    "BACKEND-01",
    "BACKEND-02",
    "CONFIG-01",
    "SEC-06",
    "SEC-08",
    "SEC-08A",
    "SEC-10",
    "SCHEMA-01",
    "USAGE-01",
    "AUTH-01",
    "AUTH-02",
    "AUTH-03",
    "AUTH-04",
    "AUTH-05",
    "AUTH-06",
    "AUTH-07",
    "CONFIG-02",
    "NF-1",
    "NF-2",
    "NF-3",
    "NF-4",
    "NF-5",
    "QUAL-01",
    "ROUTE-01",
    "STRUCT-01",
];

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
enum OwnerPhase {
    P0,
    P2,
    P4,
    P5,
    P6,
    P7,
    P8,
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
enum ExpectationClass {
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
    expectation_class: ExpectationClass,
    evidence_method: EvidenceMethod,
    planned_evidence_id: String,
    source_evidence: String,
    target_assertion: String,
    fixture_applicability: FixtureApplicability,
    planned_fixture_ids: Vec<String>,
}

#[derive(Clone, Copy, Deserialize)]
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

pub(crate) const fn authority() -> (&'static str, &'static str) {
    (TRACEABILITY_PATH, TRACEABILITY_SHA256)
}

pub(crate) fn validate_traceability_document(
    registration: &ArtifactRegistration,
    bytes: &[u8],
    issues: &mut Vec<ArtifactIssue>,
) {
    if registration.path().as_str() != TRACEABILITY_PATH {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        issues.push(issue(RedactionCode::InvalidJsonl));
        return;
    };
    if !text.ends_with('\n') {
        issues.push(issue(RedactionCode::InvalidJsonl));
    }

    let mut rows = Vec::new();
    let mut row_digests = BTreeSet::<Digest>::new();
    for line in text.lines() {
        if line.is_empty() {
            issues.push(issue(RedactionCode::InvalidJsonl));
            continue;
        }
        if !row_digests.insert(digest_bytes(line.as_bytes())) {
            issues.push(issue(RedactionCode::DuplicateJsonlRow));
        }
        match decode_strict_json::<TraceabilityRow>(line.as_bytes()) {
            Ok(row) => rows.push(row),
            Err(_) => issues.push(issue(RedactionCode::InvalidJsonl)),
        }
    }
    validate_rows(&rows, issues);
    if digest_bytes(bytes).to_hex() != TRACEABILITY_SHA256 {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
}

fn validate_rows(rows: &[TraceabilityRow], issues: &mut Vec<ArtifactIssue>) {
    if rows.len() != EXPECTED_FINDING_IDS.len() {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
    let mut finding_ids = BTreeSet::new();
    let mut evidence_ids = BTreeSet::new();
    let mut fixture_ids = BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        let expected_id = EXPECTED_FINDING_IDS.get(index).copied();
        if expected_id != Some(row.finding_id.as_str()) || !row_is_valid(row) {
            issues.push(issue(RedactionCode::SchemaMismatch));
        }
        if !finding_ids.insert(row.finding_id.as_str())
            || !evidence_ids.insert(row.planned_evidence_id.as_str())
        {
            issues.push(issue(RedactionCode::DuplicateJsonlRow));
        }
        for fixture_id in &row.planned_fixture_ids {
            if !fixture_ids.insert(fixture_id.as_str()) {
                issues.push(issue(RedactionCode::DuplicateJsonlRow));
            }
        }
        scan_row(row, issues);
    }
    if !inventory_totals_match(rows) {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
}

fn row_is_valid(row: &TraceabilityRow) -> bool {
    let strings_present = !row.fixture_category.is_empty()
        && !row.current_seams.is_empty()
        && !row.planned_evidence_id.is_empty()
        && !row.source_evidence.is_empty()
        && !row.target_assertion.is_empty();
    if !strings_present {
        return false;
    }
    match row.closure_status {
        ClosureStatus::AcceptedP0 => {
            row.owner_phase == OwnerPhase::P0
                && row.expectation_class == ExpectationClass::AcceptedEvidence
                && row.fixture_applicability == FixtureApplicability::NotApplicableAcceptedP0
                && row.planned_fixture_ids.is_empty()
                && matches!(
                    (row.evidence_class, row.evidence_method),
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
            row.owner_phase != OwnerPhase::P0
                && row.fixture_applicability == FixtureApplicability::Planned
                && row.planned_fixture_ids.len() == 1
                && matches!(
                    (
                        row.evidence_class,
                        row.expectation_class,
                        row.evidence_method
                    ),
                    (
                        EvidenceClass::ConfirmedDefect,
                        ExpectationClass::BaselineRed,
                        EvidenceMethod::DefectRegression
                    ) | (
                        EvidenceClass::Design,
                        ExpectationClass::ContractTarget,
                        EvidenceMethod::DesignContract
                    ) | (
                        EvidenceClass::Enhancement,
                        ExpectationClass::ContractTarget,
                        EvidenceMethod::EnhancementContract
                    ) | (
                        EvidenceClass::Measurement,
                        ExpectationClass::ContractTarget,
                        EvidenceMethod::MeasurementExperiment
                    )
                )
        }
    }
}

fn inventory_totals_match(rows: &[TraceabilityRow]) -> bool {
    let mut owners = [0_usize; 7];
    let mut severities = [0_usize; 9];
    let mut evidence_classes = [0_usize; 6];
    let mut closure_statuses = [0_usize; 2];
    let mut expectations = [0_usize; 3];
    let mut methods = [0_usize; 6];
    let mut applicability = [0_usize; 2];

    for row in rows {
        owners[owner_index(row.owner_phase)] += 1;
        severities[severity_index(row.source_severity)] += 1;
        evidence_classes[evidence_class_index(row.evidence_class)] += 1;
        closure_statuses[closure_status_index(row.closure_status)] += 1;
        expectations[expectation_index(row.expectation_class)] += 1;
        methods[evidence_method_index(row.evidence_method)] += 1;
        applicability[applicability_index(row.fixture_applicability)] += 1;
    }

    owners == [23, 9, 8, 6, 5, 6, 5]
        && severities == [5, 1, 1, 25, 1, 2, 2, 24, 1]
        && evidence_classes == [1, 55, 2, 1, 2, 1]
        && closure_statuses == [23, 39]
        && expectations == [23, 35, 4]
        && methods == [1, 22, 35, 2, 1, 1]
        && applicability == [23, 39]
}

const fn owner_index(value: OwnerPhase) -> usize {
    match value {
        OwnerPhase::P0 => 0,
        OwnerPhase::P2 => 1,
        OwnerPhase::P4 => 2,
        OwnerPhase::P5 => 3,
        OwnerPhase::P6 => 4,
        OwnerPhase::P7 => 5,
        OwnerPhase::P8 => 6,
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

const fn evidence_class_index(value: EvidenceClass) -> usize {
    match value {
        EvidenceClass::AcceptedLimitation => 0,
        EvidenceClass::ConfirmedDefect => 1,
        EvidenceClass::Design => 2,
        EvidenceClass::Enhancement => 3,
        EvidenceClass::GateFinding => 4,
        EvidenceClass::Measurement => 5,
    }
}

const fn closure_status_index(value: ClosureStatus) -> usize {
    match value {
        ClosureStatus::AcceptedP0 => 0,
        ClosureStatus::Open => 1,
    }
}

const fn expectation_index(value: ExpectationClass) -> usize {
    match value {
        ExpectationClass::AcceptedEvidence => 0,
        ExpectationClass::BaselineRed => 1,
        ExpectationClass::ContractTarget => 2,
    }
}

const fn evidence_method_index(value: EvidenceMethod) -> usize {
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

fn scan_row(row: &TraceabilityRow, issues: &mut Vec<ArtifactIssue>) {
    let scalar_strings = [
        row.finding_id.as_str(),
        row.fixture_category.as_str(),
        row.planned_evidence_id.as_str(),
        row.source_evidence.as_str(),
        row.target_assertion.as_str(),
    ];
    for value in scalar_strings
        .into_iter()
        .chain(row.current_seams.iter().map(String::as_str))
        .chain(row.planned_fixture_ids.iter().map(String::as_str))
    {
        if let Some(code) = decoded_structural_violation(value) {
            issues.push(issue(code.into()));
        }
    }
}

const fn issue(code: RedactionCode) -> ArtifactIssue {
    ArtifactIssue::new(None, code)
}
