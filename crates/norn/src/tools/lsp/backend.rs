//! Norn-local LSP backend trait and result types.
//!
//! Norn does not depend on a specific LSP implementation. The orchestrator
//! supplies an [`LspBackend`] (typically delegating to the workspace `lsp`
//! crate or an extension). Result types are plain serde-friendly structs so
//! the tool's JSON output is stable independent of `lsp-types`.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A source location (path + one-based positions).
///
/// Producers convert from the LSP wire protocol's zero-based positions by
/// adding one, so these fields line up with editor gutters and compiler
/// diagnostics as-is.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspLocation {
    /// Filesystem path of the location.
    pub path: String,
    /// One-based start line.
    pub line: u32,
    /// One-based start column (UTF-16 code units, per LSP).
    pub column: u32,
    /// One-based end line.
    pub end_line: u32,
    /// One-based end column.
    pub end_column: u32,
}

/// Hover content with an optional range the hover applies to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LspHover {
    /// Rendered hover content (typically markdown).
    pub content: String,
    /// Source range the hover describes, if reported by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<LspLocation>,
}

/// Symbol-kind classification, mirroring LSP's standard set.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LspSymbolKind {
    /// A file symbol.
    File,
    /// A module symbol.
    Module,
    /// A namespace symbol.
    Namespace,
    /// A package symbol.
    Package,
    /// A class symbol.
    Class,
    /// A method symbol.
    Method,
    /// A property symbol.
    Property,
    /// A field symbol.
    Field,
    /// A constructor symbol.
    Constructor,
    /// An enum symbol.
    Enum,
    /// An interface symbol.
    Interface,
    /// A function symbol.
    Function,
    /// A variable symbol.
    Variable,
    /// A constant symbol.
    Constant,
    /// A string-literal symbol.
    String,
    /// A numeric-literal symbol.
    Number,
    /// A boolean-literal symbol.
    Boolean,
    /// An array symbol.
    Array,
    /// An object symbol.
    Object,
    /// A key symbol.
    Key,
    /// A null-literal symbol.
    Null,
    /// An enum-member symbol.
    EnumMember,
    /// A struct symbol.
    Struct,
    /// An event symbol.
    Event,
    /// An operator symbol.
    Operator,
    /// A type-parameter symbol.
    TypeParameter,
    /// Catch-all when the server reports a kind we do not map.
    Other,
}

/// A document symbol entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LspSymbol {
    /// Symbol name.
    pub name: String,
    /// Symbol kind.
    pub kind: LspSymbolKind,
    /// Location of the full symbol declaration.
    pub location: LspLocation,
}

/// Classification of a runnable test discovered via LSP.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestRunnableKind {
    /// A single test function (e.g. `#[test] fn foo`).
    Test,
    /// A module-level runnable (e.g. `mod tests`).
    TestModule,
    /// A documentation test embedded in a doc comment.
    DocTest,
}

/// A test runnable reported by an LSP backend.
///
/// Aggregates information from `experimental/runnables` (rust-analyzer),
/// `textDocument/codeLens` (generic), or other server-specific sources
/// into a uniform serde-friendly shape. Plain primitives are used
/// throughout — no `lsp_types::*` leakage (CO5/CO13).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestRunnable {
    /// Human-readable label (e.g. `test foo::bar`).
    pub label: String,
    /// Kind of runnable.
    pub kind: TestRunnableKind,
    /// Source location the runnable targets, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<LspLocation>,
    /// Cargo / build-tool arguments (e.g. `["test", "--package", "foo"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cargo_args: Vec<String>,
    /// Arguments passed to the compiled test executable after `--`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executable_args: Vec<String>,
    /// Working directory the runnable expects, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Workspace root the runnable belongs to, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
}

/// Diagnostic severity, mirroring LSP's standard set.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LspDiagnosticSeverity {
    /// Error-level diagnostic.
    Error,
    /// Warning-level diagnostic.
    Warning,
    /// Informational diagnostic.
    Information,
    /// Hint-level diagnostic.
    Hint,
}

