//! `lsp` tool wrapper: dispatches structured actions to an [`LspBackend`].

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

use super::super::confinement::check_confinement;
use super::backend::{LspBackend, LspBackendError};

/// LSP tool: delegates hover/definition/references/symbols/diagnostics to
/// an injected [`LspBackend`].
pub struct LspTool {
    backend: Option<Arc<dyn LspBackend>>,
}

impl LspTool {
    /// Creates an `LspTool` with no backend.
    ///
    /// Every action invocation will return a structured "no LSP backend
    /// connected" error. This is the default until the orchestrator wires
    /// a live backend.
    #[must_use]
    pub fn new() -> Self {
        Self { backend: None }
    }

    /// Creates an `LspTool` wired to a live [`LspBackend`] implementation.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn LspBackend>) -> Self {
        Self {
            backend: Some(backend),
        }
    }
}

impl Default for LspTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct LspArgs {
    action: String,
    path: String,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    column: Option<u32>,
    /// Reserved for future workspace-symbol lookup; accepted by the schema
    /// for forward compatibility but not consumed by the current dispatch.
    #[serde(default)]
    symbol: Option<String>,
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &'static str {
        "lsp"
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/lsp.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Development
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/lsp.usage.md"))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["hover", "definition", "references", "symbols", "diagnostics"],
                    "description": "Which LSP operation to perform."
                },
                "path": {
                    "type": "string",
                    "description": "Filesystem path the action targets. Relative paths resolve against the agent working directory."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line number. Required for hover/definition/references."
                },
                "column": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based column number. Required for hover/definition/references."
                },
                "symbol": {
                    "type": "string",
                    "description": "Reserved for future workspace-symbol lookup."
                }
            },
            "required": ["action", "path"],
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: LspArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                format!("invalid lsp arguments: {e}"),
            )
        })?;

        let backend = self.backend.as_ref().ok_or(ToolError::ExecutionFailed {
            reason: "no LSP backend connected".to_owned(),
        })?;

        if args.symbol.is_some() {
            tracing::debug!(
                action = %args.action,
                "lsp: `symbol` arg ignored — workspace-symbol lookup is reserved for a future action"
            );
        }

        let path = ctx.resolve_path(&args.path);

        // Workspace confinement (opt-in): refuse before the backend touches
        // the file so even metadata of out-of-root paths is never disclosed.
        if let Err(reason) = check_confinement(ctx, &path) {
            return Ok(ToolOutput::failure_with_content(
                serde_json::json!({ "path": args.path, "kind": "confinement_refused" }),
                ToolErrorPayload::new(
                    ToolErrorKind::PermissionDenied,
                    format!("lsp refused: {reason}"),
                )
                .with_detail(serde_json::json!({ "path": args.path })),
            ));
        }

        let result = match args.action.as_str() {
            "hover" => {
                let (line, column) = require_position(&args.action, args.line, args.column)?;
                let hover = backend
                    .hover(&path, line.saturating_sub(1), column.saturating_sub(1))
                    .await
                    .map_err(|e| map_backend_err(&e))?;
                json!({ "action": "hover", "hover": hover })
            }
            "definition" => {
                let (line, column) = require_position(&args.action, args.line, args.column)?;
                let locations = backend
                    .definition(&path, line.saturating_sub(1), column.saturating_sub(1))
                    .await
                    .map_err(|e| map_backend_err(&e))?;
                json!({ "action": "definition", "locations": locations })
            }
            "references" => {
                let (line, column) = require_position(&args.action, args.line, args.column)?;
                let locations = backend
                    .references(&path, line.saturating_sub(1), column.saturating_sub(1))
                    .await
                    .map_err(|e| map_backend_err(&e))?;
                json!({ "action": "references", "locations": locations })
            }
            "symbols" => {
                let symbols = backend
                    .symbols(&path)
                    .await
                    .map_err(|e| map_backend_err(&e))?;
                json!({ "action": "symbols", "symbols": symbols })
            }
            "diagnostics" => {
                let diagnostics = backend
                    .diagnostics(&path)
                    .await
                    .map_err(|e| map_backend_err(&e))?;
                json!({ "action": "diagnostics", "diagnostics": diagnostics })
            }
            other => {
                return Err(ToolError::pre_validation(
                    ToolErrorKind::InvalidArguments,
                    format!(
                        "unknown action `{other}`; expected \
                         hover|definition|references|symbols|diagnostics"
                    ),
                ));
            }
        };

        Ok(ToolOutput::success(result))
    }
}

