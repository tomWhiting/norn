use std::collections::BTreeMap;

use norn_policy::debt::{DebtConstructKind, DebtOccurrence, DebtScanError};

use crate::support::{TestResult, attribute, macro_call, marker, method_call, scan};

#[test]
fn attributes_cover_outer_inner_nested_and_impossible_forms() -> TestResult {
    let allow = attribute("allow", "dead_code");
    let inner_expect = format!("#![{}(unused_variables)]", "expect");
    let ignore = format!("#[{}]", "ignore");
    let impossible = attribute("cfg", "any()");
    let nested_allow = attribute("cfg_attr", "feature = \"x\", allow(unused)");
    let nested_impossible = attribute("cfg_attr", "all(unix, not(unix)), expect(dead_code)");
    let source = format!(
        "{inner_expect}\n{allow}\n{ignore}\n{impossible}\n{nested_allow}\n{nested_impossible}\nfn run() {{}}\n",
    );
    let occurrences = scan(&source)?;
    let mut counts = BTreeMap::new();
    for occurrence in occurrences {
        *counts.entry(occurrence.construct()).or_insert(0_u32) += 1;
    }
    assert_eq!(counts.get(&DebtConstructKind::AllowAttribute), Some(&2));
    assert_eq!(counts.get(&DebtConstructKind::ExpectAttribute), Some(&2));
    assert_eq!(counts.get(&DebtConstructKind::IgnoreAttribute), Some(&1));
    assert_eq!(counts.get(&DebtConstructKind::ImpossibleCfg), Some(&2));
    Ok(())
}

#[test]
fn all_method_and_macro_forms_are_closed_and_typed() -> TestResult {
    let methods = ["unwrap", "unwrap_err", "expect", "expect_err"];
    let macros = ["panic", "todo", "unimplemented", "unreachable"];
    let mut body = String::new();
    for name in methods {
        body.push_str(&method_call(name));
        body.push_str(";\n");
    }
    for name in macros {
        body.push_str(&macro_call(name));
        body.push_str(";\n");
    }
    body.push_str("assert!(");
    body.push_str(&method_call("unwrap"));
    body.push_str(");\n");
    let occurrences = scan(&format!("fn run() {{\n{body}}}\n"))?;
    let kinds: Vec<DebtConstructKind> = occurrences.iter().map(DebtOccurrence::construct).collect();
    for expected in [
        DebtConstructKind::UnwrapCall,
        DebtConstructKind::UnwrapErrCall,
        DebtConstructKind::ExpectCall,
        DebtConstructKind::ExpectErrCall,
        DebtConstructKind::PanicMacro,
        DebtConstructKind::TodoMacro,
        DebtConstructKind::UnimplementedMacro,
        DebtConstructKind::UnreachableMacro,
    ] {
        assert!(kinds.contains(&expected));
    }
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == DebtConstructKind::UnwrapCall)
            .count(),
        2
    );
    assert_eq!(kinds.len(), 9);
    Ok(())
}

#[test]
fn associated_and_ufcs_calls_cannot_bypass_method_debt() -> TestResult {
    let source = format!(
        "fn run(value: Result<u8, u8>) {{\n Result::{first}(value);\n <Result<u8, u8>>::{second}(value, \"message\");\n Result::<u8, u8>::r#{third}(value);\n}}\n",
        first = "unwrap",
        second = "expect",
        third = "unwrap_err",
    );
    let occurrences = scan(&source)?;
    let kinds: Vec<_> = occurrences.iter().map(DebtOccurrence::construct).collect();

    assert!(kinds.contains(&DebtConstructKind::UnwrapCall));
    assert!(kinds.contains(&DebtConstructKind::ExpectCall));
    assert!(kinds.contains(&DebtConstructKind::UnwrapErrCall));
    assert_eq!(kinds.len(), 3);
    Ok(())
}

#[test]
fn macro_wrappers_cannot_hide_method_or_associated_debt() -> TestResult {
    let method = "unwrap";
    let associated = "expect_err";
    let source = format!(
        "macro_rules! wrapped {{ ($value:expr) => {{ $value.{method}(); Result::r#{associated}($value, \"message\"); }}; }}\n",
    );
    let occurrences = scan(&source)?;
    let kinds: Vec<_> = occurrences.iter().map(DebtOccurrence::construct).collect();

    assert!(kinds.contains(&DebtConstructKind::UnwrapCall));
    assert!(kinds.contains(&DebtConstructKind::ExpectErrCall));
    assert_eq!(kinds.len(), 2);
    Ok(())
}

#[test]
fn named_bindings_cover_pattern_sites_but_bare_wildcards_are_excluded() -> TestResult {
    let parameter = ["_", "parameter"].concat();
    let tuple = ["_", "tuple"].concat();
    let field = ["_", "field"].concat();
    let shorthand = ["_", "shorthand"].concat();
    let entry = ["_", "entry"].concat();
    let inside = ["_", "inside"].concat();
    let arm = ["_", "arm"].concat();
    let closure = ["_", "closure"].concat();
    let source = format!(
        "fn run({parameter}: u8) {{\n    let ({tuple}, _) = (1, 2);\n    let Struct {{ field: {field}, shorthand: {shorthand} }} = value;\n    for {entry} in values {{}}\n    if let Some({inside}) = value {{}}\n    match value {{ Some({arm}) => {{}}, None => {{}} }}\n    let closure = |{closure}, _| {closure};\n}}\n"
    );
    let occurrences = scan(&source)?;
    let bindings: Vec<_> = occurrences
        .iter()
        .filter(|occurrence| occurrence.construct() == DebtConstructKind::UnderscoreBinding)
        .collect();
    assert_eq!(bindings.len(), 8);
    Ok(())
}

