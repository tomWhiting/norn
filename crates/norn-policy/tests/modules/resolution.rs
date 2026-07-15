use norn_policy::rust::modules::{GeneratedIncludeRegistry, ModuleTargetKind};

use super::support::{TestResult, analyze, file, has_code, standard_manifest};
use norn_policy::rust::modules::ModuleDiagnosticCode;

#[test]
fn nested_inline_path_include_and_shared_references_are_classified() -> TestResult {
    let source = r#"
        mod direct;
        mod nested;
        mod inline { mod child; }
        include!("included.rs");

        #[cfg(test)]
        #[path = "test_only.rs"]
        mod test_support;

        #[path = r"shared.rs"]
        mod shared_production;
        #[cfg(test)]
        #[path = "shared.rs"]
        mod shared_test;
    "#;
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", source),
            ("src/direct.rs", "mod leaf;"),
            ("src/direct/leaf.rs", "pub fn leaf() {}"),
            ("src/nested/mod.rs", "pub fn nested() {}"),
            ("src/inline/child.rs", "pub fn child() {}"),
            ("src/included.rs", "mod from_include;"),
            ("src/from_include.rs", "pub fn included() {}"),
            ("src/test_only.rs", "fn helper() {}"),
            ("src/shared.rs", "pub fn shared() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(result.is_valid(), "{:#?}", result.diagnostics);
    let test_only = file(&result, "src/test_only.rs").ok_or("test source not classified")?;
    assert!(!test_only.production);
    assert!(test_only.test_only);
    assert!(test_only.production_targets.is_empty());
    assert_eq!(test_only.test_targets.len(), 1);
    assert_eq!(test_only.test_targets[0].kind, ModuleTargetKind::Library);
    let shared = file(&result, "src/shared.rs").ok_or("shared source not classified")?;
    assert!(shared.production);
    assert!(shared.test_only);
    assert_eq!(shared.production_targets.len(), 1);
    assert_eq!(shared.test_targets.len(), 1);
    for production in [
        "src/direct.rs",
        "src/direct/leaf.rs",
        "src/nested/mod.rs",
        "src/inline/child.rs",
        "src/included.rs",
        "src/from_include.rs",
    ] {
        assert!(file(&result, production).is_some_and(|value| value.production));
    }
    Ok(())
}

#[test]
fn cfg_attr_unions_possible_production_and_test_specific_paths() -> TestResult {
    let source = r#"
        #[cfg_attr(feature = "alternate", path = "alternate.rs")]
        mod selectable;

        #[cfg_attr(test, path = "test_variant.rs")]
        mod variant;
    "#;
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", source),
            ("src/selectable.rs", "pub fn default() {}"),
            ("src/alternate.rs", "pub fn alternate() {}"),
            ("src/variant.rs", "pub fn production() {}"),
            ("src/test_variant.rs", "fn test_variant() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(result.is_valid(), "{:#?}", result.diagnostics);
    assert!(file(&result, "src/selectable.rs").is_some_and(|value| value.production));
    assert!(file(&result, "src/alternate.rs").is_some_and(|value| value.production));
    assert!(file(&result, "src/variant.rs").is_some_and(|value| value.production));
    let test_variant = file(&result, "src/test_variant.rs").ok_or("test variant missing")?;
    assert!(!test_variant.production);
    assert!(test_variant.test_only);
    Ok(())
}

#[test]
fn production_to_test_reclassification_remains_visible_for_origin_comparison() -> TestResult {
    let base = [
        ("Cargo.toml", standard_manifest()),
        ("src/lib.rs", "mod guarded;"),
        ("src/guarded.rs", "pub fn guarded() {}"),
    ];
    let current = [
        ("Cargo.toml", standard_manifest()),
        ("src/lib.rs", "#[cfg(test)]\nmod guarded;"),
        ("src/guarded.rs", "pub fn guarded() {}"),
    ];
    let (_, _, base_result) = analyze(&base, &GeneratedIncludeRegistry::empty())?;
    let (_, _, current_result) = analyze(&current, &GeneratedIncludeRegistry::empty())?;

    let before = file(&base_result, "src/guarded.rs").ok_or("base classification missing")?;
    let after = file(&current_result, "src/guarded.rs").ok_or("current classification missing")?;
    assert!(before.production);
    assert!(!after.production);
    assert!(after.test_only);
    Ok(())
}

#[test]
fn inline_module_path_attribute_sets_the_nested_module_directory() -> TestResult {
    let source = r#"
        #[path = "thread_files"]
        mod thread {
            #[path = "tls.rs"]
            mod local_data;
        }
    "#;
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", source),
            ("src/thread_files/tls.rs", "pub fn local() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(result.is_valid(), "{:#?}", result.diagnostics);
    assert!(file(&result, "src/thread_files/tls.rs").is_some_and(|value| value.production));
    Ok(())
}

#[test]
fn inline_path_in_non_mod_rs_source_uses_the_semantic_module_directory() -> TestResult {
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", "mod outer;"),
            (
                "src/outer.rs",
                "mod first { #[path = \"selected.rs\"] mod leaf; }",
            ),
            ("src/outer/first/selected.rs", "pub fn selected() {}"),
            ("src/selected.rs", "pub fn decoy() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(file(&result, "src/outer/first/selected.rs").is_some_and(|value| value.production));
    assert!(file(&result, "src/selected.rs").is_none());
    assert!(has_code(
        &result,
        ModuleDiagnosticCode::UnclassifiedRustSource
    ));
    Ok(())
}

#[test]
fn custom_extension_crate_root_is_mod_rs_for_inline_path_resolution() -> TestResult {
    let manifest = concat!(
        "[workspace]\n",
        "[package]\nname = \"app\"\nedition = \"2024\"\nbuild = false\n",
        "[lib]\npath = \"roots/crate.entry\"\n",
    );
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", manifest),
            (
                "roots/crate.entry",
                "mod first { mod second { #[path = \"selected.rs\"] mod leaf; } }",
            ),
            ("roots/first/second/selected.rs", "pub fn selected() {}"),
            ("roots/selected.rs", "pub fn decoy() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(file(&result, "roots/first/second/selected.rs").is_some_and(|value| value.production));
    assert!(file(&result, "roots/selected.rs").is_none());
    assert!(has_code(
        &result,
        ModuleDiagnosticCode::UnclassifiedRustSource
    ));
    Ok(())
}

#[test]
fn path_selected_mod_rs_uses_its_physical_directory_for_inline_children() -> TestResult {
    let source = r#"
        #[path = "renamed/mod.rs"]
        mod logical;
    "#;
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", source),
            (
                "src/renamed/mod.rs",
                "mod inline { #[path = \"chosen.rs\"] mod leaf; }",
            ),
            ("src/renamed/inline/chosen.rs", "pub fn selected() {}"),
            (
                "src/logical/inline/chosen.rs",
                "pub fn decoy_for_logical_directory() {}",
            ),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(file(&result, "src/renamed/inline/chosen.rs").is_some_and(|value| value.production));
    assert!(file(&result, "src/logical/inline/chosen.rs").is_none());
    assert!(has_code(
        &result,
        ModuleDiagnosticCode::UnclassifiedRustSource
    ));
    Ok(())
}

#[test]
fn raw_cfg_names_classify_outer_inner_and_nested_test_modules() -> TestResult {
    let source = r"
        #[r#cfg(r#test)]
        mod outer;

        mod inline {
            #![r#cfg(test)]
            mod inner;
        }

        #[r#cfg_attr(all(), r#cfg(test))]
        mod nested;
    ";
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", source),
            ("src/outer.rs", "fn outer() {}"),
            ("src/inline/inner.rs", "fn inner() {}"),
            ("src/nested.rs", "fn nested() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(result.is_valid(), "{:#?}", result.diagnostics);
    for path in ["src/outer.rs", "src/inline/inner.rs", "src/nested.rs"] {
        let reached = file(&result, path).ok_or("raw cfg source not classified")?;
        assert!(!reached.production, "{path}");
        assert!(reached.test_only, "{path}");
    }
    Ok(())
}

#[test]
fn raw_path_and_include_names_resolve_as_their_semantic_builtins() -> TestResult {
    let source = r#"
        #[r#path = "selected.rs"]
        mod logical;
        r#include!("included.rs");
    "#;
    let (_, _, result) = analyze(
        &[
            ("Cargo.toml", standard_manifest()),
            ("src/lib.rs", source),
            ("src/selected.rs", "pub fn selected() {}"),
            ("src/included.rs", "pub fn included() {}"),
        ],
        &GeneratedIncludeRegistry::empty(),
    )?;

    assert!(result.is_valid(), "{:#?}", result.diagnostics);
    assert!(file(&result, "src/selected.rs").is_some_and(|value| value.production));
    assert!(file(&result, "src/included.rs").is_some_and(|value| value.production));
    Ok(())
}
