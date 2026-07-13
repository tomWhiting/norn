//! Type mapping, retry, and filesystem helpers shared across the workspace
//! backend submodules.
//!
//! Converts `lsp-types` shapes into norn's plain serde structs (CO13) and
//! provides the content-modified retry primitive used by every method on
//! [`super::adapter::WorkspaceLspBackend`].

use std::error::Error as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::super::backend::{
    LspBackendError, LspDiagnostic, LspDiagnosticSeverity, LspHover, LspLocation, LspSymbol,
    LspSymbolKind,
};

pub(super) const CONTENT_MODIFIED_CODE: i32 = -32801;
pub(super) const CONTENT_MODIFIED_RETRIES: u32 = 3;
pub(super) const CONTENT_MODIFIED_DELAY: Duration = Duration::from_millis(300);

/// JSON-RPC "method not found" error code, returned by servers that do not
/// implement an optional extension method.
pub(super) const METHOD_NOT_FOUND_CODE: i32 = -32601;

// ─── Retry ─────────────────────────────────────────────────────────────

fn is_content_modified(err: &lsp::error::LspError) -> bool {
    matches!(err, lsp::error::LspError::JsonRpc { code, .. } if *code == CONTENT_MODIFIED_CODE)
}

pub(super) async fn retry_on_content_modified<F, Fut, T>(mut op: F) -> Result<T, LspBackendError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = lsp::error::LspResult<T>>,
{
    for attempt in 0..CONTENT_MODIFIED_RETRIES {
        match op().await {
            Ok(val) => return Ok(val),
            Err(ref e) if is_content_modified(e) && attempt + 1 < CONTENT_MODIFIED_RETRIES => {
                tokio::time::sleep(CONTENT_MODIFIED_DELAY).await;
            }
            Err(e) => return Err(map_lsp_error(e)),
        }
    }
    Err(LspBackendError::ProtocolError {
        reason: "content modified after retries exhausted".to_owned(),
    })
}

// ─── Filesystem ────────────────────────────────────────────────────────

pub(super) async fn read_file(path: &Path) -> Result<String, LspBackendError> {
    let _descriptor_permit = crate::resource::acquire_filesystem_operation()
        .map_err(|error| LspBackendError::DescriptorAdmission(Box::new(error)))?;
    tokio::fs::read_to_string(path)
        .await
        .map_err(|e| LspBackendError::ProtocolError {
            reason: format!("failed to read {}: {e}", path.display()),
        })
}

/// Sentinel returned by [`file_mtime_or_deleted`] when the file no longer
/// exists on disk.
pub(super) enum MtimeResult {
    /// The file exists with the given modification time.
    Ok(SystemTime),
    /// The file no longer exists on disk.
    Deleted,
}

pub(super) fn file_mtime_or_deleted(path: &Path) -> Result<MtimeResult, LspBackendError> {
    let _descriptor_permit = crate::resource::acquire_filesystem_operation()
        .map_err(|error| LspBackendError::DescriptorAdmission(Box::new(error)))?;
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => Ok(MtimeResult::Ok(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MtimeResult::Deleted),
        Err(e) => Err(LspBackendError::ProtocolError {
            reason: format!("failed to stat {}: {e}", path.display()),
        }),
    }
}

pub(super) fn file_mtime(path: &Path) -> Result<SystemTime, LspBackendError> {
    let _descriptor_permit = crate::resource::acquire_filesystem_operation()
        .map_err(|error| LspBackendError::DescriptorAdmission(Box::new(error)))?;
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| LspBackendError::ProtocolError {
            reason: format!("failed to stat {}: {e}", path.display()),
        })
}

// ─── URI helpers ───────────────────────────────────────────────────────

pub(super) fn path_to_uri(path: &Path) -> Result<lsp_types::Uri, LspBackendError> {
    let url = url::Url::from_file_path(path).map_err(|()| LspBackendError::ProtocolError {
        reason: format!(
            "failed to build URI for {}: not an absolute path",
            path.display()
        ),
    })?;
    url.as_str()
        .parse()
        .map_err(|_err| LspBackendError::ProtocolError {
            reason: format!("failed to parse URI for {}", path.display()),
        })
}

