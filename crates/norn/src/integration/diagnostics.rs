//! Norn-side diagnostic collection.
//!
//! Tool failures, schema violations, and policy violations are reported as
//! [`NornDiagnostic`] values into a [`DiagnosticCollector`]. The collector
//! is a passive sink: it owns no rendering, formatting, or transport — only
//! the in-memory data and the drain operation that hands the accumulated
//! values to a consumer.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::{SchemaError, ToolError};

/// Severity classification for [`NornDiagnostic`].
///
/// Ordered from most to least severe — `Error` > `Warning` > `Info` > `Hint`.
/// This matches the ordering of compiler diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// A failure that prevented some operation from succeeding.
    Error,
    /// A condition that did not block the operation but should be addressed.
    Warning,
    /// Informational message about an operation.
    Info,
    /// A non-actionable hint or suggestion.
    Hint,
}

/// In-memory diagnostic record produced by Norn's integration surface.
///
/// Holds the data necessary for a downstream renderer to produce a
/// compiler-grade diagnostic message. Norn does not render or format —
/// it only collects.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NornDiagnostic {
    /// Severity classification.
    pub severity: DiagnosticSeverity,
    /// Machine-readable code identifying the diagnostic class.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Tool that produced the diagnostic, if applicable.
    pub source_tool: Option<String>,
    /// File path the diagnostic applies to, if applicable.
    pub file_path: Option<String>,
    /// Suggested remediation, if any.
    pub suggestion: Option<String>,
}

impl NornDiagnostic {
    /// Build a diagnostic from a [`SchemaError`].
    ///
    /// Always produces an `Error`-severity record with the code
    /// `schema-violation`, attributed to the `structured_output` tool.
    #[must_use]
    pub fn from_schema_error(err: &SchemaError) -> Self {
        let (message, suggestion) = match err {
            SchemaError::ValidationFailed { errors, .. } => (
                format!("schema validation failed: {}", errors.join("; ")),
                Some(
                    "re-emit the structured-output tool call with arguments matching the schema"
                        .to_owned(),
                ),
            ),
            SchemaError::Unreachable {
                attempts,
                validation_errors,
                ..
            } => (
                format!(
                    "schema unreachable after {attempts} attempts: {}",
                    validation_errors.join("; ")
                ),
                Some(
                    "relax the schema, switch model, or provide additional context to the agent"
                        .to_owned(),
                ),
            ),
            SchemaError::InvalidSchema { reason } => (
                format!("invalid schema: {reason}"),
                Some("fix the schema definition before re-running the step".to_owned()),
            ),
        };
        Self {
            severity: DiagnosticSeverity::Error,
            code: "schema-violation".to_owned(),
            message,
            source_tool: Some("structured_output".to_owned()),
            file_path: None,
            suggestion,
        }
    }

    /// Build a diagnostic for a tool that was blocked by pre-validation.
    ///
    /// Produces a `Warning`-severity record with code `tool-blocked`.
    #[must_use]
    pub fn from_tool_block(tool_name: impl Into<String>, reason: impl Into<String>) -> Self {
        let tool = tool_name.into();
        let reason = reason.into();
        Self {
            severity: DiagnosticSeverity::Warning,
            code: "tool-blocked".to_owned(),
            message: format!("tool '{tool}' blocked: {reason}"),
            source_tool: Some(tool),
            file_path: None,
            suggestion: None,
        }
    }

