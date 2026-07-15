use norn_policy::rust::cargo::{CargoTargetKind, PackageRoot, TargetClass, discover_cargo};

use super::support::{TestResult, snapshot};

#[test]
fn automatic_layout_discovers_every_target_family_and_class() -> TestResult {
    let snapshot = snapshot(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"app\"]"),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\nedition = \"2024\"",
        ),
        ("app/src/lib.rs", ""),
        ("app/src/main.rs", ""),
        ("app/src/bin/tool.rs", ""),
        ("app/src/bin/multi/main.rs", ""),
        ("app/examples/example.rs", ""),
        ("app/examples/nested/main.rs", ""),
        ("app/tests/integration.rs", ""),
        ("app/tests/nested/main.rs", ""),
        ("app/benches/bench.rs", ""),
        ("app/benches/nested/main.rs", ""),
        ("app/build.rs", ""),
    ])?;

    let result = discover_cargo(&snapshot);
    assert!(result.is_valid());
    let serialized = serde_json::to_value(&result)?;
    assert_eq!(serialized["packages"][0]["name"], "app");
    assert_eq!(serialized["packages"][0]["targets"][0]["kind"], "library");
    assert_eq!(serialized["diagnostics"], serde_json::json!([]));
    let package = &result.packages()[0];
    assert_eq!(package.root(), &PackageRoot::Member("app".parse()?));
    assert_eq!(package.targets().len(), 11);
    assert_target(
        package,
        CargoTargetKind::Library,
        TargetClass::Production,
        "app",
        "app/src/lib.rs",
    );
    assert_target(
        package,
        CargoTargetKind::Binary,
        TargetClass::Production,
        "app",
        "app/src/main.rs",
    );
    assert_target(
        package,
        CargoTargetKind::Binary,
        TargetClass::Production,
        "multi",
        "app/src/bin/multi/main.rs",
    );
    assert_target(
        package,
        CargoTargetKind::Example,
        TargetClass::Production,
        "nested",
        "app/examples/nested/main.rs",
    );
    assert_target(
        package,
        CargoTargetKind::BuildScript,
        TargetClass::Production,
        "build-script-build",
        "app/build.rs",
    );
    assert_target(
        package,
        CargoTargetKind::IntegrationTest,
        TargetClass::TestOnly,
        "integration",
        "app/tests/integration.rs",
    );
    assert_target(
        package,
        CargoTargetKind::Benchmark,
        TargetClass::TestOnly,
        "bench",
        "app/benches/bench.rs",
    );
    Ok(())
}

#[test]
fn explicit_targets_and_build_path_override_all_auto_families() -> TestResult {
    let manifest = r#"
        [workspace]
        [package]
        name = "macro-package"
        edition = "2024"
        autolib = false
        autobins = false
        autoexamples = false
        autotests = false
        autobenches = false
        build = "support/build.rs"

        [lib]
        name = "macro_api"
        path = "code/lib.rs"
        proc-macro = true

        [[bin]]
        name = "runner"
        path = "code/main.rs"

        [[example]]
        name = "demo"
        path = "code/demo.rs"

        [[test]]
        name = "integration"
        path = "code/test.rs"

        [[bench]]
        name = "speed"
        path = "code/bench.rs"
    "#;
    let snapshot = snapshot(&[
        ("Cargo.toml", manifest),
        ("code/lib.rs", ""),
        ("code/main.rs", ""),
        ("code/demo.rs", ""),
        ("code/test.rs", ""),
        ("code/bench.rs", ""),
        ("support/build.rs", ""),
        ("src/lib.rs", "must remain disabled"),
        ("src/main.rs", "must remain disabled"),
        ("examples/auto.rs", "must remain disabled"),
        ("tests/auto.rs", "must remain disabled"),
        ("benches/auto.rs", "must remain disabled"),
    ])?;

    let result = discover_cargo(&snapshot);
    assert!(result.is_valid());
    let package = &result.packages()[0];
    assert_eq!(package.targets().len(), 6);
    assert_target(
        package,
        CargoTargetKind::ProcMacro,
        TargetClass::Production,
        "macro_api",
        "code/lib.rs",
    );
    assert_target(
        package,
        CargoTargetKind::Binary,
        TargetClass::Production,
        "runner",
        "code/main.rs",
    );
    assert_target(
        package,
        CargoTargetKind::IntegrationTest,
        TargetClass::TestOnly,
        "integration",
        "code/test.rs",
    );
    assert_target(
        package,
        CargoTargetKind::BuildScript,
        TargetClass::Production,
        "build-script-build",
        "support/build.rs",
    );
    Ok(())
}

#[test]
fn edition_2015_manual_targets_disable_auto_unless_reenabled() -> TestResult {
    let manifest = r#"
        [workspace]
        [package]
        name = "legacy"
        edition = "2015"
        autolib = true
        build = false

        [[bin]]
        name = "declared"
    "#;
    let snapshot = snapshot(&[
        ("Cargo.toml", manifest),
        ("src/lib.rs", ""),
        ("src/main.rs", "must not be automatic"),
        ("src/bin/other.rs", "must not be automatic"),
        ("src/bin/declared.rs", ""),
        ("build.rs", "must remain disabled"),
    ])?;

    let result = discover_cargo(&snapshot);
    assert!(result.is_valid());
    let roots: Vec<_> = result.packages()[0]
        .targets()
        .iter()
        .map(|target| target.root().as_str())
        .collect();
    assert_eq!(roots, ["src/lib.rs", "src/bin/declared.rs"]);
    Ok(())
}

fn assert_target(
    package: &norn_policy::rust::cargo::CargoPackage,
    kind: CargoTargetKind,
    class: TargetClass,
    name: &str,
    root: &str,
) {
    assert!(package.targets().iter().any(|target| {
        target.kind() == kind
            && target.class() == class
            && target.name() == name
            && target.root().as_str() == root
    }));
}
