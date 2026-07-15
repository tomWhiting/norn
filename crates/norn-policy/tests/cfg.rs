//! Three-valued production configuration predicate tests.

use norn_policy::{CfgError, CfgTruth, evaluate_cfg};

#[test]
fn production_test_predicates_are_exact() -> Result<(), CfgError> {
    assert_eq!(evaluate_cfg("test")?, CfgTruth::False);
    assert_eq!(evaluate_cfg("not(test)")?, CfgTruth::True);
    assert_eq!(evaluate_cfg("all()")?, CfgTruth::True);
    assert_eq!(evaluate_cfg("any()")?, CfgTruth::False);
    Ok(())
}

#[test]
fn unknown_build_predicates_remain_possible() -> Result<(), CfgError> {
    assert_eq!(evaluate_cfg("unix")?, CfgTruth::Possible);
    assert_eq!(evaluate_cfg("feature = \"policy\"")?, CfgTruth::Possible);
    assert_eq!(
        evaluate_cfg("all(not(test), any(unix, feature = \"policy\"))")?,
        CfgTruth::Possible
    );
    Ok(())
}

#[test]
fn boolean_operators_propagate_three_values() -> Result<(), CfgError> {
    assert_eq!(evaluate_cfg("all(test, unix)")?, CfgTruth::False);
    assert_eq!(evaluate_cfg("any(not(test), unix)")?, CfgTruth::True);
    assert_eq!(evaluate_cfg("not(unix)")?, CfgTruth::Possible);
    assert_eq!(evaluate_cfg("any(test, unix)")?, CfgTruth::Possible);
    Ok(())
}

#[test]
fn trailing_commas_are_supported() -> Result<(), CfgError> {
    assert_eq!(evaluate_cfg("all(not(test), unix,)")?, CfgTruth::Possible);
    Ok(())
}

#[test]
fn malformed_or_unsupported_input_fails_closed() {
    assert!(evaluate_cfg("not(test, unix)").is_err());
    assert!(evaluate_cfg("all(test").is_err());
    assert!(evaluate_cfg("feature = policy").is_err());
    assert!(evaluate_cfg("test extra").is_err());
    assert!(evaluate_cfg("unknown(test)").is_err());
    assert!(evaluate_cfg("target_os = r#\"macos\"#").is_err());
}

#[test]
fn cfg_evaluation_uses_heap_at_twenty_thousand_levels() -> Result<(), CfgError> {
    const DEPTH: usize = 20_000;

    let mut predicate = "not(".repeat(DEPTH);
    predicate.push_str("test");
    predicate.extend(std::iter::repeat_n(')', DEPTH));
    assert_eq!(evaluate_cfg(&predicate)?, CfgTruth::False);
    Ok(())
}
