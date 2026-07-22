//! Agent variant profiles — named child-agent configurations (prompt, tool
//! subset, model, reasoning effort) honoured by the spawn path.
//!
//! Brief `agent-variants` (2026-07-04). Built-ins (`explorer`, `reviewer`,
//! `implementer`) are data in [`builtin`]; configured `variants` settings
//! overlay them per-field in [`catalog`].

mod builtin;
mod catalog;

pub use catalog::{ResolvedVariant, VariantCatalog, VariantCatalogError, VariantPromptOrigin};
