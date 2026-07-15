use norn_policy::baseline::{
    GovernanceAuthoringError, GovernanceLinkError, GovernanceTable, GovernanceToken,
    LegacyGovernance, LegacyState, OriginId, OriginLedger, P1GovernanceReview,
    ReviewedDebtGovernanceRow, ReviewedLocGovernanceRow,
};
use norn_policy::phase_lock::CampaignPhase;

use super::support::{TestResult, baseline_from_sources, decoded_origin_fixture, digest, origin};

const EMPTY_CANONICAL_GOVERNANCE: &[u8] = br#"schema_version = 1
loc_exceptions = []
debt_exceptions = []

[algorithms]
analyzer = "norn-policy-1"
digest = "norn-sha256-canonical-json-1"
"#;

#[test]
fn authors_sorted_governance_and_round_trips_canonical_toml() -> TestResult {
    let ledger = two_family_origin()?;
    let loc_ids = over_limit_ids(&ledger);
    let debt_ids = ledger
        .prohibited_debt()
        .iter()
        .map(norn_policy::baseline::DebtOriginFact::origin_id)
        .collect::<Vec<_>>();
    let ordered_loc = loc_rows(&loc_ids)?;
    let ordered_debt = debt_rows(&debt_ids)?;
    let mut reversed_loc = ordered_loc.clone();
    let mut reversed_debt = ordered_debt.clone();
    reversed_loc.reverse();
    reversed_debt.reverse();

    let first = LegacyGovernance::author_p1(
        &ledger,
        P1GovernanceReview::new(reversed_loc, reversed_debt),
    )?;
    let second =
        LegacyGovernance::author_p1(&ledger, P1GovernanceReview::new(ordered_loc, ordered_debt))?;

    assert_eq!(first, second);
    assert!(strictly_sorted(first.loc_exceptions()));
    assert!(strictly_sorted(first.debt_exceptions()));
    let encoded = first.encode_canonical_toml();
    assert_eq!(encoded, second.encode_canonical_toml());
    assert!(encoded.ends_with(b"\n"));
    assert!(!encoded.ends_with(b"\n\n"));
    assert_eq!(LegacyGovernance::decode(&encoded)?, first);
    Ok(())
}

#[test]
fn canonical_empty_document_has_one_exact_representation() -> TestResult {
    let baseline = baseline_from_sources(&[("src/lib.rs", "pub fn stable() {}\n")])?;
    let ledger = decoded_origin_fixture(digest(10), &baseline)?;
    let governance =
        LegacyGovernance::author_p1(&ledger, P1GovernanceReview::new(Vec::new(), Vec::new()))?;

    let encoded = governance.encode_canonical_toml();
    assert_eq!(encoded, EMPTY_CANONICAL_GOVERNANCE);
    assert_eq!(LegacyGovernance::decode(&encoded)?, governance);
    Ok(())
}

#[test]
fn rejects_missing_wrong_family_and_duplicate_review_rows() -> TestResult {
    let ledger = origin()?;
    let loc_id = over_limit_ids(&ledger)
        .first()
        .copied()
        .ok_or_else(|| super::support::missing("LOC origin identity"))?;
    let debt_id = ledger
        .prohibited_debt()
        .first()
        .map(norn_policy::baseline::DebtOriginFact::origin_id)
        .ok_or_else(|| super::support::missing("debt origin identity"))?;
    let loc = loc_row(loc_id, "loc-review")?;
    let debt = debt_row(debt_id, "debt-review")?;

    assert_eq!(
        LegacyGovernance::author_p1(
            &ledger,
            P1GovernanceReview::new(Vec::new(), vec![debt.clone()]),
        ),
        Err(GovernanceAuthoringError::OriginLink(
            GovernanceLinkError::LocCoverage,
        ))
    );
    assert_eq!(
        LegacyGovernance::author_p1(
            &ledger,
            P1GovernanceReview::new(
                vec![loc_row(debt_id, "wrong-loc-family")?],
                vec![debt_row(loc_id, "wrong-debt-family")?],
            ),
        ),
        Err(GovernanceAuthoringError::OriginLink(
            GovernanceLinkError::LocCoverage,
        ))
    );
    assert_eq!(
        LegacyGovernance::author_p1(
            &ledger,
            P1GovernanceReview::new(vec![loc.clone(), loc], vec![debt]),
        ),
        Err(GovernanceAuthoringError::DuplicateOrigin {
            table: GovernanceTable::Loc,
        })
    );
    Ok(())
}

fn loc_rows(ids: &[OriginId]) -> TestResult<Vec<ReviewedLocGovernanceRow>> {
    ids.iter()
        .enumerate()
        .map(|(index, origin_id)| loc_row(*origin_id, &format!("loc-review-{index}")))
        .collect()
}

fn debt_rows(ids: &[OriginId]) -> TestResult<Vec<ReviewedDebtGovernanceRow>> {
    ids.iter()
        .enumerate()
        .map(|(index, origin_id)| debt_row(*origin_id, &format!("debt-review-{index}")))
        .collect()
}

fn loc_row(origin_id: OriginId, record: &str) -> TestResult<ReviewedLocGovernanceRow> {
    Ok(ReviewedLocGovernanceRow::new(
        origin_id,
        GovernanceToken::parse("policy-maintenance")?,
        CampaignPhase::P4,
        GovernanceToken::parse(record)?,
        LegacyState::Active,
    ))
}

fn debt_row(origin_id: OriginId, record: &str) -> TestResult<ReviewedDebtGovernanceRow> {
    Ok(ReviewedDebtGovernanceRow::new(
        origin_id,
        GovernanceToken::parse("policy-maintenance")?,
        CampaignPhase::P4,
        GovernanceToken::parse(record)?,
        LegacyState::Active,
    ))
}

fn over_limit_ids(ledger: &OriginLedger) -> Vec<OriginId> {
    ledger
        .production_files()
        .iter()
        .filter(|fact| fact.production_loc() > 500)
        .map(norn_policy::baseline::ProductionFileFact::origin_id)
        .collect()
}

fn strictly_sorted(entries: &[norn_policy::baseline::LegacyGovernanceEntry]) -> bool {
    entries
        .windows(2)
        .all(|pair| pair[0].origin_id() < pair[1].origin_id())
}

fn two_family_origin() -> TestResult<OriginLedger> {
    let first = legacy_module("FIRST", "first debt");
    let second = legacy_module("SECOND", "second debt");
    let baseline = baseline_from_sources(&[
        ("src/lib.rs", "mod first;\nmod second;\n"),
        ("src/first.rs", &first),
        ("src/second.rs", &second),
    ])?;
    decoded_origin_fixture(digest(10), &baseline)
}

fn legacy_module(prefix: &str, debt_message: &str) -> String {
    let mut source = String::new();
    for index in 0..510 {
        source.push_str("pub const ");
        source.push_str(prefix);
        source.push('_');
        source.push_str(&index.to_string());
        source.push_str(": u32 = 1;\n");
    }
    source.push_str("pub fn legacy_debt() { ");
    source.push_str(&["pan", "ic!"].concat());
    source.push_str("(\"");
    source.push_str(debt_message);
    source.push_str("\"); }\n");
    source
}