pub(super) fn uri_to_path(uri: &lsp_types::Uri) -> String {
    url::Url::parse(uri.as_str())
        .ok()
        .and_then(|u| u.to_file_path().ok())
        .map_or_else(
            || uri.as_str().to_owned(),
            |p| p.to_string_lossy().into_owned(),
        )
}

pub(super) fn map_lsp_error(err: lsp::error::LspError) -> LspBackendError {
    match err {
        lsp::error::LspError::Configuration(msg) => LspBackendError::NoServerForFile { path: msg },
        lsp::error::LspError::Timeout(_) => LspBackendError::Timeout,
        lsp::error::LspError::Transport(lsp::error::TransportError::Admission(error)) => {
            if let Some(admission) = error.source().and_then(|source| {
                source.downcast_ref::<crate::resource::DescriptorAdmissionError>()
            }) {
                LspBackendError::DescriptorAdmission(Box::new(admission.clone()))
            } else {
                LspBackendError::ProtocolError {
                    reason: error.to_string(),
                }
            }
        }
        other => LspBackendError::ProtocolError {
            reason: other.to_string(),
        },
    }
}

// ─── Type Mapping ──────────────────────────────────────────────────────

pub(super) fn map_hover(hover: lsp_types::Hover, path: &Path) -> LspHover {
    let content = match hover.contents {
        lsp_types::HoverContents::Scalar(ms) => markup_string_to_text(ms),
        lsp_types::HoverContents::Array(parts) => parts
            .into_iter()
            .map(markup_string_to_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lsp_types::HoverContents::Markup(mc) => mc.value,
    };
    let range = hover.range.map(|r| range_to_location(r, path));
    LspHover { content, range }
}

fn markup_string_to_text(ms: lsp_types::MarkedString) -> String {
    match ms {
        lsp_types::MarkedString::String(s) => s,
        lsp_types::MarkedString::LanguageString(ls) => {
            format!("```{}\n{}\n```", ls.language, ls.value)
        }
    }
}

pub(super) fn map_goto_response(resp: &lsp_types::GotoDefinitionResponse) -> Vec<LspLocation> {
    match resp {
        lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![map_location(loc)],
        lsp_types::GotoDefinitionResponse::Array(locs) => locs.iter().map(map_location).collect(),
        lsp_types::GotoDefinitionResponse::Link(links) => {
            links.iter().map(map_location_link).collect()
        }
    }
}

pub(super) fn map_location(loc: &lsp_types::Location) -> LspLocation {
    let path = uri_to_path(&loc.uri);
    LspLocation {
        path,
        line: loc.range.start.line + 1,
        column: loc.range.start.character + 1,
        end_line: loc.range.end.line + 1,
        end_column: loc.range.end.character + 1,
    }
}

fn map_location_link(link: &lsp_types::LocationLink) -> LspLocation {
    let path = uri_to_path(&link.target_uri);
    LspLocation {
        path,
        line: link.target_selection_range.start.line + 1,
        column: link.target_selection_range.start.character + 1,
        end_line: link.target_selection_range.end.line + 1,
        end_column: link.target_selection_range.end.character + 1,
    }
}

pub(super) fn range_to_location(range: lsp_types::Range, path: &Path) -> LspLocation {
    LspLocation {
        path: path.to_string_lossy().into_owned(),
        line: range.start.line + 1,
        column: range.start.character + 1,
        end_line: range.end.line + 1,
        end_column: range.end.character + 1,
    }
}

pub(super) fn map_document_symbols(
    resp: &lsp_types::DocumentSymbolResponse,
    path: &Path,
) -> Vec<LspSymbol> {
    match resp {
        lsp_types::DocumentSymbolResponse::Flat(symbols) => {
            symbols.iter().map(map_symbol_information).collect()
        }
        lsp_types::DocumentSymbolResponse::Nested(symbols) => {
            let mut out = Vec::new();
            for sym in symbols {
                flatten_document_symbol(sym, path, &mut out);
            }
            out
        }
    }
}

fn map_symbol_information(si: &lsp_types::SymbolInformation) -> LspSymbol {
    LspSymbol {
        name: si.name.clone(),
        kind: map_symbol_kind(si.kind),
        location: map_location(&si.location),
    }
}

fn flatten_document_symbol(sym: &lsp_types::DocumentSymbol, path: &Path, out: &mut Vec<LspSymbol>) {
    out.push(LspSymbol {
        name: sym.name.clone(),
        kind: map_symbol_kind(sym.kind),
        location: range_to_location(sym.selection_range, path),
    });
    if let Some(ref children) = sym.children {
        for child in children {
            flatten_document_symbol(child, path, out);
        }
    }
}

fn map_symbol_kind(kind: lsp_types::SymbolKind) -> LspSymbolKind {
    match kind {
        lsp_types::SymbolKind::FILE => LspSymbolKind::File,
        lsp_types::SymbolKind::MODULE => LspSymbolKind::Module,
        lsp_types::SymbolKind::NAMESPACE => LspSymbolKind::Namespace,
        lsp_types::SymbolKind::PACKAGE => LspSymbolKind::Package,
        lsp_types::SymbolKind::CLASS => LspSymbolKind::Class,
        lsp_types::SymbolKind::METHOD => LspSymbolKind::Method,
        lsp_types::SymbolKind::PROPERTY => LspSymbolKind::Property,
        lsp_types::SymbolKind::FIELD => LspSymbolKind::Field,
        lsp_types::SymbolKind::CONSTRUCTOR => LspSymbolKind::Constructor,
        lsp_types::SymbolKind::ENUM => LspSymbolKind::Enum,
        lsp_types::SymbolKind::INTERFACE => LspSymbolKind::Interface,
        lsp_types::SymbolKind::FUNCTION => LspSymbolKind::Function,
        lsp_types::SymbolKind::VARIABLE => LspSymbolKind::Variable,
        lsp_types::SymbolKind::CONSTANT => LspSymbolKind::Constant,
        lsp_types::SymbolKind::STRING => LspSymbolKind::String,
        lsp_types::SymbolKind::NUMBER => LspSymbolKind::Number,
        lsp_types::SymbolKind::BOOLEAN => LspSymbolKind::Boolean,
        lsp_types::SymbolKind::ARRAY => LspSymbolKind::Array,
        lsp_types::SymbolKind::OBJECT => LspSymbolKind::Object,
        lsp_types::SymbolKind::KEY => LspSymbolKind::Key,
        lsp_types::SymbolKind::NULL => LspSymbolKind::Null,
        lsp_types::SymbolKind::ENUM_MEMBER => LspSymbolKind::EnumMember,
        lsp_types::SymbolKind::STRUCT => LspSymbolKind::Struct,
        lsp_types::SymbolKind::EVENT => LspSymbolKind::Event,
        lsp_types::SymbolKind::OPERATOR => LspSymbolKind::Operator,
        lsp_types::SymbolKind::TYPE_PARAMETER => LspSymbolKind::TypeParameter,
        _ => LspSymbolKind::Other,
    }
}

pub(super) fn map_diagnostic(diag: lsp_types::Diagnostic) -> LspDiagnostic {
    let severity = match diag.severity {
        Some(lsp_types::DiagnosticSeverity::ERROR) => LspDiagnosticSeverity::Error,
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => LspDiagnosticSeverity::Information,
        Some(lsp_types::DiagnosticSeverity::HINT) => LspDiagnosticSeverity::Hint,
        _ => LspDiagnosticSeverity::Warning,
    };
    let code = diag.code.map(|c| match c {
        lsp_types::NumberOrString::Number(n) => n.to_string(),
        lsp_types::NumberOrString::String(s) => s,
    });
    LspDiagnostic {
        severity,
        message: diag.message,
        line: diag.range.start.line + 1,
        column: diag.range.start.character + 1,
        end_line: diag.range.end.line + 1,
        end_column: diag.range.end.character + 1,
        source: diag.source,
        code,
    }
}

// `deprecated` is allowed because `lsp_types::DocumentSymbol` /
// `SymbolInformation` carry a deprecated `deprecated` field that struct
// literals must still populate.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, deprecated)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn range(sl: u32, sc: u32, el: u32, ec: u32) -> lsp_types::Range {
        lsp_types::Range::new(
            lsp_types::Position::new(sl, sc),
            lsp_types::Position::new(el, ec),
        )
    }

    fn uri(s: &str) -> lsp_types::Uri {
        s.parse().expect("valid uri")
    }

    #[test]
    fn map_lsp_error_preserves_descriptor_admission_type() -> Result<(), Box<dyn std::error::Error>>
    {
        let governor = crate::resource::DescriptorGovernor::with_capacity(1);
        let admission = governor
            .try_acquire(2)
            .err()
            .ok_or_else(|| std::io::Error::other("oversized admission unexpectedly succeeded"))?;
        let lsp_error = lsp::error::LspError::Transport(lsp::error::TransportError::Admission(
            lsp::server::admission::ProcessAdmissionError::new(admission),
        ));
        assert!(matches!(
            map_lsp_error(lsp_error),
            LspBackendError::DescriptorAdmission(_)
        ));
        Ok(())
    }

    // ─── One-based conversion pins ─────────────────────────────────────

    #[test]
    fn map_location_converts_zero_based_wire_positions_to_one_based() {
        let loc = lsp_types::Location::new(uri("file:///tmp/x.rs"), range(0, 0, 2, 7));
        let mapped = map_location(&loc);
        assert_eq!(mapped.path, "/tmp/x.rs");
        assert_eq!(
            (
                mapped.line,
                mapped.column,
                mapped.end_line,
                mapped.end_column
            ),
            (1, 1, 3, 8),
            "wire line 0 / col 0 must surface as line 1 / col 1"
        );
    }

    #[test]
    fn range_to_location_converts_to_one_based() {
        let mapped = range_to_location(range(9, 4, 9, 12), Path::new("/tmp/y.rs"));
        assert_eq!(mapped.path, "/tmp/y.rs");
        assert_eq!(
            (
                mapped.line,
                mapped.column,
                mapped.end_line,
                mapped.end_column
            ),
            (10, 5, 10, 13)
        );
    }

    #[test]
    fn map_diagnostic_converts_to_one_based_and_maps_metadata() {
        let diag = lsp_types::Diagnostic {
            range: range(4, 8, 4, 15),
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            code: Some(lsp_types::NumberOrString::String("E0308".to_owned())),
            source: Some("rustc".to_owned()),
            message: "mismatched types".to_owned(),
            ..Default::default()
        };
        let mapped = map_diagnostic(diag);
        assert_eq!(mapped.severity, LspDiagnosticSeverity::Error);
        assert_eq!(
            (
                mapped.line,
                mapped.column,
                mapped.end_line,
                mapped.end_column
            ),
            (5, 9, 5, 16)
        );
        assert_eq!(mapped.source.as_deref(), Some("rustc"));
        assert_eq!(mapped.code.as_deref(), Some("E0308"));
    }

    #[test]
    fn map_diagnostic_defaults_missing_severity_to_warning_and_maps_numeric_code() {
        let diag = lsp_types::Diagnostic {
            range: range(0, 0, 0, 1),
            severity: None,
            code: Some(lsp_types::NumberOrString::Number(42)),
            message: "hm".to_owned(),
            ..Default::default()
        };
        let mapped = map_diagnostic(diag);
        assert_eq!(mapped.severity, LspDiagnosticSeverity::Warning);
        assert_eq!(mapped.code.as_deref(), Some("42"));
    }

    #[test]
    fn map_diagnostic_maps_information_and_hint() {
        for (wire, expected) in [
            (
                lsp_types::DiagnosticSeverity::INFORMATION,
                LspDiagnosticSeverity::Information,
            ),
            (
                lsp_types::DiagnosticSeverity::HINT,
                LspDiagnosticSeverity::Hint,
            ),
        ] {
            let diag = lsp_types::Diagnostic {
                range: range(0, 0, 0, 1),
                severity: Some(wire),
                message: "m".to_owned(),
                ..Default::default()
            };
            assert_eq!(map_diagnostic(diag).severity, expected);
        }
    }

    // ─── Goto / symbol response shapes ─────────────────────────────────

    #[test]
    fn map_goto_response_scalar_array_and_link_are_one_based() {
        let loc = lsp_types::Location::new(uri("file:///tmp/x.rs"), range(1, 2, 1, 5));

        let scalar = lsp_types::GotoDefinitionResponse::Scalar(loc.clone());
        assert_eq!(map_goto_response(&scalar)[0].line, 2);

        let array = lsp_types::GotoDefinitionResponse::Array(vec![loc.clone(), loc]);
        assert_eq!(map_goto_response(&array).len(), 2);

        let link = lsp_types::LocationLink {
            origin_selection_range: None,
            target_uri: uri("file:///tmp/z.rs"),
            target_range: range(0, 0, 10, 0),
            target_selection_range: range(3, 4, 3, 9),
        };
        let mapped = map_goto_response(&lsp_types::GotoDefinitionResponse::Link(vec![link]));
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].path, "/tmp/z.rs");
        assert_eq!((mapped[0].line, mapped[0].column), (4, 5));
    }

    #[test]
    fn map_document_symbols_flattens_nested_children() {
        let inner = lsp_types::DocumentSymbol {
            name: "inner".to_owned(),
            detail: None,
            kind: lsp_types::SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: range(2, 0, 4, 0),
            selection_range: range(2, 7, 2, 12),
            children: None,
        };
        let nested = lsp_types::DocumentSymbol {
            name: "outer".to_owned(),
            detail: None,
            kind: lsp_types::SymbolKind::MODULE,
            tags: None,
            deprecated: None,
            range: range(0, 0, 20, 0),
            selection_range: range(0, 4, 0, 9),
            children: Some(vec![inner]),
        };
        let resp = lsp_types::DocumentSymbolResponse::Nested(vec![nested]);
        let mapped = map_document_symbols(&resp, Path::new("/tmp/mod.rs"));
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].name, "outer");
        assert_eq!(mapped[0].kind, LspSymbolKind::Module);
        assert_eq!(mapped[0].location.line, 1);
        assert_eq!(mapped[1].name, "inner");
        assert_eq!(mapped[1].kind, LspSymbolKind::Function);
        assert_eq!((mapped[1].location.line, mapped[1].location.column), (3, 8));
    }

    #[test]
    fn map_document_symbols_flat_variant() {
        let info = lsp_types::SymbolInformation {
            name: "answer".to_owned(),
            kind: lsp_types::SymbolKind::CONSTANT,
            tags: None,
            deprecated: None,
            location: lsp_types::Location::new(uri("file:///tmp/x.rs"), range(0, 6, 0, 12)),
            container_name: None,
        };
        let resp = lsp_types::DocumentSymbolResponse::Flat(vec![info]);
        let mapped = map_document_symbols(&resp, Path::new("/tmp/x.rs"));
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].kind, LspSymbolKind::Constant);
        assert_eq!((mapped[0].location.line, mapped[0].location.column), (1, 7));
    }

    // ─── Hover ─────────────────────────────────────────────────────────

    #[test]
    fn map_hover_scalar_markup_and_array_variants() {
        let scalar = lsp_types::Hover {
            contents: lsp_types::HoverContents::Scalar(lsp_types::MarkedString::String(
                "plain".to_owned(),
            )),
            range: Some(range(1, 0, 1, 5)),
        };
        let mapped = map_hover(scalar, Path::new("/tmp/x.rs"));
        assert_eq!(mapped.content, "plain");
        let r = mapped.range.expect("range");
        assert_eq!((r.line, r.column), (2, 1));

        let lang = lsp_types::Hover {
            contents: lsp_types::HoverContents::Array(vec![
                lsp_types::MarkedString::LanguageString(lsp_types::LanguageString {
                    language: "rust".to_owned(),
                    value: "fn f()".to_owned(),
                }),
                lsp_types::MarkedString::String("docs".to_owned()),
            ]),
            range: None,
        };
        let mapped = map_hover(lang, Path::new("/tmp/x.rs"));
        assert_eq!(mapped.content, "```rust\nfn f()\n```\n\ndocs");
        assert!(mapped.range.is_none());

        let markup = lsp_types::Hover {
            contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: "**bold**".to_owned(),
            }),
            range: None,
        };
        assert_eq!(
            map_hover(markup, Path::new("/tmp/x.rs")).content,
            "**bold**"
        );
    }

    // ─── Symbol kinds ──────────────────────────────────────────────────

    #[test]
    fn map_symbol_kind_covers_standard_set_and_falls_back_to_other() {
        assert_eq!(
            map_symbol_kind(lsp_types::SymbolKind::STRUCT),
            LspSymbolKind::Struct
        );
        assert_eq!(
            map_symbol_kind(lsp_types::SymbolKind::TYPE_PARAMETER),
            LspSymbolKind::TypeParameter
        );
        // An out-of-range kind (servers may emit proposed kinds).
        let unknown: lsp_types::SymbolKind =
            serde_json::from_value(serde_json::json!(255)).expect("deserializes");
        assert_eq!(map_symbol_kind(unknown), LspSymbolKind::Other);
    }

    // ─── URI helpers ───────────────────────────────────────────────────

    #[test]
    fn path_uri_round_trip() {
        let path = Path::new("/tmp/some dir/file.rs");
        let u = path_to_uri(path).expect("uri");
        assert_eq!(uri_to_path(&u), "/tmp/some dir/file.rs");
    }

    #[test]
    fn path_to_uri_rejects_relative_path() {
        let err = path_to_uri(Path::new("relative/file.rs")).expect_err("must fail");
        assert!(
            matches!(err, LspBackendError::ProtocolError { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn uri_to_path_falls_back_to_raw_uri_for_non_file_schemes() {
        let u = uri("untitled:Untitled-1");
        assert_eq!(uri_to_path(&u), "untitled:Untitled-1");
    }

    // ─── Filesystem helpers ────────────────────────────────────────────

    #[test]
    fn file_mtime_or_deleted_distinguishes_missing_from_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let present = dir.path().join("a.txt");
        std::fs::write(&present, "x").expect("write");

        match file_mtime_or_deleted(&present).expect("stat ok") {
            MtimeResult::Ok(_) => {}
            MtimeResult::Deleted => panic!("present file reported deleted"),
        }
        match file_mtime_or_deleted(&dir.path().join("gone.txt")).expect("stat ok") {
            MtimeResult::Deleted => {}
            MtimeResult::Ok(_) => panic!("missing file reported present"),
        }
    }

    #[test]
    fn file_mtime_errors_on_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = file_mtime(&dir.path().join("gone.txt")).expect_err("must fail");
        assert!(matches!(err, LspBackendError::ProtocolError { .. }));
    }

    #[tokio::test]
    async fn read_file_errors_on_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = read_file(&dir.path().join("gone.txt"))
            .await
            .expect_err("must fail");
        assert!(matches!(err, LspBackendError::ProtocolError { .. }));
    }

    // ─── Error mapping and retry ───────────────────────────────────────

    #[test]
    fn map_lsp_error_variants() {
        assert!(matches!(
            map_lsp_error(lsp::error::LspError::Configuration("no server".to_owned())),
            LspBackendError::NoServerForFile { .. }
        ));
        assert!(matches!(
            map_lsp_error(lsp::error::LspError::Timeout(Duration::from_secs(1))),
            LspBackendError::Timeout
        ));
        assert!(matches!(
            map_lsp_error(lsp::error::LspError::ServerCrashed("boom".to_owned())),
            LspBackendError::ProtocolError { .. }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_on_content_modified_retries_then_succeeds() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let result: Result<u32, _> = retry_on_content_modified(|| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                if n == 0 {
                    Err(lsp::error::LspError::JsonRpc {
                        code: CONTENT_MODIFIED_CODE,
                        message: "content modified".to_owned(),
                    })
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert_eq!(result.expect("succeeds on retry"), 7);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_on_content_modified_exhausts_retries() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let result: Result<u32, _> = retry_on_content_modified(|| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async {
                Err(lsp::error::LspError::JsonRpc {
                    code: CONTENT_MODIFIED_CODE,
                    message: "content modified".to_owned(),
                })
            }
        })
        .await;
        let err = result.expect_err("must exhaust");
        assert!(matches!(err, LspBackendError::ProtocolError { .. }));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            CONTENT_MODIFIED_RETRIES
        );
    }

    #[tokio::test]
    async fn retry_on_content_modified_passes_other_errors_through_immediately() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let result: Result<u32, _> = retry_on_content_modified(|| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Err(lsp::error::LspError::Configuration("no server".to_owned())) }
        })
        .await;
        assert!(matches!(
            result.expect_err("must fail"),
            LspBackendError::NoServerForFile { .. }
        ));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    // Keep PathBuf import exercised for the helpers above.
    #[test]
    fn uri_to_path_returns_plain_string_path() {
        let u = uri("file:///tmp/x.rs");
        assert_eq!(PathBuf::from(uri_to_path(&u)), PathBuf::from("/tmp/x.rs"));
    }
}
