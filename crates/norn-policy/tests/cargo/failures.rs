use norn_policy::EntryKind;
use norn_policy::rust::cargo::{CargoDiagnosticCode, CargoTargetKind, discover_cargo};

use super::support::{TestResult, snapshot, snapshot_with_kind};

#[test]
fn invalid_options_paths_and_missing_targets_fail_closed() -> TestResult {
    let manifest = r#"
        [package]
        name = "broken"
        edition.workspace = true
        autobins = "yes"
        build = "../outside.rs"

        [[bin]]
        name = "escape"
        path = "../outside.rs"

        [[example]]
        name = "wrong-type"
        path = 7

        [[test]]
        name = "missing"
        path = "missing.rs"
    "#;
    let snapshot = snapshot(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"member\"]"),
        ("member/Cargo.toml", manifest),
    ])?;

    let result = discover_cargo(&snapshot);
    assert_code(&result, CargoDiagnosticCode::PackageOptionInvalid);
    assert_code(&result, CargoDiagnosticCode::TargetPathInvalid);
    assert_code(&result, CargoDiagnosticCode::TargetMissing);
    assert!(!result.is_valid());
    Ok(())
}

#[test]
fn ambiguous_auto_roots_and_duplicate_explicit_names_fail_closed() -> TestResult {
    let manifest = r#"
        [workspace]
        [package]
        name = "ambiguous"
        edition = "2024"

        [[example]]
        name = "duplicate"
        path = "examples/one.rs"

        [[example]]
        name = "duplicate"
        path = "examples/two.rs"
    "#;
    let snapshot = snapshot(&[
        ("Cargo.toml", manifest),
        ("src/bin/tool.rs", ""),
        ("src/bin/tool/main.rs", ""),
        ("examples/one.rs", ""),
        ("examples/two.rs", ""),
    ])?;

    let result = discover_cargo(&snapshot);
    assert_code(&result, CargoDiagnosticCode::TargetAmbiguous);
    assert_code(&result, CargoDiagnosticCode::DuplicateTarget);
    Ok(())
}

#[test]
fn nonregular_targets_and_malformed_target_tables_fail_closed() -> TestResult {
    let manifest = r#"
        [workspace]
        [package]
        name = "links"
        edition = "2024"

        [lib]
        path = "src/lib.rs"

        [[bin]]
        path = "src/main.rs"
    "#;
    let snapshot = snapshot_with_kind(&[
        ("Cargo.toml", EntryKind::Regular, manifest),
        ("src/lib.rs", EntryKind::Symlink, "../../outside.rs"),
        ("src/main.rs", EntryKind::Regular, ""),
    ])?;

    let result = discover_cargo(&snapshot);
    assert_code(&result, CargoDiagnosticCode::EntryNotRegular);
    assert_code(&result, CargoDiagnosticCode::TargetInvalid);
    Ok(())
}

#[test]
fn malformed_package_and_inherited_edition_are_diagnostics() -> TestResult {
    let malformed_package = snapshot(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"member\"]"),
        ("member/Cargo.toml", "[package]\nname = 17"),
    ])?;
    assert_code(
        &discover_cargo(&malformed_package),
        CargoDiagnosticCode::PackageInvalid,
    );

    let inherited = snapshot(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"member\"]\n[workspace.package]\nedition = 7",
        ),
        (
            "member/Cargo.toml",
            "[package]\nname = \"member\"\nedition.workspace = true",
        ),
    ])?;
    assert_code(
        &discover_cargo(&inherited),
        CargoDiagnosticCode::PackageOptionInvalid,
    );
    Ok(())
}

#[test]
fn crate_types_are_closed_and_proc_macro_classification_is_explicit() -> TestResult {
    for crate_types in ["[\"not-a-crate-type\"]", "[]", "[\"proc-macro\", \"lib\"]"] {
        let manifest = format!(
            "[workspace]\n[package]\nname = \"invalid\"\nedition = \"2024\"\n[lib]\ncrate-type = {crate_types}"
        );
        let snapshot = snapshot(&[("Cargo.toml", &manifest), ("src/lib.rs", "")])?;
        assert_code(
            &discover_cargo(&snapshot),
            CargoDiagnosticCode::TargetInvalid,
        );
    }

    let valid = snapshot(&[
        (
            "Cargo.toml",
            "[workspace]\n[package]\nname = \"macro\"\nedition = \"2024\"\n[lib]\ncrate-type = [\"proc-macro\"]",
        ),
        ("src/lib.rs", ""),
    ])?;
    let result = discover_cargo(&valid);
    assert!(result.is_valid());
    assert_eq!(
        result.packages()[0].targets()[0].kind(),
        CargoTargetKind::ProcMacro
    );
    Ok(())
}

#[test]
fn package_without_any_compilation_target_is_invalid() -> TestResult {
    let manifest = concat!(
        "[workspace]\n",
        "[package]\nname = \"empty\"\nedition = \"2024\"\nbuild = false\n",
        "autolib = false\nautobins = false\nautoexamples = false\n",
        "autotests = false\nautobenches = false\n",
    );
    let snapshot = snapshot(&[("Cargo.toml", manifest)])?;

    let result = discover_cargo(&snapshot);
    assert!(!result.is_valid());
    assert_code(&result, CargoDiagnosticCode::NoPackageTargets);
    assert_eq!(result.packages().len(), 1);
    assert!(result.packages()[0].targets().is_empty());
    Ok(())
}

fn assert_code(result: &norn_policy::rust::cargo::CargoDiscovery, code: CargoDiagnosticCode) {
    assert!(
        result
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == code)
    );
}
