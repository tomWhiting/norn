use norn_policy::baseline::{
    CurrentRepositoryFacts, ItemComparisonSide, ItemGroupError, ItemGroupFact, LegacyIssueCode,
    LegacyKind, compare_item_groups, evaluate_legacy,
};
use norn_policy::path::RepositoryPath;
use norn_policy::phase_lock::CampaignPhase;
use norn_policy::rust::rust_item_projections;

use super::support::{
    TestResult, baseline_from_sources, decoded_origin_fixture, digest, empty_governance, item_group,
};

#[test]
fn comparison_detects_only_a_production_to_test_count_transfer() -> TestResult {
    let baseline = item_group("crates/sample/src/lib.rs", 1, 2, 3, 1)?;
    let hidden = item_group("crates/sample/src/lib.rs", 1, 2, 1, 3)?;

    let findings = compare_item_groups(std::slice::from_ref(&baseline), &[hidden])?;
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].origin_id(), baseline.origin_id());
    assert_eq!(findings[0].hidden_count(), 2);
    assert_eq!(findings[0].path(), baseline.path());
    Ok(())
}

#[test]
fn comparison_distinguishes_removal_test_addition_and_content_change() -> TestResult {
    let baseline = item_group("crates/sample/src/lib.rs", 1, 2, 1, 0)?;
    let test_added = item_group("crates/sample/src/lib.rs", 1, 2, 1, 1)?;
    let changed_content = item_group("crates/sample/src/lib.rs", 1, 3, 0, 1)?;

    assert!(compare_item_groups(std::slice::from_ref(&baseline), &[])?.is_empty());
    assert!(compare_item_groups(std::slice::from_ref(&baseline), &[test_added])?.is_empty());
    assert!(compare_item_groups(&[baseline], &[changed_content])?.is_empty());
    Ok(())
}

#[test]
fn canonical_projection_adapter_preserves_stable_group_fields() -> TestResult {
    let path = RepositoryPath::parse("crates/sample/src/lib.rs")?;
    let projections =
        rust_item_projections(&path, b"fn stable() {}\n#[cfg(test)]\nfn test_only() {}\n")?;
    let projection = projections
        .first()
        .ok_or_else(|| super::support::missing("Rust item projection"))?;
    let fact = ItemGroupFact::from_projection(&path, projection)?;

    assert_eq!(fact.path(), &path);
    assert_eq!(fact.base_identity(), projection.base_identity());
    assert_eq!(fact.content(), projection.content());
    assert_eq!(fact.production_count(), projection.production_count());
    assert_eq!(fact.test_only_count(), projection.test_only_count());
    Ok(())
}

#[test]
fn empty_and_duplicate_aggregates_fail_closed() -> TestResult {
    assert_eq!(
        ItemGroupFact::new(
            RepositoryPath::parse("crates/sample/src/lib.rs")?,
            digest(1),
            digest(2),
            0,
            0,
        ),
        Err(ItemGroupError::Empty)
    );

    let duplicate = item_group("crates/sample/src/lib.rs", 1, 2, 1, 0)?;
    assert!(matches!(
        compare_item_groups(&[duplicate.clone(), duplicate], &[]),
        Err(error)
            if error.side() == ItemComparisonSide::Origin && error.index() == 1
    ));
    let earlier = item_group("a/src/lib.rs", 3, 4, 1, 0)?;
    let later = item_group("z/src/lib.rs", 3, 4, 1, 0)?;
    assert!(matches!(
        compare_item_groups(&[], &[later, earlier]),
        Err(error)
            if error.side() == ItemComparisonSide::Current && error.index() == 1
    ));
    Ok(())
}

#[test]
fn legacy_evaluation_surfaces_production_hidden_as_test() -> TestResult {
    let before = baseline_from_sources(&[("src/lib.rs", "pub fn stable_value() -> u8 { 7 }\n")])?;
    let after = baseline_from_sources(&[(
        "src/lib.rs",
        "#[cfg(test)]\npub fn stable_value() -> u8 { 7 }\n",
    )])?;
    let origin = decoded_origin_fixture(digest(10), &before)?;
    let current = CurrentRepositoryFacts::from_baseline(&after);
    let governance = empty_governance()?;

    let result = evaluate_legacy(
        &current,
        &origin,
        &governance,
        super::support::limits()?,
        CampaignPhase::P1,
    )?;
    assert_eq!(result.issues().len(), 1);
    assert_eq!(
        result.issues()[0].code(),
        LegacyIssueCode::ProductionHiddenAsTest
    );
    assert_eq!(result.issues()[0].kind(), LegacyKind::ProductionItem);
    assert_eq!(result.issues()[0].hidden_count(), Some(1));
    Ok(())
}

