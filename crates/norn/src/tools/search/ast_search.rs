//! AST structural search via tree-sitter S-expression queries.
//!
//! Parses each candidate file with tree-sitter and evaluates an
//! S-expression query against it, returning matched node locations and
//! captured text.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use serde::Serialize;
use serde_json::json;
use tree_sitter::{Parser, Query as TsQuery, QueryCursor, StreamingIterator};

use crate::error::ToolError;
use crate::tool::failure::ToolErrorKind;
use crate::tool::traits::ToolOutput;
use crate::tools::ast::SyntaxLanguage;

use super::helpers::{GlobFilter, SkippedEntry, compile_glob, walk_tree};

/// A single AST-query hit.
#[derive(Debug, Serialize)]
pub(super) struct AstMatch {
    path: String,
    line: u32,
    column: u32,
    node_kind: String,
    capture_name: String,
    text: String,
}

/// A per-language query-compilation failure surfaced to the model.
#[derive(Debug, Serialize)]
struct QueryError {
    language: String,
    error: String,
}

/// Run a tree-sitter structural search under `root`.
///
/// Lazily compiles the `query_source` S-expression per language. A compile
/// failure for one language does not abort the search — other languages can
/// still produce matches — but every failure is reported in the output's
/// `query_errors` array. When the query fails to compile for *every*
/// language present under `root`, the call fails with a typed error
/// carrying the per-language messages, so an invalid query is never
/// mistaken for "no matches".
pub(super) fn run_ast_search(
    query_source: &str,
    root: &Path,
    glob_filter: Option<&str>,
    max_results: u32,
    include_ignored: bool,
) -> Result<ToolOutput, ToolError> {
    let compiled_filter: Option<GlobFilter> = compile_glob(glob_filter)?;

    let walked = walk_tree(root, include_ignored);
    let mut skipped = walked.skipped;

    let mut compiled: HashMap<SyntaxLanguage, Result<TsQuery, String>> = HashMap::new();
    let mut parser = Parser::new();
    let mut out: Vec<AstMatch> = Vec::new();
    let mut truncated = false;
    let cap = max_results as usize;

    for entry in walked.entries.iter().filter(|e| e.is_file) {
        if truncated {
            break;
        }
        let path = &entry.path;
        if let Some(filter) = compiled_filter.as_ref()
            && !filter.matches(path)
        {
            continue;
        }
        let Some(language) = SyntaxLanguage::from_path(path) else {
            continue;
        };

        let compiled_query = compiled.entry(language).or_insert_with(|| {
            let ts_lang = language.tree_sitter_language();
            TsQuery::new(&ts_lang, query_source).map_err(|e| {
                format!(
                    "invalid for {language:?} at offset {}: {}",
                    e.offset, e.message
                )
            })
        });
        let Ok(query) = &*compiled_query else {
            continue;
        };

        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            // Binary / non-UTF-8 content cannot be parsed as source code,
            // so it is not a lost result.
            Err(e) if e.kind() == ErrorKind::InvalidData => continue,
            Err(e) => {
                skipped.push(SkippedEntry {
                    path: path.to_string_lossy().into_owned(),
                    reason: format!("unreadable: {e}"),
                });
                continue;
            }
        };

        let Some(tree) = crate::tools::ast::parse_with(&mut parser, &source, language) else {
            skipped.push(SkippedEntry {
                path: path.to_string_lossy().into_owned(),
                reason: format!("tree-sitter failed to parse the file as {language:?}"),
            });
            continue;
        };

        let source_bytes = source.as_bytes();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), source_bytes);
        let capture_names = query.capture_names();

        let mut hit_cap = false;
        while let Some(m) = matches.next() {
            for capture in m.captures {
                if out.len() >= cap {
                    truncated = true;
                    hit_cap = true;
                    break;
                }
                let node = capture.node;
                let pos = node.start_position();
                let line = u32::try_from(pos.row.saturating_add(1)).unwrap_or(u32::MAX);
                let column = u32::try_from(pos.column.saturating_add(1)).unwrap_or(u32::MAX);
                let text = node.utf8_text(source_bytes).unwrap_or("").to_owned();
                let capture_name = capture_names
                    .get(capture.index as usize)
                    .copied()
                    .unwrap_or("")
                    .to_owned();

                out.push(AstMatch {
                    path: path.to_string_lossy().into_owned(),
                    line,
                    column,
                    node_kind: node.kind().to_owned(),
                    capture_name,
                    text,
                });
            }
            if hit_cap {
                break;
            }
        }
    }

    let mut query_errors: Vec<QueryError> = compiled
        .iter()
        .filter_map(|(language, result)| {
            result.as_ref().err().map(|error| QueryError {
                language: format!("{language:?}"),
                error: error.clone(),
            })
        })
        .collect();
    query_errors.sort_by(|a, b| a.language.cmp(&b.language));

    if !compiled.is_empty() && query_errors.len() == compiled.len() {
        let details: Vec<String> = query_errors
            .into_iter()
            .map(|qe| format!("{}: {}", qe.language, qe.error))
            .collect();
        return Err(ToolError::pre_validation(
            ToolErrorKind::InvalidArguments,
            format!(
                "ast_query failed to compile for every candidate language -- {}",
                details.join("; ")
            ),
        ));
    }

    Ok(ToolOutput::success(json!({
        "matches": out,
        "truncated": truncated,
        "skipped": skipped,
        "query_errors": query_errors,
    })))
}
