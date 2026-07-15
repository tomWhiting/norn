//! Exact writer-operation classification cardinality.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::{WriterInventory, WriterOperationId, WriterToken};

/// One reviewed classification for one stable operation.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriterClassification {
    /// Stable operation being classified.
    pub operation: WriterOperationId,
    /// Exact reviewed classification.
    pub classification: WriterClassificationKind,
}

/// Mutually exclusive reviewed operation classification.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
pub enum WriterClassificationKind {
    /// Operation belongs to one artifact family.
    Family {
        /// Stable artifact-family identifier.
        family: WriterToken,
    },
    /// Operation is a reviewed cleanup rather than a retained writer.
    ReviewedCleanup {
        /// Stable review record identifier.
        review: WriterToken,
    },
    /// Operation is a reviewed lexical false positive.
    ReviewedFalsePositive {
        /// Stable review record identifier.
        review: WriterToken,
    },
    /// Generic primitive shared by explicit inbound artifact families.
    SharedPrimitive {
        /// Stable shared-primitive identifier.
        primitive: WriterToken,
        /// Sorted, unique inbound artifact-family edges.
        inbound_families: Vec<WriterToken>,
    },
}

/// Closed classification-integrity issue.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum ClassificationIssue {
    /// An inventory operation has no classification.
    Missing {
        /// Operation missing a classification row.
        operation: WriterOperationId,
    },
    /// An operation has more than one classification row.
    Duplicate {
        /// Operation named by duplicate rows.
        operation: WriterOperationId,
    },
    /// A row names no current operation.
    Stale {
        /// Operation named only by stale governance.
        operation: WriterOperationId,
    },
    /// A shared primitive lacks at least two unique inbound family edges.
    SharedEdges {
        /// Shared operation with invalid inbound-family edges.
        operation: WriterOperationId,
    },
}

/// Validate exact operation-to-family/review classification cardinality.
///
/// The result is sorted and reports every missing, duplicate, stale, role, and
/// shared-edge defect rather than stopping at the first one.
#[must_use]
pub fn validate_writer_classifications(
    inventory: &WriterInventory,
    classifications: &[WriterClassification],
) -> Vec<ClassificationIssue> {
    validate_classifications_for_operations(
        inventory
            .operations()
            .iter()
            .map(super::model::WriterOperation::id),
        classifications,
    )
}

pub(crate) fn validate_classifications_for_operations(
    operations: impl IntoIterator<Item = WriterOperationId>,
    classifications: &[WriterClassification],
) -> Vec<ClassificationIssue> {
    validate_operation_rows(operations, classifications, true)
}

pub(crate) fn validate_required_classifications_for_operations(
    operations: impl IntoIterator<Item = WriterOperationId>,
    classifications: &[WriterClassification],
) -> Vec<ClassificationIssue> {
    validate_operation_rows(operations, classifications, false)
}

fn validate_operation_rows(
    operations: impl IntoIterator<Item = WriterOperationId>,
    classifications: &[WriterClassification],
    report_stale: bool,
) -> Vec<ClassificationIssue> {
    let operations = operations
        .into_iter()
        .map(|operation| (operation, ()))
        .collect::<BTreeMap<_, _>>();
    let mut rows: BTreeMap<WriterOperationId, Vec<&WriterClassificationKind>> = BTreeMap::new();
    for row in classifications {
        rows.entry(row.operation)
            .or_default()
            .push(&row.classification);
    }

    let mut issues = Vec::new();
    for operation in operations.keys() {
        match rows.get(operation).map(Vec::as_slice) {
            None | Some([]) => issues.push(ClassificationIssue::Missing {
                operation: *operation,
            }),
            Some([classification]) => {
                if !classification_has_valid_structure(classification) {
                    issues.push(ClassificationIssue::SharedEdges {
                        operation: *operation,
                    });
                }
            }
            Some(_) => issues.push(ClassificationIssue::Duplicate {
                operation: *operation,
            }),
        }
    }
    if report_stale {
        for operation in rows.keys() {
            if !operations.contains_key(operation) {
                issues.push(ClassificationIssue::Stale {
                    operation: *operation,
                });
            }
        }
    }
    issues.sort();
    issues
}

pub(crate) fn classification_has_valid_structure(
    classification: &WriterClassificationKind,
) -> bool {
    !matches!(
        classification,
        WriterClassificationKind::SharedPrimitive {
            inbound_families,
            ..
        } if inbound_families.len() < 2
            || inbound_families.windows(2).any(|pair| pair[0] >= pair[1])
    )
}