#[test]
fn comparison_detects_cross_path_and_container_transfers() -> TestResult {
    let path_origin = item_group("src/production.rs", 1, 9, 1, 0)?;
    let path_current = item_group("src/tests.rs", 2, 9, 0, 1)?;
    let path_findings = compare_item_groups(std::slice::from_ref(&path_origin), &[path_current])?;
    assert_eq!(path_findings.len(), 1);
    assert_eq!(path_findings[0].origin_id(), path_origin.origin_id());

    let before = baseline_from_sources(&[(
        "src/lib.rs",
        "mod production { pub fn stable() -> u8 { 7 } }\n",
    )])?;
    let after = baseline_from_sources(&[(
        "src/lib.rs",
        "#[cfg(test)] mod tests { pub fn stable() -> u8 { 7 } }\n",
    )])?;
    let result = evaluate_between(&before, &after)?;
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0],
        (LegacyIssueCode::ProductionHiddenAsTest, Some(1))
    );
    Ok(())
}

#[test]
fn comparison_accounts_for_cross_group_duplicate_multiplicity() -> TestResult {
    let first = item_group("src/first.rs", 1, 9, 2, 1)?;
    let second = item_group("src/second.rs", 2, 9, 1, 0)?;
    let retained = item_group("src/first.rs", 1, 9, 1, 0)?;
    let tests = item_group("tests/moved.rs", 3, 9, 0, 3)?;

    let findings = compare_item_groups(&[first, second], &[retained, tests])?;
    assert_eq!(findings.len(), 2);
    assert_eq!(
        findings
            .iter()
            .map(norn_policy::baseline::ItemReclassification::hidden_count)
            .sum::<u32>(),
        2
    );
    Ok(())
}

#[test]
fn semantic_comparison_does_not_conflate_non_transfers() -> TestResult {
    let production = item_group("src/live.rs", 1, 9, 1, 0)?;
    let retained = item_group("src/live.rs", 1, 9, 1, 0)?;
    let added_test = item_group("tests/new.rs", 2, 9, 0, 1)?;
    let changed_test = item_group("tests/changed.rs", 3, 8, 0, 1)?;

    assert!(compare_item_groups(std::slice::from_ref(&production), &[])?.is_empty());
    assert!(
        compare_item_groups(std::slice::from_ref(&production), &[retained, added_test],)?
            .is_empty()
    );
    assert!(compare_item_groups(&[production], &[changed_test])?.is_empty());

    let moved_production = item_group("src/moved.rs", 4, 9, 1, 0)?;
    let independent_test = item_group("tests/copy.rs", 5, 9, 0, 1)?;
    let original = item_group("src/live.rs", 1, 9, 1, 0)?;
    assert!(compare_item_groups(&[original], &[moved_production, independent_test])?.is_empty());
    Ok(())
}

#[test]
fn raw_equivalent_name_cannot_hide_a_cross_path_transfer() -> TestResult {
    let before = baseline_from_sources(&[
        ("src/lib.rs", "mod live;\n"),
        ("src/live.rs", "pub fn stable() -> u8 { 7 }\n"),
    ])?;
    let after = baseline_from_sources(&[
        ("src/lib.rs", "#[cfg(test)] mod hidden;\n"),
        ("src/hidden.rs", "pub fn r#stable() -> u8 { 7 }\n"),
    ])?;

    let result = evaluate_between(&before, &after)?;
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0],
        (LegacyIssueCode::ProductionHiddenAsTest, Some(1))
    );
    Ok(())
}

fn evaluate_between(
    before: &norn_policy::baseline::RepositoryBaselineFacts,
    after: &norn_policy::baseline::RepositoryBaselineFacts,
) -> TestResult<Vec<(LegacyIssueCode, Option<u32>)>> {
    let origin = decoded_origin_fixture(digest(10), before)?;
    let current = CurrentRepositoryFacts::from_baseline(after);
    let result = evaluate_legacy(
        &current,
        &origin,
        &empty_governance()?,
        super::support::limits()?,
        CampaignPhase::P1,
    )?;
    Ok(result
        .issues()
        .iter()
        .map(|issue| (issue.code(), issue.hidden_count()))
        .collect())
}
