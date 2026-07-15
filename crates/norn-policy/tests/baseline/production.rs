use norn_policy::baseline::{ProductionFactError, ProductionFileFact, ProductionLocClass};
use norn_policy::rust::modules::ModuleTargetKind;

use super::support::{TestResult, canonical_production, limits, target};

#[test]
fn custom_binary_root_uses_thin_ceiling_regardless_of_name() -> TestResult {
    let path = "crates/sample/src/daemon.rs";
    let canonical = canonical_production(
        path,
        201,
        30,
        vec![target("sample", "daemon", ModuleTargetKind::Binary, path)?],
    )?;
    let fact = ProductionFileFact::try_from(&canonical)?;

    assert_eq!(fact.loc_class(), ProductionLocClass::ThinEntrypoint);
    assert_eq!(limits()?.limit_for(&fact), 200);
    Ok(())
}

#[test]
fn custom_library_and_proc_macro_roots_use_thin_ceiling() -> TestResult {
    for (kind, name, path) in [
        (
            ModuleTargetKind::Library,
            "sample-lib",
            "crates/sample/src/public_api.rs",
        ),
        (
            ModuleTargetKind::ProcMacro,
            "sample-macros",
            "crates/sample/src/macros_root.rs",
        ),
    ] {
        let canonical =
            canonical_production(path, 201, 36, vec![target("sample", name, kind, path)?])?;
        let fact = ProductionFileFact::try_from(&canonical)?;

        assert_eq!(fact.loc_class(), ProductionLocClass::ThinEntrypoint);
        assert_eq!(limits()?.limit_for(&fact), 200);
    }
    Ok(())
}

#[test]
fn example_and_build_script_roots_use_other_file_ceiling() -> TestResult {
    for (kind, name, path) in [
        (
            ModuleTargetKind::Example,
            "showcase",
            "crates/sample/examples/showcase.rs",
        ),
        (
            ModuleTargetKind::BuildScript,
            "build-script-build",
            "crates/sample/build.rs",
        ),
    ] {
        let canonical =
            canonical_production(path, 201, 37, vec![target("sample", name, kind, path)?])?;
        let fact = ProductionFileFact::try_from(&canonical)?;

        assert_eq!(fact.loc_class(), ProductionLocClass::Other);
        assert_eq!(limits()?.limit_for(&fact), 500);
    }
    Ok(())
}

#[test]
fn nested_lib_and_main_modules_use_other_file_ceiling() -> TestResult {
    for path in [
        "crates/sample/src/nested/lib.rs",
        "crates/sample/src/nested/main.rs",
    ] {
        let canonical = canonical_production(
            path,
            450,
            31,
            vec![target(
                "sample",
                "daemon",
                ModuleTargetKind::Binary,
                "crates/sample/src/daemon.rs",
            )?],
        )?;
        let fact = ProductionFileFact::try_from(&canonical)?;

        assert_eq!(fact.loc_class(), ProductionLocClass::Other);
        assert_eq!(limits()?.limit_for(&fact), 500);
    }
    Ok(())
}

#[test]
fn mixed_shared_target_set_is_preserved_without_selecting_one_kind() -> TestResult {
    let targets = vec![
        target(
            "sample",
            "sample-lib",
            ModuleTargetKind::Library,
            "crates/sample/src/lib_root.rs",
        )?,
        target(
            "sample",
            "sample-bin",
            ModuleTargetKind::Binary,
            "crates/sample/src/bin_root.rs",
        )?,
    ];
    let canonical = canonical_production("crates/sample/src/shared.rs", 40, 32, targets.clone())?;
    let fact = ProductionFileFact::try_from(&canonical)?;

    assert_eq!(fact.targets(), targets);
    assert_eq!(fact.loc_class(), ProductionLocClass::Other);
    assert_eq!(fact.projection_identity(), fact.projection_hash());
    Ok(())
}

#[test]
fn shared_source_uses_stricter_class_when_it_is_any_thin_target_root() -> TestResult {
    let path = "crates/sample/src/shared.rs";
    let targets = vec![
        target("sample", "sample-lib", ModuleTargetKind::Library, path)?,
        target(
            "sample",
            "sample-bin",
            ModuleTargetKind::Binary,
            "crates/sample/src/bin_root.rs",
        )?,
    ];
    let canonical = canonical_production(path, 201, 33, targets)?;
    let fact = ProductionFileFact::try_from(&canonical)?;

    assert_eq!(fact.loc_class(), ProductionLocClass::ThinEntrypoint);
    assert_eq!(limits()?.limit_for(&fact), 200);
    Ok(())
}

#[test]
fn conversion_rejects_empty_unsorted_duplicate_and_test_target_sets() -> TestResult {
    let library = target(
        "sample",
        "sample-lib",
        ModuleTargetKind::Library,
        "crates/sample/src/lib.rs",
    )?;
    let binary = target(
        "sample",
        "sample-bin",
        ModuleTargetKind::Binary,
        "crates/sample/src/main.rs",
    )?;
    let integration = target(
        "sample",
        "integration",
        ModuleTargetKind::IntegrationTest,
        "crates/sample/tests/integration.rs",
    )?;

    let empty = canonical_production("crates/sample/src/shared.rs", 1, 34, Vec::new())?;
    assert!(matches!(
        ProductionFileFact::try_from(&empty),
        Err(ProductionFactError::EmptyTargets)
    ));

    let unsorted = canonical_production(
        "crates/sample/src/shared.rs",
        1,
        34,
        vec![binary, library.clone()],
    )?;
    assert!(matches!(
        ProductionFileFact::try_from(&unsorted),
        Err(ProductionFactError::TargetOrder { .. })
    ));

    let duplicate = canonical_production(
        "crates/sample/src/shared.rs",
        1,
        34,
        vec![library.clone(), library],
    )?;
    assert!(matches!(
        ProductionFileFact::try_from(&duplicate),
        Err(ProductionFactError::TargetOrder { .. })
    ));

    let test_target = canonical_production(
        "crates/sample/tests/integration.rs",
        1,
        34,
        vec![integration],
    )?;
    assert!(matches!(
        ProductionFileFact::try_from(&test_target),
        Err(ProductionFactError::NonProductionTarget { .. })
    ));
    Ok(())
}

#[test]
fn conversion_rejects_loc_outside_stable_ledger_range() -> TestResult {
    let canonical = canonical_production(
        "crates/sample/src/main.rs",
        u64::MAX,
        35,
        vec![target(
            "sample",
            "sample-bin",
            ModuleTargetKind::Binary,
            "crates/sample/src/main.rs",
        )?],
    )?;

    assert!(matches!(
        ProductionFileFact::try_from(&canonical),
        Err(ProductionFactError::LocOverflow(_))
    ));
    Ok(())
}
