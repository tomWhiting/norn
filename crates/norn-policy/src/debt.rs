//! Syntax-aware prohibited-debt analysis.

mod fingerprint;
mod meta;
mod meta_lex;
mod model;
mod scan;

pub use model::{
    DebtConstructKind, DebtOccurrence, DebtScanError, DebtTargetContext, DebtTargetContextError,
    DebtTargetField, DebtTargetKind,
};
pub use scan::scan_rust_debt;
