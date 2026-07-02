//! Adapter connecting the `lsp` workspace crate to norn's [`LspBackend`](crate::tools::lsp::LspBackend) trait.
//!
//! [`WorkspaceLspBackend`] wraps an `LspWorkspace` and implements the
//! backend methods by delegating to the workspace API and mapping
//! `lsp-types` results into norn's serialisation-friendly types.
//!
//! Logic is split into named submodules: [`adapter`] holds the struct and
//! trait implementation, [`mapping`] holds type-mapping helpers and the
//! retry primitive, and [`runnables`] holds the test-runnable parsing and
//! call-hierarchy fallback used by `related_tests`.

pub mod adapter;
pub mod mapping;
pub mod runnables;

#[cfg(test)]
mod stub_tests;

pub use self::adapter::{WorkspaceLspBackend, build_lsp_backend};
