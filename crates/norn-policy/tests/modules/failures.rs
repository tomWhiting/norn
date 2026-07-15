use norn_policy::EntryKind;
use norn_policy::rust::modules::{GeneratedIncludeRegistry, ModuleDiagnosticCode};

use super::support::{TestResult, analyze, analyze_kinds, has_code, standard_manifest};

#[test]
fn standard_dual_layout_is_ambiguous() -> TestResult {
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", "mod duplicate;"),
            ("src/duplicate.rs", "pub fn direct() {}"),
            ("src/duplicate/mod.rs", "pub fn nested() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;
    assert!(has_code(&result, ModuleDiagnosticCode::ModuleAmbiguous));
    Ok(())
}

#[test]
fn include_cycles_fail_closed() -> TestResult {
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", "include!(\"a.rs\");"),
            ("src/a.rs", "include!(\"b.rs\");"),
            ("src/b.rs", "include!(\"a.rs\");"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;
    assert!(has_code(&result, ModuleDiagnosticCode::ResolutionCycle));
    Ok(())
}

#[test]
fn escapes_nonregular_targets_and_unclassified_sources_fail() -> TestResult {
    let result = analyze_kinds(&[
        ("Cargo.toml", EntryKind::Regular, standard_manifest()),
        (
            "src/lib.rs",
            EntryKind::Regular,
            "#[path = \"../../outside.rs\"] mod escaped;\nmod linked;",
        ),
        ("src/linked.rs", EntryKind::Symlink, "elsewhere.rs"),
        ("src/orphan.rs", EntryKind::Regular, "pub fn hidden() {}"),
        ("outside.rs", EntryKind::Regular, "pub fn outside() {}"),
    ])?;
    assert!(has_code(&result, ModuleDiagnosticCode::AuthorityEscape));
    assert!(has_code(&result, ModuleDiagnosticCode::EntryNotRegular));
    assert!(has_code(
        &result,
        ModuleDiagnosticCode::UnclassifiedRustSource
    ));
    Ok(())
}

#[test]
fn conflicting_possible_paths_and_nonliteral_includes_fail() -> TestResult {
    let source = r#"
        #[cfg_attr(feature = "one", path = "one.rs")]
        #[cfg_attr(feature = "two", path = "two.rs")]
        mod conflict;
        #[path = concat!("dynamic", ".rs")]
        mod nonliteral_path;
        include!(concat!("dynamic", ".rs"));
    "#;
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", source),
            ("src/conflict.rs", "pub fn default() {}"),
            ("src/one.rs", "pub fn one() {}"),
            ("src/two.rs", "pub fn two() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;
    assert!(has_code(&result, ModuleDiagnosticCode::PathConflict));
    assert!(has_code(&result, ModuleDiagnosticCode::PathNonliteral));
    assert!(has_code(&result, ModuleDiagnosticCode::IncludeUnsupported));
    Ok(())
}

#[test]
fn missing_modules_and_parse_errors_are_typed() -> TestResult {
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            (
                "src/lib.rs",
                "mod missing;\n#[cfg_attr(feature = \"missing\", path = \"absent.rs\")] mod present;\nmod malformed;",
            ),
            ("src/present.rs", "pub fn present() {}"),
            ("src/malformed.rs", "fn broken( {"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;
    assert!(has_code(&result, ModuleDiagnosticCode::ModuleMissing));
    assert!(has_code(&result, ModuleDiagnosticCode::SourceParse));
    Ok(())
}
