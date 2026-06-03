//! Shared findings accumulator used across the convention-driven post-check
//! sub-modules. Each sub-module pushes to `errors` (blocking failures) or
//! `advisories` (informational notes); the entry point then composes the
//! final [`crate::tool::lifecycle::PostCheckResult`].

use crate::tool::lifecycle::Advisory;

/// Mutable accumulator threaded through the post-check sub-modules.
pub(super) struct Findings<'a> {
    pub(super) errors: &'a mut Vec<String>,
    pub(super) advisories: &'a mut Vec<Advisory>,
}
