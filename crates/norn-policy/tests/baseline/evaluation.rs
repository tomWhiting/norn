use norn_policy::baseline::{
    CurrentRepositoryFacts, LegacyIssueCode, LegacyState, evaluate_legacy,
};
use norn_policy::phase_lock::CampaignPhase;

use super::support::{
    TestResult, baseline_from_sources, current_legacy, current_with_new_debt,
    decoded_origin_fixture, digest, empty_governance, governance, isolated_debt_current,
    isolated_debt_origin, limits, origin, origin_baseline, origin_current,
};

#[test]
fn exact_unchanged_legacy_is_active_only_before_its_due_phase() -> TestResult {
    let ledger = origin()?;
    let governance = governance(&ledger, "active", "active", "P4")?;
    let evaluation = evaluate_legacy(
        &origin_current()?,
        &ledger,
        &governance,
        limits()?,
        CampaignPhase::P1,
    )?;

    assert!(evaluation.issues().is_empty());
    assert!(
        evaluation
            .dispositions()
            .iter()
            .all(|entry| entry.state() == LegacyState::Active)
    );
    Ok(())
}

#[test]
fn production_change_while_still_over_limit_fails_even_when_loc_decreases() -> TestResult {
    let ledger = origin()?;
    let governance = governance(&ledger, "active", "resolved", "P4")?;
    let changed = current_legacy(505, 99, false)?;
    let evaluation = evaluate_legacy(&changed, &ledger, &governance, limits()?, CampaignPhase::P1)?;

    assert_eq!(codes(&evaluation), vec![LegacyIssueCode::LocChanged]);
    Ok(())
}

#[test]
fn reaching_limit_requires_durable_resolution_then_remains_resolved() -> TestResult {
    let ledger = origin()?;
    let within_limit = current_legacy(480, 98, false)?;
    let active = governance(&ledger, "active", "resolved", "P4")?;
    let pending = evaluate_legacy(
        &within_limit,
        &ledger,
        &active,
        limits()?,
        CampaignPhase::P1,
    )?;
    assert_eq!(
        codes(&pending),
        vec![LegacyIssueCode::ResolutionNotRecorded]
    );

    let resolved = governance(&ledger, "resolved", "resolved", "P4")?;
    let complete = evaluate_legacy(
        &within_limit,
        &ledger,
        &resolved,
        limits()?,
        CampaignPhase::P1,
    )?;
    assert!(complete.issues().is_empty());
    assert_eq!(complete.dispositions()[0].state(), LegacyState::Resolved);
    Ok(())
}

#[test]
fn resolved_loc_exception_cannot_reactivate() -> TestResult {
    let ledger = origin()?;
    let resolved = governance(&ledger, "resolved", "active", "P4")?;
    let evaluation = evaluate_legacy(
        &origin_current()?,
        &ledger,
        &resolved,
        limits()?,
        CampaignPhase::P1,
    )?;

    assert!(codes(&evaluation).contains(&LegacyIssueCode::LocReactivated));
    Ok(())
}

#[test]
fn due_phase_is_exclusive_for_active_exceptions() -> TestResult {
    let ledger = origin()?;
    let governance = governance(&ledger, "active", "active", "P2")?;
    let evaluation = evaluate_legacy(
        &origin_current()?,
        &ledger,
        &governance,
        limits()?,
        CampaignPhase::P2,
    )?;
    let observed = codes(&evaluation);

    assert!(observed.contains(&LegacyIssueCode::LocOverdue));
    assert!(observed.contains(&LegacyIssueCode::DebtOverdue));
    Ok(())
}

#[test]
fn new_over_limit_file_and_new_debt_can_never_gain_legacy_status() -> TestResult {
    let ledger = origin()?;
    let governance = governance(&ledger, "active", "active", "P4")?;
    let current = current_with_new_debt()?;
    let evaluation = evaluate_legacy(&current, &ledger, &governance, limits()?, CampaignPhase::P1)?;
    let observed = codes(&evaluation);

    assert!(observed.contains(&LegacyIssueCode::NewLocException));
    assert!(observed.contains(&LegacyIssueCode::NewDebtException));
    Ok(())
}

#[test]
fn tightened_limit_creates_a_finding_without_rewriting_origin_governance() -> TestResult {
    let source = "pub const VALUE: u8 = 1;\n".repeat(450);
    let baseline = baseline_from_sources(&[
        ("src/lib.rs", "mod ordinary;\n"),
        ("src/ordinary.rs", &source),
    ])?;
    let ledger = decoded_origin_fixture(digest(10), &baseline)?;
    let current = CurrentRepositoryFacts::from_baseline(&baseline);
    let governance = empty_governance()?;
    let tightened = norn_policy::baseline::LocCeilings::new(200, 400)?;

    let evaluation = evaluate_legacy(&current, &ledger, &governance, tightened, CampaignPhase::P1)?;
    assert_eq!(codes(&evaluation), vec![LegacyIssueCode::NewLocException]);
    Ok(())
}

#[test]
fn unchanged_suppression_cannot_cover_changed_production_content() -> TestResult {
    let ledger = origin()?;
    let governance = governance(&ledger, "active", "active", "P4")?;
    let current = current_legacy(510, 77, true)?;
    let evaluation = evaluate_legacy(&current, &ledger, &governance, limits()?, CampaignPhase::P1)?;

    assert!(codes(&evaluation).contains(&LegacyIssueCode::DebtProductionChanged));
    Ok(())
}

#[test]
fn debt_removal_must_be_recorded_and_then_cannot_reactivate() -> TestResult {
    let ledger = isolated_debt_origin()?;
    let without_debt = isolated_debt_current(false)?;
    let active = governance(&ledger, "active", "active", "P4")?;
    let pending = evaluate_legacy(
        &without_debt,
        &ledger,
        &active,
        limits()?,
        CampaignPhase::P1,
    )?;
    assert!(codes(&pending).contains(&LegacyIssueCode::ResolutionNotRecorded));

    let resolved = governance(&ledger, "active", "resolved", "P4")?;
    let complete = evaluate_legacy(
        &without_debt,
        &ledger,
        &resolved,
        limits()?,
        CampaignPhase::P1,
    )?;
    assert!(complete.issues().is_empty());

    let reactivated = evaluate_legacy(
        &isolated_debt_current(true)?,
        &ledger,
        &resolved,
        limits()?,
        CampaignPhase::P1,
    )?;
    assert!(codes(&reactivated).contains(&LegacyIssueCode::DebtReactivated));
    Ok(())
}

#[test]
fn sealed_current_reconstruction_retains_every_origin_family() -> TestResult {
    let baseline = origin_baseline()?;
    let current = CurrentRepositoryFacts::from_baseline(&baseline);

    assert_eq!(
        current.source_inventory_digest(),
        baseline.source_inventory_digest()
    );
    assert_eq!(current.production_files(), baseline.production_files());
    assert_eq!(current.item_groups(), baseline.item_groups());
    assert_eq!(current.prohibited_debt(), baseline.prohibited_debt());
    assert_eq!(current.writer_operations(), baseline.writer_operations());
    Ok(())
}

fn codes(evaluation: &norn_policy::baseline::LegacyEvaluation) -> Vec<LegacyIssueCode> {
    evaluation
        .issues()
        .iter()
        .map(norn_policy::baseline::LegacyIssue::code)
        .collect()
}
