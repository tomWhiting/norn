//! Construction helpers for diagnostics post-validation infrastructure.
//!
//! This module owns the shared setup that used to live in CLI runtime wiring:
//! compiled adapter/policy registration and `CONVENTIONS.toml` loading.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::tools::diagnostics_check::DiagnosticInfra;
use crate::tools::lsp::LspBackend;
use diagnostics::conventions::{ConventionsConfig, ConventionsError};
use diagnostics::lsp_bridge::LspBridge;
use lsp::workspace::LspWorkspace;

/// Build diagnostic infrastructure for a workspace root.
///
/// `CONVENTIONS.toml` is loaded from `workspace_root`. A missing file is
/// treated as an unconfigured workspace; parse/validation errors are logged and
/// also leave conventions disabled so agent startup remains best-effort.
///
/// `lsp_backend` plumbs an optional [`LspBackend`] into the resulting
/// [`DiagnosticInfra`] so that convention-driven LSP test execution
/// (LD-014 R3) can dispatch through it. `None` skips LSP-driven tests
/// silently (CO5 — graceful degradation). Workflow / TUI / standalone
/// wiring will fill the slot in LD-012 and LD-015.
///
/// `lsp_workspace`, when supplied, donates a shared
/// [`DiagnosticAggregator`](lsp::features::diagnostics::DiagnosticAggregator)
/// handle that is wrapped in an [`LspBridge`] and stored on the resulting
/// [`DiagnosticInfra`]. The bridge gives the post-check pipeline a fast LSP
/// path before the LD-003 server-query / inline-adapter cascade (LD-012 R3).
/// `None` means the LSP fast path is silently skipped (CO5). LD-015 will
/// fill this slot with the runtime-shared `LspWorkspace`.
#[must_use]
pub fn build_diagnostic_infra(
    workspace_root: &Path,
    lsp_backend: Option<Arc<dyn LspBackend>>,
    lsp_workspace: Option<&LspWorkspace>,
) -> DiagnosticInfra {
    let mut adapter_reg = diagnostics::adapter::registry::AdapterRegistry::new();
    diagnostics::languages::rust::register_rust_adapters(&mut adapter_reg);
    diagnostics::languages::typescript::register_typescript_adapters(&mut adapter_reg);
    diagnostics::languages::gleam::register_gleam_adapters(&mut adapter_reg);

    let mut policy_reg = diagnostics::registry::PolicyRegistry::new();
    diagnostics::languages::rust::register_rust_policies(&mut policy_reg);
    diagnostics::languages::typescript::register_typescript_policies(&mut policy_reg);
    diagnostics::languages::gleam::register_gleam_policies(
        &mut policy_reg,
        &diagnostics::languages::gleam::GleamPolicyConfig::default(),
    );

    let conventions_path = workspace_root.join("CONVENTIONS.toml");
    let conventions = match ConventionsConfig::load(&conventions_path) {
        Ok(config) => Some(config),
        Err(ConventionsError::FileNotFound(_)) => None,
        Err(err) => {
            tracing::warn!(
                path = %conventions_path.display(),
                error = %err,
                "failed to load CONVENTIONS.toml; continuing without it"
            );
            None
        }
    };

    let lsp_bridge =
        lsp_workspace.map(|workspace| Arc::new(LspBridge::new(workspace.diagnostics_arc())));

    DiagnosticInfra {
        adapters: Arc::new(adapter_reg),
        policies: Arc::new(policy_reg),
        workspace_root: workspace_root.to_path_buf(),
        socket_path: diagnostics::server::default_socket_path(workspace_root),
        conventions,
        lsp_backend,
        lsp_bridge,
        modified_files: Arc::new(Mutex::new(HashSet::new())),
    }
}
