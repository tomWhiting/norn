//! Compatibility facade for convention configuration types.
//!
//! Convention parsing and matching live in the diagnostics crate; norn keeps
//! this module so existing `norn::tools::conventions::*` imports continue to
//! compile while convention dispatch stays in norn's post-check pipeline.

pub use diagnostics::conventions::*;
