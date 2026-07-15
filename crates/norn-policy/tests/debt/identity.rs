use norn_policy::RepositoryPath;
use norn_policy::debt::{DebtConstructKind, DebtTargetContext, DebtTargetKind, scan_rust_debt};

use crate::support::{TestResult, method_call, scan, scan_at};

#[test]
fn blank_lines_and_token_whitespace_do_not_change_fingerprints() -> TestResult {
    let call = method_call("unwrap");
    let compact = format!("fn run() {{ {call}; }}\n");
    let shifted = format!("\n\nfn run()\n{{\n    {call} ;\n}}\n");
    let first = scan(&compact)?;
    let second = scan(&shifted)?;
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].fingerprint(), second[0].fingerprint());
    assert_ne!(first[0].span(), second[0].span());
    Ok(())
}

#[test]
fn item_scope_path_and_target_moves_change_identity() -> TestResult {
    let call = method_call("unwrap");
    let first_item = scan(&format!("fn first() {{ {call}; }}\n"))?;
    let second_item = scan(&format!("fn second() {{ {call}; }}\n"))?;
    let nested = scan(&format!("fn first() {{ if ready {{ {call}; }} }}\n"))?;
    let moved_path = scan_at("src/moved.rs", &format!("fn first() {{ {call}; }}\n"))?;
    assert_ne!(first_item[0].fingerprint(), second_item[0].fingerprint());
    assert_ne!(first_item[0].fingerprint(), nested[0].fingerprint());
    assert_ne!(first_item[0].fingerprint(), moved_path[0].fingerprint());

    let path: RepositoryPath = "src/lib.rs".parse()?;
    let alternate = DebtTargetContext::new(DebtTargetKind::Binary, "fixture", "runner")?;
    let alternate_occurrences = scan_rust_debt(
        &path,
        &alternate,
        format!("fn first() {{ {call}; }}\n").as_bytes(),
    )?;
    assert_ne!(
        first_item[0].fingerprint(),
        alternate_occurrences[0].fingerprint()
    );
    Ok(())
}

#[test]
fn identical_occurrences_receive_distinct_multiset_ordinals() -> TestResult {
    let call = method_call("unwrap");
    let occurrences = scan(&format!("fn run() {{ {call}; {call}; }}\n"))?;
    let calls: Vec<_> = occurrences
        .iter()
        .filter(|occurrence| occurrence.construct() == DebtConstructKind::UnwrapCall)
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].ordinal(), 0);
    assert_eq!(calls[1].ordinal(), 1);
    assert_eq!(calls[0].item_identity(), calls[1].item_identity());
    assert_eq!(calls[0].syntax_digest(), calls[1].syntax_digest());
    assert_eq!(calls[0].scope_digest(), calls[1].scope_digest());
    assert_ne!(calls[0].fingerprint(), calls[1].fingerprint());
    Ok(())
}

#[test]
fn equivalent_cfg_literals_have_one_stable_identity() -> TestResult {
    let predicates = [
        r#"all(feature = "x", not(feature = "x"))"#,
        r#"all(feature = "x", not(feature = r"x"))"#,
        r####"all(feature = "x", not(feature = r###"x"###))"####,
        r#"all(r#feature = "x", not(feature = "x"))"#,
        r#"all(feature = "x", not(feature = "\x78"))"#,
        r#"all(feature = "x", not(feature = "\u{78}"))"#,
    ];
    let mut occurrences = Vec::new();
    for predicate in predicates {
        let source = format!("#[cfg({predicate})]\nfn run() {{}}\n");
        let scanned = scan(&source)?;
        assert_eq!(scanned.len(), 1);
        occurrences.push(scanned[0].clone());
    }
    for occurrence in &occurrences[1..] {
        assert_eq!(occurrence.syntax_digest(), occurrences[0].syntax_digest());
        assert_eq!(occurrence.fingerprint(), occurrences[0].fingerprint());
    }
    Ok(())
}
