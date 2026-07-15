use norn_policy::baseline::{
    GovernanceAnchorError, GovernanceError, GovernanceLinkError, LegacyGovernance,
    P1_GOVERNANCE_ANCHOR_IDENTITY, ReviewedGovernanceAnchor,
};

use super::support::{
    TestResult, baseline_from_sources, decoded_origin_fixture, digest, governance_document, limits,
    origin,
};

const EMPTY_GOVERNANCE: &[u8] = br#"schema_version = 1
loc_exceptions = []
debt_exceptions = []

[algorithms]
analyzer = "norn-policy-1"
digest = "norn-sha256-canonical-json-1"
"#;

#[test]
fn accepts_exact_closed_governance_and_ignores_toml_formatting() -> TestResult {
    let ledger = origin()?;
    let document = governance_document(&ledger, "active", "active", "P4")?;
    let reordered = document
        .replacen(
            "analyzer = \"norn-policy-1\"\ndigest = \"norn-sha256-canonical-json-1\"",
            "digest = \"norn-sha256-canonical-json-1\"\nanalyzer = \"norn-policy-1\"",
            1,
        )
        .replacen(
            "owner = \"policy-team\"\ndue_phase = \"P4\"",
            "due_phase = \"P4\"\nowner = \"policy-team\"",
            1,
        );
    let first = LegacyGovernance::decode(document.as_bytes())?;
    let second = LegacyGovernance::decode(reordered.as_bytes())?;

    first.validate_against(&ledger, limits()?)?;
    assert_eq!(first, second);
    assert_eq!(first.normalized_digest()?, second.normalized_digest()?);
    Ok(())
}

#[test]
fn rejects_duplicate_unknown_invalid_token_and_algorithm_drift() -> TestResult {
    let ledger = origin()?;
    let document = governance_document(&ledger, "active", "active", "P4")?;
    let duplicate = document.replacen(
        "schema_version = 1",
        "schema_version = 1\nschema_version = 1",
        1,
    );
    let unknown = document.replacen(
        "schema_version = 1",
        "schema_version = 1\nadvisory = true",
        1,
    );
    let obsolete_writer_table = document.replacen(
        "schema_version = 1",
        "schema_version = 1\nwriter_operations = []",
        1,
    );
    let prose_owner = document.replacen("owner = \"policy-team\"", "owner = \"Policy Team\"", 1);
    let analyzer = document.replacen("norn-policy-1", "norn-policy-2", 1);

    for invalid in [duplicate, unknown, obsolete_writer_table, prose_owner] {
        assert!(matches!(
            LegacyGovernance::decode(invalid.as_bytes()),
            Err(GovernanceError::Toml(_))
        ));
    }
    assert!(matches!(
        LegacyGovernance::decode(analyzer.as_bytes()),
        Err(GovernanceError::AnalyzerVersion)
    ));
    Ok(())
}

#[test]
fn rejects_unsorted_or_duplicate_governance_rows() -> TestResult {
    let ledger = origin()?;
    let document = governance_document(&ledger, "active", "active", "P4")?;
    let loc_block = document
        .split("[[loc_exceptions]]")
        .nth(1)
        .and_then(|rest| rest.split("[[debt_exceptions]]").next())
        .ok_or_else(|| super::support::missing("LOC governance block"))?;
    let duplicate = document.replacen(
        "[[debt_exceptions]]",
        &format!("[[loc_exceptions]]{loc_block}[[debt_exceptions]]"),
        1,
    );

    assert!(matches!(
        LegacyGovernance::decode(duplicate.as_bytes()),
        Err(GovernanceError::Order { .. })
    ));
    Ok(())
}

#[test]
fn requires_exact_origin_coverage_for_every_governed_family() -> TestResult {
    let ledger = origin()?;
    let document = governance_document(&ledger, "active", "active", "P4")?;
    let over_limit = ledger
        .production_files()
        .iter()
        .find(|fact| fact.production_loc() > 500)
        .ok_or_else(|| super::support::missing("over-limit production fact"))?;
    let prohibited_debt = ledger
        .prohibited_debt()
        .first()
        .ok_or_else(|| super::support::missing("prohibited debt fact"))?;
    let wrong_loc = document.replacen(
        &over_limit.origin_id().digest().to_string(),
        &prohibited_debt.origin_id().digest().to_string(),
        1,
    );
    let wrong_debt = document.replacen(
        &prohibited_debt.origin_id().digest().to_string(),
        &over_limit.origin_id().digest().to_string(),
        1,
    );

    assert!(matches!(
        LegacyGovernance::decode(wrong_loc.as_bytes())?.validate_against(&ledger, limits()?),
        Err(GovernanceLinkError::LocCoverage)
    ));
    assert!(matches!(
        LegacyGovernance::decode(wrong_debt.as_bytes())?.validate_against(&ledger, limits()?),
        Err(GovernanceLinkError::DebtCoverage)
    ));
    Ok(())
}

#[test]
fn reviewed_anchor_accepts_only_the_compiled_identity() -> TestResult {
    let baseline = baseline_from_sources(&[("src/lib.rs", "pub fn stable() {}\n")])?;
    let ledger = decoded_origin_fixture(digest(10), &baseline)?;
    let anchor = ReviewedGovernanceAnchor::acquire(EMPTY_GOVERNANCE, &ledger)?;
    let current = LegacyGovernance::decode(EMPTY_GOVERNANCE)?;

    assert_eq!(anchor.identity(), P1_GOVERNANCE_ANCHOR_IDENTITY);
    anchor.validate_successor(&current)?;
    assert_eq!(format!("{anchor:?}"), "ReviewedGovernanceAnchor { .. }");
    Ok(())
}

#[test]
fn reviewed_anchor_rejects_caller_selected_and_unlinked_documents() -> TestResult {
    let ledger = origin()?;
    let selected = governance_document(&ledger, "active", "active", "P4")?;
    assert!(matches!(
        ReviewedGovernanceAnchor::acquire(selected.as_bytes(), &ledger),
        Err(GovernanceAnchorError::Identity)
    ));
    assert!(matches!(
        ReviewedGovernanceAnchor::acquire(EMPTY_GOVERNANCE, &ledger),
        Err(GovernanceAnchorError::OriginLink)
    ));

    let sentinel = "norn-synthetic-private-governance-value";
    let error = ReviewedGovernanceAnchor::acquire(sentinel.as_bytes(), &ledger);
    let Err(error) = error else {
        return Err("invalid reviewed anchor unexpectedly acquired".into());
    };
    assert_eq!(error, GovernanceAnchorError::Invalid);
    assert!(!error.to_string().contains(sentinel));
    assert!(!format!("{error:?}").contains(sentinel));
    Ok(())
}
