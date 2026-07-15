use norn_policy::rust::modules::{GeneratedIncludeRegistry, ModuleAnalysis, ModuleDiagnosticCode};

use super::fixtures::{TRYBUILD_LOCK, TRYBUILD_MANIFEST, harness, has_code, has_diagnostic};
use super::support::{TestResult, analyze, file};

#[test]
fn dead_helpers_closures_and_false_branches_cannot_select() -> TestResult {
    let helper = format!(
        "fn helper() {{\n{}\n}}\n{}",
        "let cases = trybuild::TestCases::new();\ncases.compile_fail(\"tests/ui/case.rs\");",
        harness("")
    );
    let closure = harness("let run = || cases.compile_fail(\"tests/ui/case.rs\");\nrun();");
    let false_branch = harness("if false { cases.compile_fail(\"tests/ui/case.rs\"); }");
    for (name, source) in [
        ("helper", helper),
        ("closure", closure),
        ("false branch", false_branch),
    ] {
        let result = authority_analysis(TRYBUILD_MANIFEST, TRYBUILD_LOCK, &source)?;
        assert_unclassified(&result, name);
    }
    Ok(())
}

#[test]
fn fake_module_and_alias_cannot_supply_the_trybuild_name() -> TestResult {
    let fake = r#"
        pub struct TestCases;
        impl TestCases {
            pub fn new() -> Self { Self }
            pub fn compile_fail(&self, _: &str) {}
        }
    "#;
    let module = format!("mod trybuild {{ {fake} }}\n{}", selected_harness());
    let alias = format!(
        "mod fake {{ {fake} }}\nuse fake as trybuild;\n{}",
        selected_harness()
    );
    for (name, source) in [("module", module), ("alias", alias)] {
        let result = authority_analysis(TRYBUILD_MANIFEST, TRYBUILD_LOCK, &source)?;
        assert_unclassified(&result, name);
    }
    Ok(())
}

#[test]
fn modified_function_headers_are_not_authoritative() -> TestResult {
    for modifier in [
        "pub",
        "pub(crate)",
        "async",
        "const",
        "unsafe",
        "extern \"C\"",
    ] {
        let source = format!("#[test]\n{modifier} fn ui() {{\n{}\n}}\n", selected_body());
        let result = authority_analysis(TRYBUILD_MANIFEST, TRYBUILD_LOCK, &source)?;
        assert_unclassified(&result, modifier);
    }
    Ok(())
}

#[test]
fn parameterized_generic_and_returning_harnesses_are_not_authoritative() -> TestResult {
    for header in ["fn ui(_: ())", "fn ui<T>()", "fn ui() -> ()", "fn r#ui()"] {
        let source = format!("#[test]\n{header} {{\n{}\n}}\n", selected_body());
        let result = authority_analysis(TRYBUILD_MANIFEST, TRYBUILD_LOCK, &source)?;
        assert_unclassified(&result, header);
    }
    Ok(())
}

#[test]
fn non_registry_dependency_forms_and_explicit_tests_are_rejected() -> TestResult {
    let prefix = "[workspace]\n[package]\nname = \"app\"\nedition = \"2024\"\nbuild = false\n";
    let manifests = [
        format!("{prefix}[dev-dependencies]\ntrybuild = {{ path = \"vendor\" }}\n"),
        format!("{prefix}[dev-dependencies]\ntrybuild = {{ git = \"https://invalid\" }}\n"),
        format!("{prefix}[dev-dependencies]\ntrybuild = {{ workspace = true }}\n"),
        format!(
            "{prefix}[dev-dependencies]\ntrybuild = {{ package = \"other\", version = \"1\" }}\n"
        ),
        format!("{prefix}[dev-dependencies]\ntrybuild = \"^1\"\n"),
        format!(
            "{prefix}[dev-dependencies]\ntrybuild = \"1\"\n[[test]]\nname = \"harness\"\npath = \"tests/harness.rs\"\nharness = false\n"
        ),
        format!(
            "{prefix}[dev-dependencies]\ntrybuild = \"1\"\n[patch.crates-io]\nglob = {{ path = \"vendor/glob\" }}\n"
        ),
        format!(
            "{prefix}[dev-dependencies]\ntrybuild = \"1\"\n[replace]\n\"trybuild:1.0.117\" = {{ path = \"vendor/trybuild\" }}\n"
        ),
    ];
    for manifest in manifests {
        assert_dependency_failure(&manifest, TRYBUILD_LOCK)?;
    }
    Ok(())
}