#[test]
fn literal_markers_are_found_in_comments_and_strings() -> TestResult {
    let first = marker(&["TO", "DO"]);
    let second = marker(&["FIX", "ME"]);
    let third = marker(&["HA", "CK"]);
    let source = format!("// {first}\nconst NOTE: &str = \"{second} {third}\";\n");
    let occurrences = scan(&source)?;
    let kinds: Vec<_> = occurrences.iter().map(DebtOccurrence::construct).collect();
    assert!(kinds.contains(&DebtConstructKind::TodoMarker));
    assert!(kinds.contains(&DebtConstructKind::FixmeMarker));
    assert!(kinds.contains(&DebtConstructKind::HackMarker));
    assert_eq!(kinds.len(), 3);
    Ok(())
}

#[test]
fn nested_statement_scope_is_analyzed() -> TestResult {
    let allow = attribute("allow", "unused_variables");
    let call = method_call("unwrap");
    let source = format!(
        "fn run() {{\n if condition {{\n  {allow}\n  let value = input;\n  {call};\n }}\n}}\n"
    );
    let occurrences = scan(&source)?;
    assert!(
        occurrences
            .iter()
            .any(|occurrence| occurrence.construct() == DebtConstructKind::AllowAttribute)
    );
    assert!(
        occurrences
            .iter()
            .any(|occurrence| occurrence.construct() == DebtConstructKind::UnwrapCall)
    );
    Ok(())
}

#[test]
fn malformed_source_and_relevant_metadata_fail_closed() {
    assert!(
        matches!(scan("fn broken("), Err(error) if error.downcast_ref::<DebtScanError>().is_some())
    );
    let malformed = format!(
        "{}\nfn run() {{}}\n",
        attribute("cfg_attr", "feature = \"x\"")
    );
    assert!(
        matches!(scan(&malformed), Err(error) if error.downcast_ref::<DebtScanError>().is_some())
    );
}

#[test]
fn raw_identifiers_cannot_hide_prohibited_debt() -> TestResult {
    let raw_panic = ["r#", "pan", "ic", "!();"].concat();
    let source = format!(
        r"
#[r#allow(dead_code)]
#[r#cfg(any())]
#[r#cfg_attr(any(), r#expect(unused_variables))]
fn run() {{
    value.r#unwrap();
    {raw_panic}
}}
"
    );
    let occurrences = scan(&source)?;
    let mut counts = BTreeMap::new();
    for occurrence in occurrences {
        *counts.entry(occurrence.construct()).or_insert(0_u32) += 1;
    }
    assert_eq!(counts.get(&DebtConstructKind::AllowAttribute), Some(&1));
    assert_eq!(counts.get(&DebtConstructKind::ExpectAttribute), Some(&1));
    assert_eq!(counts.get(&DebtConstructKind::ImpossibleCfg), Some(&2));
    assert_eq!(counts.get(&DebtConstructKind::UnwrapCall), Some(&1));
    assert_eq!(counts.get(&DebtConstructKind::PanicMacro), Some(&1));
    Ok(())
}

#[test]
fn equivalent_cfg_spellings_form_real_contradictions() -> TestResult {
    for predicate in [
        r#"all(feature = "x", not(feature = r"x"))"#,
        r####"all(feature = "x", not(feature = r###"x"###))"####,
        r#"all(feature = "x", not(feature = "\x78"))"#,
        r#"all(feature = "x", not(feature = "\u{78}"))"#,
        "all(r#unix, not(unix))",
        "r#all(unix, r#not(unix))",
    ] {
        let source = format!("#[cfg({predicate})]\nfn run() {{}}\n");
        let occurrences = scan(&source)?;
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].construct(), DebtConstructKind::ImpossibleCfg);
    }
    Ok(())
}

#[test]
fn distinct_cfg_values_are_not_conflated() -> TestResult {
    let source = r#"
#[cfg(all(feature = "x", not(feature = "y")))]
fn run() {}
"#;
    assert!(scan(source)?.is_empty());
    Ok(())
}

#[test]
fn cfg_satisfiability_has_no_fixed_atom_limit() -> TestResult {
    let mut atoms: Vec<String> = (0..24)
        .map(|index| format!(r#"feature = "feature_{index}""#))
        .collect();
    atoms.push(r#"not(feature = "feature_23")"#.to_owned());
    let predicate = atoms.join(", ");
    let source = format!("#[cfg(all({predicate}))]\nfn run() {{}}\n");
    let occurrences = scan(&source)?;
    assert_eq!(occurrences.len(), 1);
    assert_eq!(occurrences[0].construct(), DebtConstructKind::ImpossibleCfg);
    Ok(())
}
