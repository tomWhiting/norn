//! Type mapping, retry, and filesystem helpers shared across the workspace
//! backend submodules.
//!
//! Converts `lsp-types` shapes into norn's plain serde structs (CO13) and
//! provides the content-modified retry primitive used by every method on
//! [`super::adapter::WorkspaceLspBackend`].

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::super::backend::{
    LspBackendError, LspDiagnostic, LspDiagnosticSeverity, LspHover, LspLocation, LspSymbol,
    LspSymbolKind,
};

pub(super) const CONTENT_MODIFIED_CODE: i32 = -32801;
pub(super) const CONTENT_MODIFIED_RETRIES: u32 = 3;
pub(super) const CONTENT_MODIFIED_DELAY: Duration = Duration::from_millis(300);

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
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => Ok(MtimeResult::Ok(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MtimeResult::Deleted),
        Err(e) => Err(LspBackendError::ProtocolError {
            reason: format!("failed to stat {}: {e}", path.display()),
        }),
    }
}

pub(super) fn file_mtime(path: &Path) -> Result<SystemTime, LspBackendError> {
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