#[test]
fn any_pinned_lock_tuple_or_edge_drift_is_rejected() -> TestResult {
    let locks = [
        TRYBUILD_LOCK.replacen("1.0.117", "1.0.116", 1),
        TRYBUILD_LOCK.replacen(
            "registry+https://github.com/rust-lang/crates.io-index",
            "git+https://invalid",
            1,
        ),
        TRYBUILD_LOCK.replacen(
            "0710d4dfbeae4f9c390baa784c49858a7468fa433f3fe5d0ec5ebef651cf59f9",
            "1710d4dfbeae4f9c390baa784c49858a7468fa433f3fe5d0ec5ebef651cf59f9",
            1,
        ),
        TRYBUILD_LOCK.replacen("dependencies = [\"trybuild\"]", "dependencies = []", 1),
    ];
    for lock in locks {
        assert_dependency_failure(TRYBUILD_MANIFEST, &lock)?;
    }
    Ok(())
}

#[test]
fn repository_cargo_source_authority_is_rejected() -> TestResult {
    for (config_path, config) in [
        (".cargo/config", "paths = [\"vendor\"]\n"),
        (
            ".cargo/config.toml",
            "[patch.crates-io]\ntrybuild = { path = \"vendor/trybuild\" }\n",
        ),
        (
            ".cargo/config.toml",
            "[source.crates-io]\nreplace-with = \"mirror\"\n",
        ),
        (
            ".cargo/config.toml",
            "[registries.private]\nindex = \"https://invalid\"\n",
        ),
    ] {
        let source = selected_harness();
        let (_, _, result) = analyze(
            &[
                ("Cargo.toml", TRYBUILD_MANIFEST),
                ("Cargo.lock", TRYBUILD_LOCK),
                (config_path, config),
                ("src/lib.rs", ""),
                ("tests/harness.rs", &source),
                ("tests/ui/case.rs", "fn main() {}"),
            ],
            &GeneratedIncludeRegistry::empty(),
        )?;
        assert!(has_code(
            &result,
            ModuleDiagnosticCode::TrybuildDependencyUnverified
        ));
        assert!(file(&result, "tests/ui/case.rs").is_none());
    }
    Ok(())
}

fn authority_analysis(
    manifest: &str,
    lock: &str,
    source: &str,
) -> Result<ModuleAnalysis, Box<dyn std::error::Error>> {
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", manifest),
            ("Cargo.lock", lock),
            ("src/lib.rs", ""),
            ("tests/harness.rs", source),
            ("tests/ui/case.rs", "fn main() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;
    Ok(result)
}

fn assert_dependency_failure(manifest: &str, lock: &str) -> TestResult {
    let result = authority_analysis(manifest, lock, &selected_harness())?;
    assert!(has_code(
        &result,
        ModuleDiagnosticCode::TrybuildDependencyUnverified
    ));
    assert!(file(&result, "tests/ui/case.rs").is_none());
    Ok(())
}

fn assert_unclassified(result: &ModuleAnalysis, context: &str) {
    assert!(
        file(result, "tests/ui/case.rs").is_none(),
        "{context} unexpectedly selected the fixture"
    );
    assert!(has_diagnostic(
        result,
        ModuleDiagnosticCode::UnclassifiedRustSource,
        "tests/ui/case.rs"
    ));
}

fn selected_harness() -> String {
    harness("cases.compile_fail(\"tests/ui/case.rs\");")
}

fn selected_body() -> &'static str {
    "let cases = trybuild::TestCases::new();\ncases.compile_fail(\"tests/ui/case.rs\");"
}
