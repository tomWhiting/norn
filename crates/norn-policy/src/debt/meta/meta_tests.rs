//! Deep metadata regressions that bypass the repository syntax walker.

use super::analyze_attribute;
use crate::debt::model::{DebtConstructKind, DebtScanError};

const DEEP_META_NESTING: usize = 20_000;

#[test]
fn deeply_nested_formula_uses_heap_backed_traversal() -> Result<(), DebtScanError> {
    let predicate = format!(
        "{}any(){}",
        "not(".repeat(DEEP_META_NESTING),
        ")".repeat(DEEP_META_NESTING)
    );
    let attribute = format!("#[cfg({predicate})]");

    let findings = analyze_attribute(&attribute)?;
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].construct, DebtConstructKind::ImpossibleCfg);
    Ok(())
}
