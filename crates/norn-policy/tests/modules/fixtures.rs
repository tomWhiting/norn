use norn_policy::EntryKind;
use norn_policy::facts::analyze_facts;
use norn_policy::rust::modules::{
    CompileTestExpectation, GeneratedIncludeRegistry, ModuleDiagnosticCode,
};

use super::support::{TestResult, analyze, analyze_kinds, file};

pub(super) const TRYBUILD_MANIFEST: &str = concat!(
    "[workspace]\n",
    "[package]\nname = \"app\"\nedition = \"2024\"\nbuild = false\n",
    "[dev-dependencies]\ntrybuild = \"1\"\n",
);
const PARENT_MANIFEST: &str = concat!(
    "[package]\nname = \"parent\"\nedition = \"2024\"\nbuild = false\n",
    "[dev-dependencies]\ntrybuild = \"1\"\n",
);
pub(super) const TRYBUILD_LOCK: &str = r#"
version = 4

[[package]]
name = "app"
version = "0.0.0"
dependencies = ["trybuild"]

[[package]]
name = "trybuild"
version = "1.0.117"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0710d4dfbeae4f9c390baa784c49858a7468fa433f3fe5d0ec5ebef651cf59f9"
"#;
const PARENT_LOCK: &str = r#"
version = 4

[[package]]
name = "parent"
version = "0.0.0"
dependencies = ["trybuild"]

[[package]]
name = "trybuild"
version = "1.0.117"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0710d4dfbeae4f9c390baa784c49858a7468fa433f3fe5d0ec5ebef651cf59f9"
"#;

