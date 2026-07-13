//! Construction helpers for diagnostics post-validation infrastructure.
//!
//! This module owns the shared setup that used to live in CLI runtime wiring:
//! compiled adapter/policy registration and `CONVENTIONS.toml` loading.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::tools::diagnostics_check::DiagnosticInfra;
use crate::tools::lsp::LspBackend;
use diagnostics::conventions::{ConventionsConfig, ConventionsError, ToolRef};
use diagnostics::lsp_bridge::LspBridge;
use lsp::workspace::LspWorkspace;

use crate::util::read_workspace_text_file;

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
    let workspace_root = match crate::resource::acquire_filesystem_operation() {
        Ok(_descriptor_permit) => workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf()),
        Err(error) => {
            tracing::warn!(
                %error,
                "diagnostics root resolution skipped because descriptor admission failed"
            );
            workspace_root.to_path_buf()
        }
    };
    build_diagnostic_infra_at_launch_root(&workspace_root, lsp_backend, lsp_workspace)
}

/// Builds diagnostics from an already-canonical immutable launch root.
pub(crate) fn build_diagnostic_infra_at_launch_root(
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
    let conventions = match load_non_executing_conventions(workspace_root) {
        Ok(config) => config,
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

fn load_non_executing_conventions(
    workspace_root: &Path,
) -> Result<Option<ConventionsConfig>, ConventionsError> {
    let path = workspace_root.join("CONVENTIONS.toml");
    let source = {
        let _descriptor_permit =
            crate::resource::acquire_filesystem_operation().map_err(|error| {
                ConventionsError::Io {
                    path: path.clone(),
                    source: std::io::Error::other(error),
                }
            })?;
        match read_workspace_text_file(workspace_root, Path::new("CONVENTIONS.toml")) {
            Ok(file) => file.content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConventionsError::Io { path, source });
            }
        }
    };
    let sanitized = strip_process_authority(&source)?;
    let config = ConventionsConfig::load_from_str(&sanitized)?;
    if !is_non_executing(&config) {
        return Err(ConventionsError::ParseError(
            "workspace conventions retained process authority after sanitization".to_owned(),
        ));
    }
    Ok(Some(config))
}

fn strip_process_authority(source: &str) -> Result<String, ConventionsError> {
    let mut document: toml::Table =
        toml::from_str(source).map_err(|error| ConventionsError::ParseError(error.to_string()))?;
    for (_, value) in &mut document {
        let toml::Value::Table(table) = value else {
            continue;
        };
        for key in ["lsp", "diagnostics", "remediation", "reports"] {
            table.remove(key);
        }
    }
    toml::to_string(&document).map_err(|error| ConventionsError::ParseError(error.to_string()))
}

fn is_non_executing(config: &ConventionsConfig) -> bool {
    if config.lang_defs().values().any(|definition| {
        definition.lsp.is_some()
            || definition.diagnostics.is_some()
            || definition.remediation.is_some()
            || definition.reports.is_some()
    }) {
        return false;
    }
    config.rules().values().all(|compiled| {
        compiled.rule.lsp.is_none()
            && compiled.rule.activations.iter().all(|(name, _)| {
                compiled.language.as_deref().is_none_or(|language| {
                    matches!(
                        config.lookup_tool(language, name),
                        None | Some(ToolRef::Pattern(_))
                    )
                })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED_CONVENTIONS: &str = r#"
[rust.patterns]
no-todo = { matcher = "regex", pattern = "TODO", handling = "block", feedback = "remove it" }
[rust.diagnostics]
clippy = { target = "package", handling = "block", args = ["--all-targets"], env = ["MARKER=bad"] }
[rust.remediation]
shell = { target = "workspace", args = ["-c", "touch marker"] }
[rust.reports]
report = { target = "workspace", args = ["-c", "touch report"] }
[rust.lsp]
server = "rust-analyzer"
check = "cargo check"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
loc = { limit = 500, handling = "block" }
lsp = { tests = { on = "tool", scope = "package" } }
no-todo = { on = "tool", handling = "block" }
clippy = { on = "tool", handling = "block" }
shell = { on = "tool" }
report = { on = "tool" }
"#;

    #[test]
    fn retains_only_loc_and_pattern_checks() -> Result<(), Box<dyn std::error::Error>> {
        let sanitized = strip_process_authority(MIXED_CONVENTIONS)?;
        let config = ConventionsConfig::load_from_str(&sanitized)?;

        assert!(is_non_executing(&config));
        let rule = config.rule("rust-general").ok_or("rule missing")?;
        assert_eq!(rule.rule.loc.as_ref().map(|loc| loc.limit), Some(500));
        assert!(matches!(
            config.lookup_tool("rust", "no-todo"),
            Some(ToolRef::Pattern(_))
        ));
        assert!(config.lookup_tool("rust", "clippy").is_none());
        assert!(config.lookup_tool("rust", "shell").is_none());
        assert!(config.lookup_tool("rust", "report").is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_conventions_file() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir()?;
        let outside = tempfile::NamedTempFile::new()?;
        std::fs::write(outside.path(), MIXED_CONVENTIONS)?;
        symlink(outside.path(), workspace.path().join("CONVENTIONS.toml"))?;

        assert!(load_non_executing_conventions(workspace.path()).is_err());
        Ok(())
    }
}
