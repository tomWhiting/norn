//! Shared diagnostic infrastructure passed via [`ToolContext`] extensions.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use diagnostics::adapter::registry::AdapterRegistry;
use diagnostics::conventions::ConventionsConfig;
use diagnostics::lsp_bridge::LspBridge;
use diagnostics::registry::PolicyRegistry;

use crate::tools::lsp::LspBackend;

/// Shared diagnostic infrastructure published on [`crate::tool::context::ToolContext`]
/// as an extension. Constructed once at session startup.
pub struct DiagnosticInfra {
    /// Adapter registry (clippy, nextest, biome, declarative adapters, etc.).
    pub adapters: Arc<AdapterRegistry>,
    /// Policy registry (~20 hand-written + generated policies).
    pub policies: Arc<PolicyRegistry>,
    /// Workspace root for cargo invocations and template expansion.
    pub workspace_root: PathBuf,
    /// Diagnostic server UNIX socket path. Default is
    /// `.git/yggdrasil/diag.sock` relative to [`Self::workspace_root`],
    /// populated by [`crate::tools::diagnostics_infra::build_diagnostic_infra`].
    /// Consumed by the LD-003 server-query fast path in the post-check
    /// pipeline before it falls back to inline adapter dispatch.
    pub socket_path: PathBuf,
    /// Parsed `CONVENTIONS.toml` for the workspace. `None` when no file
    /// is present or the file failed to load.
    pub conventions: Option<ConventionsConfig>,
    /// Optional LSP backend used by the post-check pipeline to discover
    /// and execute convention-driven tests (R3). `None` means LSP-driven
    /// tests are silently skipped (CO5 — graceful degradation).
    pub lsp_backend: Option<Arc<dyn LspBackend>>,
    /// Optional LSP diagnostic bridge used by the post-check pipeline to
    /// pull language-server diagnostics for a modified file before falling
    /// back to the LD-003 server-query or inline-adapter cascade
    /// (LD-012 R3). `None` means the LSP fast path is silently skipped
    /// (CO5 — graceful degradation when no language server is running).
    pub lsp_bridge: Option<Arc<LspBridge>>,
    /// Workspace-relative paths modified by tool lifecycle mutations in this
    /// session. Populated by [`super::post_check::DiagnosticsPostCheck`] and
    /// read by task-complete / stop lifecycle checks.
    pub modified_files: Arc<Mutex<HashSet<PathBuf>>>,
}

impl DiagnosticInfra {
    /// Return a point-in-time snapshot of modified workspace-relative paths.
    #[must_use]
    pub fn modified_files(&self) -> HashSet<PathBuf> {
        match self.modified_files.lock() {
            Ok(files) => files.clone(),
            Err(poisoned) => {
                tracing::warn!("modified-files accumulator mutex was poisoned; returning snapshot");
                poisoned.into_inner().clone()
            }
        }
    }
}