const HARNESS_PREFIX: &str = r#"
    #[test]
    fn ui() {
        let cases = trybuild::TestCases::new();
"#;

pub(super) fn harness(body: &str) -> String {
    format!("{HARNESS_PREFIX}{body}\n    }}\n")
}

#[test]
fn base_shaped_eleven_file_glob_classifies_all_roots_and_debt() -> TestResult {
    let source = harness(r#"cases.compile_fail("tests/ui/*.rs");"#);
    let fixture = "#![allow(dead_code)]\nfn main() {}\n";
    let (snapshot, _, result) = analyze(
        &[
            ("Cargo.toml", TRYBUILD_MANIFEST),
            ("Cargo.lock", TRYBUILD_LOCK),
            ("src/lib.rs", ""),
            ("tests/harness.rs", &source),
            ("tests/ui/external_tagging.rs", fixture),
            ("tests/ui/flatten_in_variant.rs", fixture),
            ("tests/ui/flatten_non_struct.rs", fixture),
            ("tests/ui/generic_struct.rs", fixture),
            ("tests/ui/hashmap_non_string_key.rs", fixture),
            ("tests/ui/missing_doc.rs", fixture),
            ("tests/ui/tuple_variant.rs", fixture),
            ("tests/ui/unknown_rename_rule.rs", fixture),
            ("tests/ui/unknown_tool_args_key.rs", fixture),
            ("tests/ui/unsupported_type.rs", fixture),
            ("tests/ui/untagged_duplicate_unit.rs", fixture),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(result.is_valid(), "{:#?}", result.diagnostics);
    let roots = result
        .files
        .iter()
        .filter(|entry| entry.path.as_str().starts_with("tests/ui/"))
        .collect::<Vec<_>>();
    assert_eq!(roots.len(), 11);
    assert!(roots.iter().all(|entry| {
        !entry.production
            && entry.test_only
            && entry.test_targets.len() == 1
            && entry.test_targets[0].root.as_str() == "tests/harness.rs"
    }));
    let expected_paths = [
        "tests/ui/external_tagging.rs",
        "tests/ui/flatten_in_variant.rs",
        "tests/ui/flatten_non_struct.rs",
        "tests/ui/generic_struct.rs",
        "tests/ui/hashmap_non_string_key.rs",
        "tests/ui/missing_doc.rs",
        "tests/ui/tuple_variant.rs",
        "tests/ui/unknown_rename_rule.rs",
        "tests/ui/unknown_tool_args_key.rs",
        "tests/ui/unsupported_type.rs",
        "tests/ui/untagged_duplicate_unit.rs",
    ];
    assert_eq!(
        result
            .compile_test_fixtures
            .iter()
            .map(|fact| fact.path.as_str())
            .collect::<Vec<_>>(),
        expected_paths
    );
    assert!(result.compile_test_fixtures.iter().all(|fact| {
        fact.expectation == CompileTestExpectation::CompileFail
            && fact.harness.root.as_str() == "tests/harness.rs"
    }));
    let facts = analyze_facts(&snapshot, &GeneratedIncludeRegistry::empty());
    assert!(facts.failures().is_empty(), "{:#?}", facts.failures());
    assert_eq!(facts.debt().len(), 11);
    assert_eq!(facts.compile_test_fixtures(), result.compile_test_fixtures);
    Ok(())
}

#[test]
fn duplicate_and_conflicting_fixture_selection_fail_closed() -> TestResult {
    for (second, expected) in [
        (
            r#"cases.compile_fail("tests/ui/case.rs");"#,
            ModuleDiagnosticCode::TrybuildFixtureDuplicate,
        ),
        (
            r#"cases.pass("tests/ui/case.rs");"#,
            ModuleDiagnosticCode::TrybuildExpectationConflict,
        ),
    ] {
        let source = harness(&format!(
            "cases.compile_fail(\"tests/ui/case.rs\");\n{second}"
        ));
        let (_, _, result) = analyze(
            &[
                ("Cargo.toml", TRYBUILD_MANIFEST),
                ("Cargo.lock", TRYBUILD_LOCK),
                ("src/lib.rs", ""),
                ("tests/harness.rs", &source),
                ("tests/ui/case.rs", "fn main() {}"),
            ],
            &GeneratedIncludeRegistry::empty(),
        )?;
        assert!(has_code(&result, expected));
        assert!(result.compile_test_fixtures.is_empty());
    }
    Ok(())
}

#[test]
fn pass_and_compile_fail_are_distinct_provenance() -> TestResult {
    for (method, expectation) in [
        ("compile_fail", CompileTestExpectation::CompileFail),
        ("pass", CompileTestExpectation::Pass),
    ] {
        let source = harness(&format!("cases.{method}(\"tests/ui/case.rs\");"));
        let (_, _, result) = analyze(
            &[
                ("Cargo.toml", TRYBUILD_MANIFEST),
                ("Cargo.lock", TRYBUILD_LOCK),
                ("src/lib.rs", ""),
                ("tests/harness.rs", &source),
                ("tests/ui/case.rs", "fn main() {}"),
            ],
            &GeneratedIncludeRegistry::empty(),
        )?;
        assert!(result.is_valid(), "{:#?}", result.diagnostics);
        assert_eq!(result.compile_test_fixtures.len(), 1);
        assert_eq!(result.compile_test_fixtures[0].expectation, expectation);
    }
    Ok(())
}

#[test]
fn fixture_addition_deletion_and_classification_drift_are_observable() -> TestResult {
    let exact = harness(r#"cases.compile_fail("tests/ui/case.rs");"#);
    let glob = harness(r#"cases.compile_fail("tests/ui/*.rs");"#);
    let common = [
        ("Cargo.toml", TRYBUILD_MANIFEST),
        ("Cargo.lock", TRYBUILD_LOCK),
        ("src/lib.rs", ""),
    ];
    let (_, _, baseline) = analyze(
        &[
            common[0],
            common[1],
            common[2],
            ("tests/harness.rs", &exact),
            ("tests/ui/case.rs", "fn main() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;
    let (_, _, added) = analyze(
        &[
            common[0],
            common[1],
            common[2],
            ("tests/harness.rs", &glob),
            ("tests/ui/case.rs", "fn main() {}"),
            ("tests/ui/extra.rs", "fn main() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;
    let (_, _, deleted) = analyze(
        &[
            common[0],
            common[1],
            common[2],
            ("tests/harness.rs", &exact),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert_eq!(baseline.compile_test_fixtures.len(), 1);
    assert_eq!(added.compile_test_fixtures.len(), 2);
    assert_ne!(baseline.compile_test_fixtures, added.compile_test_fixtures);
    assert!(deleted.compile_test_fixtures.is_empty());
    assert!(has_code(
        &deleted,
        ModuleDiagnosticCode::TrybuildFixtureMissing
    ));
    Ok(())
}

#[test]
fn unused_file_in_selected_subtree_remains_unclassified() -> TestResult {
    let source = harness(r#"cases.compile_fail("tests/ui/selected.rs");"#);
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", TRYBUILD_MANIFEST),
            ("Cargo.lock", TRYBUILD_LOCK),
            ("src/lib.rs", ""),
            ("tests/harness.rs", &source),
            ("tests/ui/selected.rs", "fn main() {}"),
            ("tests/ui/unused.rs", "fn unused() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(file(&result, "tests/ui/selected.rs").is_some());
    assert!(file(&result, "tests/ui/unused.rs").is_none());
    assert!(has_diagnostic(
        &result,
        ModuleDiagnosticCode::UnclassifiedRustSource,
        "tests/ui/unused.rs"
    ));
    Ok(())
}

#[test]
fn dynamic_empty_broad_and_escaping_selectors_fail_closed() -> TestResult {
    let source = harness(
        r#"
        cases.compile_fail("tests/ui/case.rs");
        cases.compile_fail(selector());
        cases.compile_fail("");
        cases.compile_fail("tests/*.rs");
        cases.compile_fail("../outside.rs");
        cases.compile_fail("tests/missing/*.rs");
    "#,
    );
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", TRYBUILD_MANIFEST),
            ("Cargo.lock", TRYBUILD_LOCK),
            ("src/lib.rs", ""),
            ("tests/harness.rs", &source),
            ("tests/ui/case.rs", "fn main() {}"),
            ("outside.rs", "fn main() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(has_code(
        &result,
        ModuleDiagnosticCode::TrybuildSelectorUnsupported
    ));
    assert!(has_code(&result, ModuleDiagnosticCode::AuthorityEscape));
    assert!(has_code(
        &result,
        ModuleDiagnosticCode::TrybuildFixtureMissing
    ));
    assert!(file(&result, "tests/ui/case.rs").is_none());
    assert!(has_diagnostic(
        &result,
        ModuleDiagnosticCode::UnclassifiedRustSource,
        "tests/ui/case.rs"
    ));
    Ok(())
}

#[test]
fn shadowed_binding_cannot_select_a_fixture() -> TestResult {
    let source = format!(
        "{HARNESS_PREFIX}let cases = other();\ncases.compile_fail(\"tests/ui/case.rs\");\n}}\n"
    );
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", TRYBUILD_MANIFEST),
            ("Cargo.lock", TRYBUILD_LOCK),
            ("src/lib.rs", ""),
            ("tests/harness.rs", &source),
            ("tests/ui/case.rs", "fn main() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(file(&result, "tests/ui/case.rs").is_none());
    assert!(has_diagnostic(
        &result,
        ModuleDiagnosticCode::UnclassifiedRustSource,
        "tests/ui/case.rs"
    ));
    Ok(())
}

#[test]
fn selector_cannot_cross_into_a_nested_package() -> TestResult {
    let root = "[workspace]\nmembers = [\"app\", \"app/tests/embedded\"]\n";
    let parent = PARENT_MANIFEST;
    let child = "[package]\nname = \"child\"\nedition = \"2024\"\nbuild = false\n";
    let source = harness(r#"cases.compile_fail("tests/embedded/tests/ui/case.rs");"#);
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", root),
            ("Cargo.lock", PARENT_LOCK),
            ("app/Cargo.toml", parent),
            ("app/src/lib.rs", ""),
            ("app/tests/harness.rs", &source),
            ("app/tests/embedded/Cargo.toml", child),
            ("app/tests/embedded/src/lib.rs", ""),
            ("app/tests/embedded/tests/ui/case.rs", "fn main() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(has_code(&result, ModuleDiagnosticCode::AuthorityEscape));
    assert!(file(&result, "app/tests/embedded/tests/ui/case.rs").is_none());
    Ok(())
}

#[test]
fn nonregular_selected_fixture_is_rejected_before_traversal() -> TestResult {
    let source = harness(r#"cases.compile_fail("tests/ui/case.rs");"#);
    let result = analyze_kinds(&[
        ("Cargo.toml", EntryKind::Regular, TRYBUILD_MANIFEST),
        ("Cargo.lock", EntryKind::Regular, TRYBUILD_LOCK),
        ("src/lib.rs", EntryKind::Regular, ""),
        ("tests/harness.rs", EntryKind::Regular, &source),
        ("tests/ui/case.rs", EntryKind::Symlink, "elsewhere.rs"),
    ])?;

    assert!(has_diagnostic(
        &result,
        ModuleDiagnosticCode::EntryNotRegular,
        "tests/ui/case.rs"
    ));
    assert!(file(&result, "tests/ui/case.rs").is_none());
    Ok(())
}

#[test]
fn production_reference_remains_visible_when_trybuild_selects_same_file() -> TestResult {
    let source = harness(r#"cases.pass("tests/ui/shared.rs");"#);
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", TRYBUILD_MANIFEST),
            ("Cargo.lock", TRYBUILD_LOCK),
            (
                "src/lib.rs",
                "#[path = \"../tests/ui/shared.rs\"] mod shared;",
            ),
            ("tests/harness.rs", &source),
            ("tests/ui/shared.rs", "pub fn shared() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(has_code(
        &result,
        ModuleDiagnosticCode::TrybuildFixtureClassification
    ));
    assert!(result.compile_test_fixtures.is_empty());
    let shared = file(&result, "tests/ui/shared.rs").ok_or("shared source not classified")?;
    assert!(shared.production);
    assert!(shared.test_only);
    assert_eq!(shared.production_targets.len(), 1);
    assert_eq!(shared.test_targets.len(), 1);
    Ok(())
}

#[test]
fn synthetic_fixture_root_evaluates_test_cfg_as_false() -> TestResult {
    let source = harness(r#"cases.compile_fail("tests/ui/case.rs");"#);
    let fixture = "#[cfg(not(test))]\nmod active;\n#[cfg(test)]\nmod inactive;\n";
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", TRYBUILD_MANIFEST),
            ("Cargo.lock", TRYBUILD_LOCK),
            ("src/lib.rs", ""),
            ("tests/harness.rs", &source),
            ("tests/ui/case.rs", fixture),
            ("tests/ui/active.rs", "fn active() {}"),
            ("tests/ui/inactive.rs", "fn inactive() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(file(&result, "tests/ui/active.rs").is_some_and(|file| file.test_only));
    assert!(file(&result, "tests/ui/inactive.rs").is_none());
    assert!(has_diagnostic(
        &result,
        ModuleDiagnosticCode::UnclassifiedRustSource,
        "tests/ui/inactive.rs"
    ));
    Ok(())
}

#[test]
fn fixture_root_recursively_resolves_modules_and_literal_includes() -> TestResult {
    let source = harness(r#"cases.compile_fail("tests/ui/case.rs");"#);
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", TRYBUILD_MANIFEST),
            ("Cargo.lock", TRYBUILD_LOCK),
            ("src/lib.rs", ""),
            ("tests/harness.rs", &source),
            (
                "tests/ui/case.rs",
                "#[path = \"support/helper.rs\"]\nmod helper;\ninclude!(\"included.rs\");\n",
            ),
            ("tests/ui/support/helper.rs", "fn helper() {}"),
            ("tests/ui/included.rs", "fn included() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(result.is_valid(), "{:#?}", result.diagnostics);
    for path in [
        "tests/ui/case.rs",
        "tests/ui/support/helper.rs",
        "tests/ui/included.rs",
    ] {
        assert!(file(&result, path).is_some_and(|file| !file.production && file.test_only));
    }
    Ok(())
}

#[test]
fn recursive_fixture_edge_cannot_hide_a_nested_package_crossing() -> TestResult {
    let root = "[workspace]\nmembers = [\"app\", \"app/tests/ui/nested\"]\n";
    let parent = PARENT_MANIFEST;
    let child = "[package]\nname = \"child\"\nedition = \"2024\"\nbuild = false\n";
    let source = harness(r#"cases.compile_fail("tests/ui/case.rs");"#);
    let fixture = "#[path = \"nested/src/lib.rs\"]\nmod nested;\n";
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", root),
            ("Cargo.lock", PARENT_LOCK),
            ("app/Cargo.toml", parent),
            ("app/src/lib.rs", ""),
            ("app/tests/harness.rs", &source),
            ("app/tests/ui/case.rs", fixture),
            ("app/tests/ui/nested/Cargo.toml", child),
            ("app/tests/ui/nested/src/lib.rs", "pub fn nested() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(has_diagnostic(
        &result,
        ModuleDiagnosticCode::UnclassifiedRustSource,
        "app/tests/ui/nested/src/lib.rs"
    ));
    let nested = file(&result, "app/tests/ui/nested/src/lib.rs")
        .ok_or("nested package target should retain its own reachability")?;
    assert!(nested.production);
    assert!(nested.test_only);
    Ok(())
}

pub(super) fn has_code(
    result: &norn_policy::rust::modules::ModuleAnalysis,
    code: ModuleDiagnosticCode,
) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code)
}

pub(super) fn has_diagnostic(
    result: &norn_policy::rust::modules::ModuleAnalysis,
    code: ModuleDiagnosticCode,
    path: &str,
) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code && diagnostic.path.as_str() == path)
}
