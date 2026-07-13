//! LSP tool (hover, definition, references, document symbols, diagnostics).
//!
//! The tool itself is a thin wrapper around an [`LspBackend`] trait that the
//! orchestrator wires in via dependency injection. This keeps the norn
//! crate free of any concrete LSP-client dependency (see CO10) while
//! providing a stable, structured tool interface for agents.

pub mod backend;
pub mod tool;
pub mod workspace_backend;

pub use self::backend::{
    LspBackend, LspBackendError, LspDiagnostic, LspDiagnosticSeverity, LspHover, LspLocation,
    LspSymbol, LspSymbolKind, TestRunnable, TestRunnableKind,
};
pub use self::tool::LspTool;
pub use self::workspace_backend::{WorkspaceLspBackend, build_lsp_backend, build_lsp_workspace};

/// Re-export of [`lsp::workspace::LspWorkspace`] so consumers of the
/// `norn` crate (`norn-cli`, `meridian-services`) can name the workspace
/// type without taking a direct dependency on the `lsp` crate.
///
/// LD-015 shares a single `LspWorkspace` across all workflow steps and
/// the TUI driver; this re-export is the single import surface those
/// callers reach through.
pub use ::lsp::workspace::LspWorkspace;
