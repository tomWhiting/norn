//! Closed structural errors for current-fact and legacy evaluation.

use thiserror::Error;

use super::super::governance::GovernanceLinkError;
use super::super::items::ItemComparisonError;

/// Invalid LOC ceiling.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocCeilingsError {
    /// Thin-entrypoint limit was zero.
    #[error("thin-entrypoint production LOC ceiling is zero")]
    ThinEntrypointZero,
    /// Other Rust limit was zero.
    #[error("other Rust production LOC ceiling is zero")]
    OtherRustZero,
}

/// Legacy comparison could not begin because governance was invalid.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LegacyEvaluationError {
    /// Governance does not exactly map immutable origin facts.
    #[error("legacy governance does not match immutable origin")]
    Governance(#[source] GovernanceLinkError),
    /// Item comparison inputs lost their validated ordering invariant.
    #[error("item comparison input is invalid")]
    ItemComparison(#[source] ItemComparisonError),
    /// A validated governance reference was absent from the derived origin map.
    #[error("validated governance reference is absent from immutable origin")]
    OriginReference,
}