    /// Build a diagnostic from a [`ToolError::PreValidationFailed`].
    ///
    /// For other variants the diagnostic still records the error but uses a
    /// less specific code.
    #[must_use]
    pub fn from_tool_error(tool_name: impl Into<String>, err: &ToolError) -> Self {
        match err {
            ToolError::PreValidationFailed { payload } => {
                let mut diagnostic = Self::from_tool_block(tool_name, payload.message.clone());
                diagnostic.suggestion = payload.guidance().map(str::to_owned);
                diagnostic
            }
            ToolError::PostValidationFailed {
                reason,
                committed_output,
            } => {
                let tool = tool_name.into();
                let detail = committed_output
                    .as_ref()
                    .and_then(serde_json::Value::as_object);
                let message = match detail.and_then(|map| map.get("diagnostics")) {
                    Some(serde_json::Value::Array(arr)) if !arr.is_empty() => {
                        let joined = arr
                            .iter()
                            .filter_map(|d| {
                                d.get("message")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_owned)
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        if joined.is_empty() {
                            format!("tool '{tool}' post-validation failed: {reason}")
                        } else {
                            format!(
                                "tool '{tool}' post-validation failed: {reason}; diagnostics: {joined}"
                            )
                        }
                    }
                    _ => format!("tool '{tool}' post-validation failed: {reason}"),
                };
                let file_path = detail.and_then(|map| {
                    map.get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| {
                            map.get("files_modified")
                                .and_then(serde_json::Value::as_array)
                                .and_then(|files| files.first())
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                });
                Self {
                    severity: DiagnosticSeverity::Error,
                    code: "tool-post-validation-failed".to_owned(),
                    message,
                    source_tool: Some(tool),
                    file_path,
                    suggestion: None,
                }
            }
            ToolError::ExecutionFailed { reason } => {
                let tool = tool_name.into();
                Self {
                    severity: DiagnosticSeverity::Error,
                    code: "tool-execution-failed".to_owned(),
                    message: format!("tool '{tool}' execution failed: {reason}"),
                    source_tool: Some(tool),
                    file_path: None,
                    suggestion: None,
                }
            }
            ToolError::DescriptorExhausted(source) => {
                let tool = tool_name.into();
                Self {
                    severity: DiagnosticSeverity::Error,
                    code: "tool-resource-exhausted".to_owned(),
                    message: format!("tool '{tool}' failed: {source}"),
                    source_tool: Some(tool),
                    file_path: source.path.as_ref().map(|path| path.display().to_string()),
                    suggestion: Some("run `norn doctor` for descriptor diagnostics".to_owned()),
                }
            }
            ToolError::DescriptorAdmission(source) => {
                let tool = tool_name.into();
                Self {
                    severity: DiagnosticSeverity::Error,
                    code: "tool-resource-admission".to_owned(),
                    message: format!("tool '{tool}' was not admitted: {source}"),
                    source_tool: Some(tool),
                    file_path: None,
                    suggestion: Some("run `norn doctor` for descriptor diagnostics".to_owned()),
                }
            }
            ToolError::ToolNotFound { name } => Self {
                severity: DiagnosticSeverity::Error,
                code: "tool-not-found".to_owned(),
                message: format!("tool '{name}' not found in registry"),
                source_tool: Some(name.clone()),
                file_path: None,
                suggestion: None,
            },
            ToolError::MissingExtension { extension } => {
                let tool = tool_name.into();
                Self {
                    severity: DiagnosticSeverity::Error,
                    code: "tool-missing-extension".to_owned(),
                    message: format!(
                        "tool '{tool}' requires a tool-context extension that is not \
                         configured: {extension}"
                    ),
                    source_tool: Some(tool),
                    file_path: None,
                    suggestion: Some(format!(
                        "publish `{extension}` on the shared ToolContext (e.g. via \
                         `ToolContext::insert_extension`) before dispatching this tool"
                    )),
                }
            }
        }
    }
}

/// Append-only accumulator for [`NornDiagnostic`] values.
///
/// Shared across threads via [`Arc`]; uses interior mutability so a single
/// `&DiagnosticCollector` can be passed everywhere a producer might emit.
#[derive(Default)]
pub struct DiagnosticCollector {
    diagnostics: Mutex<Vec<NornDiagnostic>>,
}

impl DiagnosticCollector {
    /// Construct an empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct an `Arc`-wrapped collector for sharing across threads.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Append a diagnostic to the collection.
    pub fn report(&self, diagnostic: NornDiagnostic) {
        self.diagnostics.lock().push(diagnostic);
    }

    /// Return the number of diagnostics currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.diagnostics.lock().len()
    }

    /// True when no diagnostics have been collected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.diagnostics.lock().is_empty()
    }

    /// Snapshot the current diagnostics without clearing them.
    #[must_use]
    pub fn snapshot(&self) -> Vec<NornDiagnostic> {
        self.diagnostics.lock().clone()
    }