/// A diagnostic entry attached to a file range.
///
/// Positions follow the same one-based convention as [`LspLocation`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LspDiagnostic {
    /// Severity classification.
    pub severity: LspDiagnosticSeverity,
    /// Human-readable diagnostic message.
    pub message: String,
    /// One-based start line.
    pub line: u32,
    /// One-based start column.
    pub column: u32,
    /// One-based end line.
    pub end_line: u32,
    /// One-based end column.
    pub end_column: u32,
    /// Optional source label (e.g. "rust-analyzer", "tsserver").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Optional diagnostic code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Errors reported by an [`LspBackend`].
#[derive(Debug, thiserror::Error)]
pub enum LspBackendError {
    /// No backend is wired into the [`super::tool::LspTool`].
    #[error("no LSP backend connected")]
    NotConnected,

    /// No language server is configured for the requested file type.
    #[error("no LSP server for file: {path}")]
    NoServerForFile {
        /// Path the request was made against.
        path: String,
    },

    /// The backend communicated with a server but the protocol exchange failed.
    #[error("LSP protocol error: {reason}")]
    ProtocolError {
        /// Description of the protocol failure.
        reason: String,
    },

    /// The backend timed out waiting for the server.
    #[error("LSP request timed out")]
    Timeout,

    /// Norn's active-descriptor budget refused a language-server spawn.
    #[error(transparent)]
    DescriptorAdmission(Box<crate::resource::DescriptorAdmissionError>),
}

/// Async trait for an LSP client backend.
///
/// Implementations supply hover, definition, references, document symbols,
/// and diagnostics for a single file. Position *arguments* use the LSP
/// zero-based line/column wire convention; positions in *returned* values
/// ([`LspLocation`], [`LspDiagnostic`]) are one-based.
#[async_trait]
pub trait LspBackend: Send + Sync {
    /// Returns hover information at the given position, or `None` if no
    /// hover is available there.
    async fn hover(
        &self,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Option<LspHover>, LspBackendError>;

    /// Returns the locations the symbol at `(line, column)` is defined at.
    async fn definition(
        &self,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<LspLocation>, LspBackendError>;

    /// Returns the locations where the symbol at `(line, column)` is referenced.
    async fn references(
        &self,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<LspLocation>, LspBackendError>;

    /// Returns the document symbols declared in `path`.
    async fn symbols(&self, path: &Path) -> Result<Vec<LspSymbol>, LspBackendError>;

    /// Returns the diagnostics currently reported for `path`.
    async fn diagnostics(&self, path: &Path) -> Result<Vec<LspDiagnostic>, LspBackendError>;

    /// Returns the test runnables defined in `path`.
    ///
    /// Default implementation returns an empty `Vec` for backends whose
    /// language server exposes no test-discovery source (C75). The
    /// production `WorkspaceLspBackend` overrides this with
    /// rust-analyzer's `experimental/runnables`.
    async fn test_runnables(&self, _path: &Path) -> Result<Vec<TestRunnable>, LspBackendError> {
        Ok(Vec::new())
    }

    /// Returns tests related to the symbol at `(line, column)` in `path`
    /// (e.g. tests that exercise the function under the cursor).
    ///
    /// Default implementation returns an empty `Vec` so backends without
    /// related-tests support degrade silently (C75, C77).
    async fn related_tests(
        &self,
        _path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<TestRunnable>, LspBackendError> {
        Ok(Vec::new())
    }

    /// Asks the language server to re-run its flycheck (background build
    /// check) for `path`. Used to force an immediate diagnostic refresh.
    ///
    /// Default implementation is a no-op so backends without flycheck
    /// control degrade silently (C75).
    async fn run_flycheck(&self, _path: &Path) -> Result<(), LspBackendError> {
        Ok(())
    }

    /// Asks the language server to clear any pending flycheck state.
    ///
    /// Default implementation is a no-op so backends without flycheck
    /// control degrade silently (C75).
    async fn clear_flycheck(&self) -> Result<(), LspBackendError> {
        Ok(())
    }
}
