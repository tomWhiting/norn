//! AST structural search via tree-sitter S-expression queries.
//!
//! Parses each candidate file with tree-sitter and evaluates an
//! S-expression query against it, returning matched node locations and
//! captured text.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use serde::Serialize;
use serde_json::json;
use tree_sitter::{Query as TsQuery, QueryCursor, StreamingIterator};

use crate::error::ToolError;
use crate::tool::traits::ToolOutput;
use crate::tools::ast::SyntaxLanguage;

use super::helpers::{GlobFilter, compile_glob, elapsed, walk_collect_paths};

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

/// Run a tree-sitter structural search under `root`.
///
/// Lazily compiles the `query_source` S-expression per language. Compile
/// failures are absorbed into the cache so other languages can still
/// produce matches -- the brief's boundary "AST search across languages
/// in a single query" explicitly allows this graceful degradation.
pub(super) fn run_ast_search(
    query_source: &str,
    root: &Path,
    glob_filter: Option<&str>,
    max_results: u32,
    started: Instant,
) -> Result<ToolOutput, ToolError> {
    let compiled_filter: Option<GlobFilter> = compile_glob(glob_filter)?;

    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    walk_collect_paths(root, compiled_filter.as_ref(), &mut paths);

    let mut compiled: HashMap<SyntaxLanguage, Result<TsQuery, String>> = HashMap::new();
    let mut out: Vec<AstMatch> = Vec::new();
    let mut truncated = false;
    let cap = max_results as usize;

    for path in paths {
        if truncated {
            break;
        }
        let Some(language) = SyntaxLanguage::from_path(&path) else {
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

        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };

        let Some(tree) = crate::tools::ast::parse(&source, language) else {
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

    Ok(ToolOutput {
        content: json!({
            "matches": out,
            "truncated": truncated,
        }),
        is_error: false,
        duration: elapsed(started),
    })
}