fn require_position(
    action: &str,
    line: Option<u32>,
    column: Option<u32>,
) -> Result<(u32, u32), ToolError> {
    match (line, column) {
        (Some(l), Some(c)) => Ok((l, c)),
        _ => Err(ToolError::pre_validation(
            ToolErrorKind::InvalidArguments,
            format!("action `{action}` requires `line` and `column`"),
        )),
    }
}

fn map_backend_err(err: &LspBackendError) -> ToolError {
    ToolError::ExecutionFailed {
        reason: err.to_string(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::super::backend::{
        LspBackend, LspBackendError, LspDiagnostic, LspDiagnosticSeverity, LspHover, LspLocation,
        LspSymbol, LspSymbolKind,
    };
    use super::*;
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;

    fn envelope(args: Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_owned(),
            tool_name: "lsp".to_owned(),
            model_args: args,
            metadata: Value::Null,
        }
    }

    struct MockBackend {
        location: LspLocation,
    }

    impl MockBackend {
        fn new() -> Self {
            Self {
                location: LspLocation {
                    path: "/tmp/src/lib.rs".to_owned(),
                    line: 10,
                    column: 4,
                    end_line: 10,
                    end_column: 14,
                },
            }
        }
    }

    #[async_trait]
    impl LspBackend for MockBackend {
        async fn hover(
            &self,
            _path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Option<LspHover>, LspBackendError> {
            Ok(Some(LspHover {
                content: "fn answer() -> u32".to_owned(),
                range: Some(self.location.clone()),
            }))
        }

        async fn definition(
            &self,
            _path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Vec<LspLocation>, LspBackendError> {
            Ok(vec![self.location.clone()])
        }

        async fn references(
            &self,
            _path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Vec<LspLocation>, LspBackendError> {
            Ok(vec![self.location.clone(), self.location.clone()])
        }

        async fn symbols(&self, _path: &Path) -> Result<Vec<LspSymbol>, LspBackendError> {
            Ok(vec![LspSymbol {
                name: "answer".to_owned(),
                kind: LspSymbolKind::Function,
                location: self.location.clone(),
            }])
        }

        async fn diagnostics(&self, _path: &Path) -> Result<Vec<LspDiagnostic>, LspBackendError> {
            Ok(vec![LspDiagnostic {
                severity: LspDiagnosticSeverity::Warning,
                message: "unused variable".to_owned(),
                line: 5,
                column: 9,
                end_line: 5,
                end_column: 16,
                source: Some("rust-analyzer".to_owned()),
                code: Some("unused_variables".to_owned()),
            }])
        }
    }

    #[test]
    fn tool_object_safe() {
        let _: Box<dyn Tool + Send + Sync> = Box::new(LspTool::new());
    }

    #[test]
    fn name_and_effect() {
        let tool = LspTool::new();
        assert_eq!(tool.name(), "lsp");
        assert_eq!(tool.effect(), ToolEffect::ReadOnly);
    }

    #[test]
    fn input_schema_declares_action_enum() {
        let tool = LspTool::new();
        let schema = tool.input_schema();
        let actions = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum");
        let names: Vec<&str> = actions.iter().filter_map(Value::as_str).collect();
        for action in [
            "hover",
            "definition",
            "references",
            "symbols",
            "diagnostics",
        ] {
            assert!(names.contains(&action), "action {action} missing");
        }
    }

    #[tokio::test]
    async fn no_backend_returns_execution_failed() {
        let tool = LspTool::new();
        let env = envelope(json!({ "action": "symbols", "path": "/tmp/x.rs" }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("no backend must fail");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(reason.contains("no LSP backend connected"), "got: {reason}");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_action_is_pre_validation_failure() {
        let tool = LspTool::with_backend(Arc::new(MockBackend::new()));
        let env = envelope(json!({ "action": "rename", "path": "/tmp/x.rs" }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("unknown action must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn hover_requires_position() {
        let tool = LspTool::with_backend(Arc::new(MockBackend::new()));
        let env = envelope(json!({ "action": "hover", "path": "/tmp/x.rs" }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("hover without position must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn hover_action_returns_hover_payload() {
        let tool = LspTool::with_backend(Arc::new(MockBackend::new()));
        let env = envelope(json!({
            "action": "hover",
            "path": "/tmp/x.rs",
            "line": 10_u32,
            "column": 4_u32,
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("hover ok");
        assert_eq!(out.content["action"], "hover");
        assert_eq!(out.content["hover"]["content"], "fn answer() -> u32");
    }

    #[tokio::test]
    async fn definition_action_returns_locations() {
        let tool = LspTool::with_backend(Arc::new(MockBackend::new()));
        let env = envelope(json!({
            "action": "definition",
            "path": "/tmp/x.rs",
            "line": 10_u32,
            "column": 4_u32,
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("definition ok");
        assert_eq!(out.content["action"], "definition");
        let locations = out.content["locations"]
            .as_array()
            .expect("locations array");
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0]["line"], 10);
    }

    #[tokio::test]
    async fn references_action_returns_locations() {
        let tool = LspTool::with_backend(Arc::new(MockBackend::new()));
        let env = envelope(json!({
            "action": "references",
            "path": "/tmp/x.rs",
            "line": 10_u32,
            "column": 4_u32,
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("references ok");
        let locations = out.content["locations"]
            .as_array()
            .expect("locations array");
        assert_eq!(locations.len(), 2);
    }

    #[tokio::test]
    async fn symbols_action_returns_symbol_list() {
        let tool = LspTool::with_backend(Arc::new(MockBackend::new()));
        let env = envelope(json!({
            "action": "symbols",
            "path": "/tmp/x.rs",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("symbols ok");
        let symbols = out.content["symbols"].as_array().expect("symbols array");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0]["name"], "answer");
        assert_eq!(symbols[0]["kind"], "function");
    }

    #[tokio::test]
    async fn diagnostics_action_returns_diagnostic_list() {
        let tool = LspTool::with_backend(Arc::new(MockBackend::new()));
        let env = envelope(json!({
            "action": "diagnostics",
            "path": "/tmp/x.rs",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("diagnostics ok");
        let diagnostics = out.content["diagnostics"]
            .as_array()
            .expect("diagnostics array");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["severity"], "warning");
        assert_eq!(diagnostics[0]["source"], "rust-analyzer");
    }

    #[tokio::test]
    async fn missing_path_fails_pre_validation() {
        let tool = LspTool::with_backend(Arc::new(MockBackend::new()));
        let env = envelope(json!({ "action": "symbols" }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("missing path must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    /// Backend that records the path each call receives, so tests can
    /// assert what the tool actually resolved before dispatch.
    struct PathRecordingBackend {
        seen: std::sync::Mutex<Vec<std::path::PathBuf>>,
    }

    impl PathRecordingBackend {
        fn new() -> Self {
            Self {
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LspBackend for PathRecordingBackend {
        async fn hover(
            &self,
            _path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Option<LspHover>, LspBackendError> {
            Ok(None)
        }
        async fn definition(
            &self,
            _path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Vec<LspLocation>, LspBackendError> {
            Ok(Vec::new())
        }
        async fn references(
            &self,
            _path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Vec<LspLocation>, LspBackendError> {
            Ok(Vec::new())
        }
        async fn symbols(&self, path: &Path) -> Result<Vec<LspSymbol>, LspBackendError> {
            self.seen.lock().unwrap().push(path.to_path_buf());
            Ok(Vec::new())
        }
        async fn diagnostics(&self, _path: &Path) -> Result<Vec<LspDiagnostic>, LspBackendError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn relative_path_resolves_against_agent_working_dir() {
        use crate::tool::context::SharedWorkingDir;

        let backend = Arc::new(PathRecordingBackend::new());
        let tool = LspTool::with_backend(backend.clone());
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(std::path::PathBuf::from(
            "/agent/workdir",
        )));

        let env = envelope(json!({ "action": "symbols", "path": "src/lib.rs" }));
        tool.execute(&env, &ctx).await.expect("symbols ok");

        let seen = backend.seen.lock().unwrap();
        assert_eq!(
            seen.as_slice(),
            &[std::path::PathBuf::from("/agent/workdir/src/lib.rs")],
            "relative path must resolve against the agent working dir, not process CWD"
        );
    }

    #[tokio::test]
    async fn confined_context_refuses_out_of_workspace_absolute_path() {
        use crate::tool::context::SharedWorkingDir;

        let root = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(PathRecordingBackend::new());
        let tool = LspTool::with_backend(backend.clone());
        let mut ctx =
            ToolContext::with_working_dir(SharedWorkingDir::new(root.path().to_path_buf()));
        ctx.confine_to_workspace(root.path().to_path_buf());

        let env = envelope(json!({ "action": "symbols", "path": "/etc/hosts" }));
        let out = tool
            .execute(&env, &ctx)
            .await
            .expect("refusal is a tool output");
        assert!(out.is_error(), "out-of-workspace path must be refused");
        assert_eq!(out.content["kind"], "confinement_refused");
        assert!(
            backend.seen.lock().unwrap().is_empty(),
            "backend must never see a refused path"
        );
    }

    #[tokio::test]
    async fn confined_context_refuses_relative_escape() {
        use crate::tool::context::SharedWorkingDir;

        let outer = tempfile::tempdir().expect("tempdir");
        let root = outer.path().join("ws");
        std::fs::create_dir(&root).expect("mkdir");
        std::fs::write(outer.path().join("secret.rs"), "fn s() {}").expect("write");

        let backend = Arc::new(PathRecordingBackend::new());
        let tool = LspTool::with_backend(backend.clone());
        let mut ctx = ToolContext::with_working_dir(SharedWorkingDir::new(root.clone()));
        ctx.confine_to_workspace(root);

        let env = envelope(json!({ "action": "symbols", "path": "../secret.rs" }));
        let out = tool
            .execute(&env, &ctx)
            .await
            .expect("refusal is a tool output");
        assert!(out.is_error(), "`..` escape must be refused");
        assert!(backend.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn confined_context_allows_in_workspace_path() {
        use crate::tool::context::SharedWorkingDir;

        let root = tempfile::tempdir().expect("tempdir");
        let file = root.path().join("lib.rs");
        std::fs::write(&file, "fn ok() {}").expect("write");

        let backend = Arc::new(PathRecordingBackend::new());
        let tool = LspTool::with_backend(backend.clone());
        let mut ctx =
            ToolContext::with_working_dir(SharedWorkingDir::new(root.path().to_path_buf()));
        ctx.confine_to_workspace(root.path().to_path_buf());

        let env = envelope(json!({ "action": "symbols", "path": "lib.rs" }));
        let out = tool.execute(&env, &ctx).await.expect("symbols ok");
        assert!(!out.is_error(), "in-workspace path must pass confinement");
        assert_eq!(backend.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn backend_error_propagates_as_execution_failed() {
        struct FailingBackend;

        #[async_trait]
        impl LspBackend for FailingBackend {
            async fn hover(
                &self,
                _p: &Path,
                _l: u32,
                _c: u32,
            ) -> Result<Option<LspHover>, LspBackendError> {
                Err(LspBackendError::NoServerForFile {
                    path: "/tmp/x.rs".to_owned(),
                })
            }
            async fn definition(
                &self,
                _p: &Path,
                _l: u32,
                _c: u32,
            ) -> Result<Vec<LspLocation>, LspBackendError> {
                Ok(Vec::new())
            }
            async fn references(
                &self,
                _p: &Path,
                _l: u32,
                _c: u32,
            ) -> Result<Vec<LspLocation>, LspBackendError> {
                Ok(Vec::new())
            }
            async fn symbols(&self, _p: &Path) -> Result<Vec<LspSymbol>, LspBackendError> {
                Ok(Vec::new())
            }
            async fn diagnostics(&self, _p: &Path) -> Result<Vec<LspDiagnostic>, LspBackendError> {
                Ok(Vec::new())
            }
        }

        let tool = LspTool::with_backend(Arc::new(FailingBackend));
        let env = envelope(json!({
            "action": "hover",
            "path": "/tmp/x.rs",
            "line": 0_u32,
            "column": 0_u32,
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("backend error must propagate");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(reason.contains("no LSP server"), "got: {reason}");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }
}