    /// Take all accumulated diagnostics, leaving the collector empty.
    #[must_use]
    pub fn drain(&self) -> Vec<NornDiagnostic> {
        std::mem::take(&mut *self.diagnostics.lock())
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
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    #[test]
    fn drain_returns_all_then_empties() {
        let collector = DiagnosticCollector::new();
        collector.report(NornDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "x".to_owned(),
            message: "boom".to_owned(),
            source_tool: None,
            file_path: None,
            suggestion: None,
        });
        collector.report(NornDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "y".to_owned(),
            message: "watch".to_owned(),
            source_tool: None,
            file_path: None,
            suggestion: None,
        });
        collector.report(NornDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: "z".to_owned(),
            message: "fyi".to_owned(),
            source_tool: None,
            file_path: None,
            suggestion: None,
        });

        assert_eq!(collector.len(), 3);
        let drained = collector.drain();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].code, "x");
        assert_eq!(drained[0].severity, DiagnosticSeverity::Error);
        assert_eq!(drained[1].code, "y");
        assert_eq!(drained[1].severity, DiagnosticSeverity::Warning);
        assert_eq!(drained[2].code, "z");
        assert_eq!(drained[2].severity, DiagnosticSeverity::Info);
        assert!(collector.is_empty());
    }

    #[test]
    fn snapshot_does_not_drain() {
        let collector = DiagnosticCollector::new();
        collector.report(NornDiagnostic {
            severity: DiagnosticSeverity::Hint,
            code: "hint".to_owned(),
            message: "h".to_owned(),
            source_tool: None,
            file_path: None,
            suggestion: None,
        });
        let snap = collector.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(collector.len(), 1);
    }

    #[test]
    fn shared_collector_is_arc_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<DiagnosticCollector>>();
        let _shared = DiagnosticCollector::shared();
    }

    #[test]
    fn from_schema_error_validation_produces_error_record() {
        let err = SchemaError::ValidationFailed {
            schema: serde_json::json!({"type": "object"}),
            output: serde_json::json!("not-an-object"),
            errors: vec!["expected object".to_owned()],
        };
        let diag = NornDiagnostic::from_schema_error(&err);
        assert_eq!(diag.severity, DiagnosticSeverity::Error);
        assert_eq!(diag.code, "schema-violation");
        assert_eq!(diag.source_tool.as_deref(), Some("structured_output"));
        assert!(diag.message.contains("expected object"));
        assert!(diag.suggestion.is_some());
    }

    #[test]
    fn from_schema_error_unreachable_produces_error_record() {
        let err = SchemaError::Unreachable {
            best_attempt: None,
            validation_errors: vec!["missing field".to_owned()],
            attempts: 3,
        };
        let diag = NornDiagnostic::from_schema_error(&err);
        assert_eq!(diag.severity, DiagnosticSeverity::Error);
        assert_eq!(diag.code, "schema-violation");
        assert!(diag.message.contains("3 attempts"));
    }

    #[test]
    fn from_tool_block_produces_warning() {
        let diag = NornDiagnostic::from_tool_block("bash", "command requires confirmation");
        assert_eq!(diag.severity, DiagnosticSeverity::Warning);
        assert_eq!(diag.code, "tool-blocked");
        assert_eq!(diag.source_tool.as_deref(), Some("bash"));
        assert!(diag.message.contains("command requires confirmation"));
    }

    #[test]
    fn from_tool_error_pre_validation_uses_tool_blocked() {
        use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};

        let err = ToolError::PreValidationFailed {
            payload: ToolErrorPayload::new(ToolErrorKind::Blocked, "must read first")
                .with_detail(serde_json::json!({ "guidance": "read the file first" })),
        };
        let diag = NornDiagnostic::from_tool_error("write", &err);
        assert_eq!(diag.code, "tool-blocked");
        assert_eq!(diag.severity, DiagnosticSeverity::Warning);
        assert!(diag.message.contains("must read first"), "{}", diag.message);
        assert_eq!(
            diag.suggestion.as_deref(),
            Some("read the file first"),
            "block guidance must surface as the diagnostic suggestion",
        );
    }

    #[test]
    fn from_tool_error_post_validation_is_error_severity() {
        let err = ToolError::PostValidationFailed {
            reason: "ast invalid".to_owned(),
            committed_output: None,
        };
        let diag = NornDiagnostic::from_tool_error("edit", &err);
        assert_eq!(diag.severity, DiagnosticSeverity::Error);
        assert_eq!(diag.code, "tool-post-validation-failed");
    }

    #[test]
    fn from_tool_error_post_validation_extracts_committed_diagnostics() {
        let err = ToolError::PostValidationFailed {
            reason: "clippy failed".to_owned(),
            committed_output: Some(serde_json::json!({
                "committed": true,
                "path": "src/lib.rs",
                "diagnostics": [
                    {"message": "unused variable `x`", "severity": "error"},
                    {"message": "missing semicolon", "severity": "error"},
                ],
            })),
        };
        let diag = NornDiagnostic::from_tool_error("edit", &err);
        assert_eq!(diag.severity, DiagnosticSeverity::Error);
        assert_eq!(diag.code, "tool-post-validation-failed");
        assert!(
            diag.message.contains("unused variable `x`"),
            "diagnostic detail must be preserved: {}",
            diag.message,
        );
        assert!(
            diag.message.contains("missing semicolon"),
            "all committed diagnostics must be surfaced: {}",
            diag.message,
        );
        assert_eq!(diag.file_path.as_deref(), Some("src/lib.rs"));
    }
}
