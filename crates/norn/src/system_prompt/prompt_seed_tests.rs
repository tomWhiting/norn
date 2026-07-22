use serde_json::json;

use super::{PromptPlan, PromptSeedFingerprint, PromptSource};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fingerprint(fragments: &[(PromptSource, &str)]) -> PromptSeedFingerprint {
    let mut plan = PromptPlan::new();
    for (source, content) in fragments {
        plan.set(*source, *content);
    }
    PromptSeedFingerprint::from_plan(&plan)
}

#[test]
fn binds_ordered_source_authority_and_exact_utf8() {
    let base = fingerprint(&[
        (PromptSource::OperatorProfile, "operator"),
        (PromptSource::WorkspaceProfile, "repository"),
    ]);
    let changed_source = fingerprint(&[
        (PromptSource::OperatorOverride, "operator"),
        (PromptSource::WorkspaceProfile, "repository"),
    ]);
    let changed_authority = fingerprint(&[
        (PromptSource::OperatorProfile, "operator"),
        (PromptSource::OperatorRule, "repository"),
    ]);
    let changed_bytes = fingerprint(&[
        (PromptSource::OperatorProfile, "operator"),
        (PromptSource::WorkspaceProfile, "repository\n"),
    ]);

    assert_ne!(base, changed_source);
    assert_ne!(base, changed_authority);
    assert_ne!(base, changed_bytes);
}

#[test]
fn system_only_changes_do_not_change_the_seed() {
    let without_system = fingerprint(&[(PromptSource::OperatorProfile, "operator")]);
    let with_system = fingerprint(&[
        (PromptSource::ProductPolicy, "product-v1"),
        (PromptSource::OperatorProfile, "operator"),
    ]);
    let changed_system = fingerprint(&[
        (PromptSource::ProductPolicy, "product-v2"),
        (PromptSource::OperatorProfile, "operator"),
    ]);

    assert_eq!(without_system, with_system);
    assert_eq!(with_system, changed_system);
}

#[test]
fn operator_runtime_context_is_exact_and_developer_bound() {
    let base = fingerprint(&[(PromptSource::OperatorProfile, "operator")]);
    let first = base.with_operator_runtime_context("# cwd\n/one");
    let identical = base.with_operator_runtime_context("# cwd\n/one");
    let changed = base.with_operator_runtime_context("# cwd\n/two");

    assert_eq!(first, identical);
    assert_ne!(first, changed);
    assert_ne!(first, base);
}

#[test]
fn serde_is_strict_and_debug_is_redacted() -> TestResult {
    let fingerprint = fingerprint(&[(PromptSource::UserContextFile, "private operator policy")]);
    let encoded = serde_json::to_value(fingerprint)?;
    let Some(hex) = encoded.as_str() else {
        return Err(std::io::Error::other("fingerprint did not serialize as text").into());
    };
    assert_eq!(hex.len(), 64);
    assert!(
        hex.bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
    assert_eq!(
        serde_json::from_value::<PromptSeedFingerprint>(encoded)?,
        fingerprint
    );
    assert!(serde_json::from_value::<PromptSeedFingerprint>(json!("A".repeat(64))).is_err());
    assert!(serde_json::from_value::<PromptSeedFingerprint>(json!("0".repeat(62))).is_err());
    assert_eq!(
        format!("{fingerprint:?}"),
        "PromptSeedFingerprint([REDACTED])"
    );
    Ok(())
}

#[test]
fn framing_has_a_pinned_cross_platform_digest() -> TestResult {
    let fingerprint = fingerprint(&[(PromptSource::OperatorProfile, "operator")]);
    assert_eq!(
        serde_json::to_value(fingerprint)?,
        json!("4aa70ed41d5e78abd5e63d44cc0cae57ce58dfa03411f1baf2d5b4dcd456013d"),
    );
    Ok(())
}
