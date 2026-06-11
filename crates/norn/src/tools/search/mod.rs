//! Unified Search tool.
//!
//! Supports four operating modes selected by the `mode` argument:
//!
//! * **Content search** (`mode: "content"`, default when `pattern` is set) --
//!   walks the target tree and returns regex-matching lines as
//!   `file:line:content` tuples.
//! * **File finding** (`mode: "files"`, default when only `glob` is set) --
//!   expands the glob against the filesystem and returns matching paths.
//! * **Fuzzy file matching** (`mode: "fuzzy"`) -- scores file paths against
//!   `pattern` using the `nucleo-matcher` algorithm and returns them ranked
//!   best-first as `{path, score}` pairs.
//! * **AST structural search** (`mode: "ast"`) -- parses each candidate file
//!   with tree-sitter and evaluates an S-expression query (`ast_query`)
//!   against it, returning matched node locations and captured text.
//!
//! All four modes are read-only: `effect()` is [`ToolEffect::ReadOnly`] so the
//! scheduler may dispatch multiple Search calls concurrently with other
//! read-only tools.

mod ast_search;
mod fuzzy;
mod helpers;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::ToolErrorKind;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

use self::ast_search::run_ast_search;
use self::fuzzy::run_fuzzy_search;
use self::helpers::{GlobFilter, compile_glob, expand_braces, walk_for_content};

/// Default cap on matches returned by a single content-search invocation.
const DEFAULT_MAX_RESULTS: u32 = 50;

/// Model-supplied arguments for [`SearchTool`].
#[derive(Debug, Default, Deserialize, Serialize)]
struct SearchArgs {
    /// Regex pattern (content mode) or fuzzy needle (fuzzy mode).
    #[serde(default)]
    pattern: Option<String>,
    /// Optional directory root to search within. Defaults to the process CWD.
    #[serde(default)]
    path: Option<String>,
    /// Optional glob filter. With `pattern` it scopes the content walk; on
    /// its own it switches the tool into file-finding mode.
    #[serde(default)]
    glob: Option<String>,
    /// Cap on returned matches/paths. Defaults to `DEFAULT_MAX_RESULTS`.
    #[serde(default)]
    max_results: Option<u32>,
    /// Explicit operating mode. One of `content`, `files`, `fuzzy`, `ast`.
    /// When absent the tool falls back to legacy detection from
    /// `pattern`/`glob`.
    #[serde(default)]
    mode: Option<String>,
    /// Tree-sitter S-expression query, required when `mode == "ast"`.
    #[serde(default)]
    ast_query: Option<String>,
}

/// A single content-search hit.
#[derive(Debug, Serialize)]
pub(super) struct SearchMatch {
    path: String,
    line: u32,
    content: String,
}

/// Unified Search tool: regex content search, glob file finding, fuzzy
/// matching, and tree-sitter structural search.
#[derive(Debug, Default)]
pub struct SearchTool;

impl SearchTool {
    /// Creates a new `SearchTool`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "search"
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/search.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Search
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/search.usage.md"))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern for content search, or fuzzy needle when mode=fuzzy."
                },
                "path": {
                    "type": "string",
                    "description": "Directory root to search within. Defaults to the process CWD."
                },
                "glob": {
                    "type": "string",
                    "description": "Glob filter (e.g. `**/*.rs`). Restricts which files are considered in content, fuzzy, and ast modes; lists matching files in files mode."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum matches/paths to return. Defaults to 50."
                },
                "mode": {
                    "type": "string",
                    "enum": ["content", "files", "fuzzy", "ast"],
                    "description": "Operating mode. Omit for legacy detection (content if `pattern` given, files if only `glob` given)."
                },
                "ast_query": {
                    "type": "string",
                    "description": "Tree-sitter S-expression query. Required when mode=ast."
                }
            },
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
        let args: SearchArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::pre_validation(
                    ToolErrorKind::InvalidArguments,
                    format!("invalid search arguments: {e}"),
                )
            })?;

        let max_results = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS);
        let normalized_mode = args.mode.as_deref().map(str::to_ascii_lowercase);

        match normalized_mode.as_deref() {
            Some("content") => {
                let pattern = args.pattern.as_deref().ok_or_else(|| {
                    ToolError::pre_validation(
                        ToolErrorKind::InvalidArguments,
                        "mode=content requires `pattern`",
                    )
                })?;
                let root = resolve_root(ctx, args.path.as_deref());
                run_content_search(pattern, &root, args.glob.as_deref(), max_results)
            }
            Some("files") => {
                let glob_pattern = args.glob.as_deref().ok_or_else(|| {
                    ToolError::pre_validation(
                        ToolErrorKind::InvalidArguments,
                        "mode=files requires `glob`",
                    )
                })?;
                let root = resolve_root(ctx, args.path.as_deref());
                run_file_find(&root, glob_pattern, max_results)
            }
            Some("fuzzy") => {
                let needle = args.pattern.as_deref().unwrap_or_default();
                if needle.is_empty() {
                    return Err(ToolError::pre_validation(
                        ToolErrorKind::InvalidArguments,
                        "mode=fuzzy requires a non-empty `pattern`",
                    ));
                }
                let root = resolve_root(ctx, args.path.as_deref());
                run_fuzzy_search(needle, &root, args.glob.as_deref(), max_results)
            }
            Some("ast") => {
                let query_src = args.ast_query.as_deref().unwrap_or_default();
                if query_src.is_empty() {
                    return Err(ToolError::pre_validation(
                        ToolErrorKind::InvalidArguments,
                        "mode=ast requires a non-empty `ast_query`",
                    ));
                }
                let root = resolve_root(ctx, args.path.as_deref());
                run_ast_search(query_src, &root, args.glob.as_deref(), max_results)
            }
            Some(other) => Err(ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                format!("unknown search mode `{other}` -- valid: content, files, fuzzy, ast"),
            )),
            None => match (args.pattern.as_deref(), args.glob.as_deref()) {
                (Some(pattern), glob) => {
                    let root = resolve_root(ctx, args.path.as_deref());
                    run_content_search(pattern, &root, glob, max_results)
                }
                (None, Some(glob)) => {
                    let root = resolve_root(ctx, args.path.as_deref());
                    run_file_find(&root, glob, max_results)
                }
                (None, None) => Err(ToolError::pre_validation(
                    ToolErrorKind::InvalidArguments,
                    "at least one of `pattern` or `glob` must be supplied (or set `mode` \
                     explicitly)",
                )),
            },
        }
    }
}

fn resolve_root(ctx: &ToolContext, path: Option<&str>) -> PathBuf {
    match path {
        Some(p) => PathBuf::from(p),
        None => ctx.working_dir(),
    }
}

fn run_content_search(
    pattern: &str,
    root: &std::path::Path,
    glob_filter: Option<&str>,
    max_results: u32,
) -> Result<ToolOutput, ToolError> {
    let regex = Regex::new(pattern).map_err(|e| {
        ToolError::pre_validation(
            ToolErrorKind::InvalidArguments,
            format!("invalid regex `{pattern}`: {e}"),
        )
    })?;

    let compiled_filter: Option<GlobFilter> = compile_glob(glob_filter)?;

    let mut matches: Vec<SearchMatch> = Vec::new();
    let mut truncated = false;
    walk_for_content(
        root,
        &regex,
        compiled_filter.as_ref(),
        max_results,
        &mut matches,
        &mut truncated,
    );

    Ok(ToolOutput::success(json!({
        "matches": matches,
        "truncated": truncated,
    })))
}

fn run_file_find(
    base: &Path,
    glob_pattern: &str,
    max_results: u32,
) -> Result<ToolOutput, ToolError> {
    let expanded = expand_braces(glob_pattern);

    let mut paths: Vec<String> = Vec::new();
    let mut truncated = false;
    let cap = max_results as usize;

    let base_str = base.to_string_lossy();
    for pat in &expanded {
        if truncated {
            break;
        }
        let resolved = format!("{}/{}", base_str.trim_end_matches('/'), pat);

        let iter = glob::glob(&resolved).map_err(|e| {
            ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                format!("invalid glob `{resolved}`: {e}"),
            )
        })?;

        for entry in iter {
            let Ok(p) = entry else { continue };
            if paths.len() >= cap {
                truncated = true;
                break;
            }
            paths.push(p.to_string_lossy().into_owned());
        }
    }

    Ok(ToolOutput::success(json!({
        "paths": paths,
        "truncated": truncated,
    })))
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
    use super::*;
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};
    use serde_json::json;
    use tempfile::tempdir;

    fn envelope(args: Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_owned(),
            tool_name: "search".to_owned(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: Value::Null,
        }
    }

    #[test]
    fn object_safe() {
        let _: Box<dyn Tool + Send + Sync> = Box::new(SearchTool::new());
    }

    #[test]
    fn effect_is_read_only() {
        assert_eq!(SearchTool::new().effect(), ToolEffect::ReadOnly);
    }

    #[tokio::test]
    async fn content_search_finds_pattern_in_tempdir() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta needle\ngamma\n").expect("write a");
        std::fs::write(dir.path().join("b.txt"), "no match here\n").expect("write b");
        std::fs::write(dir.path().join("c.txt"), "needle on line 1\nother\n").expect("write c");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        assert!(!out.is_error());
        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 2);

        let mut got: Vec<(String, u64)> = matches
            .iter()
            .map(|m| {
                (
                    m["path"].as_str().unwrap_or_default().to_owned(),
                    m["line"].as_u64().unwrap_or_default(),
                )
            })
            .collect();
        got.sort();
        assert!(got[0].0.ends_with("a.txt"));
        assert_eq!(got[0].1, 2);
        assert!(got[1].0.ends_with("c.txt"));
        assert_eq!(got[1].1, 1);
    }

    #[tokio::test]
    async fn content_search_respects_max_results_and_marks_truncated() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("x.txt"), "needle\nneedle\nneedle\n").expect("write x");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
            "max_results": 2_u32,
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        assert_eq!(out.content["matches"].as_array().unwrap().len(), 2);
        assert_eq!(out.content["truncated"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn glob_filter_restricts_content_search() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "needle here\n").expect("write a.rs");
        std::fs::write(dir.path().join("b.txt"), "needle here\n").expect("write b.txt");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
            "glob": "**/*.rs",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 1);
        assert!(matches[0]["path"].as_str().unwrap().ends_with("a.rs"));
    }

    #[tokio::test]
    async fn file_finding_returns_rs_paths() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir sub");
        std::fs::write(dir.path().join("top.rs"), "").expect("write top");
        std::fs::write(dir.path().join("sub").join("nested.rs"), "").expect("write nested");
        std::fs::write(dir.path().join("other.txt"), "").expect("write other");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "path": dir.path().to_string_lossy(),
            "glob": "**/*.rs",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let paths = out.content["paths"].as_array().expect("paths array");
        assert_eq!(paths.len(), 2);
        let mut got: Vec<String> = paths
            .iter()
            .map(|p| p.as_str().unwrap_or_default().to_owned())
            .collect();
        got.sort();
        assert!(got[0].ends_with("sub/nested.rs"));
        assert!(got[1].ends_with("top.rs"));
    }

    #[tokio::test]
    async fn missing_pattern_and_glob_is_pre_validation_failure() {
        let tool = SearchTool::new();
        let env = envelope(json!({}));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn invalid_regex_reports_pre_validation_failure() {
        let dir = tempdir().expect("tempdir");
        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "[unterminated",
            "path": dir.path().to_string_lossy(),
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("invalid regex must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn fuzzy_search_ranks_filename_match_first() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src/auth")).expect("mkdir auth");
        std::fs::create_dir_all(dir.path().join("src/db")).expect("mkdir db");
        std::fs::write(dir.path().join("src/auth/login.rs"), "").expect("write login");
        std::fs::write(dir.path().join("src/auth/logout.rs"), "").expect("write logout");
        std::fs::write(dir.path().join("src/db/pool.rs"), "").expect("write pool");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "fuzzy",
            "pattern": "login",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("fuzzy ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert!(!matches.is_empty(), "expected at least one fuzzy match");

        let first = &matches[0];
        let first_path = first["path"].as_str().unwrap_or_default();
        assert!(
            first_path.ends_with("login.rs"),
            "expected login.rs ranked first, got `{first_path}`"
        );
        assert!(
            first["score"].as_u64().unwrap_or(0) > 0,
            "expected positive score for ranked-first match"
        );

        // Every other returned path should score no higher than login.rs.
        let first_score = first["score"].as_u64().unwrap_or(0);
        for m in matches.iter().skip(1) {
            assert!(m["score"].as_u64().unwrap_or(0) <= first_score);
        }
    }

    #[tokio::test]
    async fn fuzzy_empty_pattern_is_pre_validation_failure() {
        let dir = tempdir().expect("tempdir");
        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "fuzzy",
            "path": dir.path().to_string_lossy(),
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("empty fuzzy pattern must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn ast_search_captures_function_name() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\nfn beta() {}\n")
            .expect("write rust file");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "ast",
            "ast_query": "(function_item name: (identifier) @name)",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("ast ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        let mut names: Vec<String> = matches
            .iter()
            .map(|m| m["text"].as_str().unwrap_or_default().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["alpha".to_owned(), "beta".to_owned()]);

        let first = &matches[0];
        assert_eq!(first["node_kind"].as_str(), Some("identifier"));
        assert_eq!(first["capture_name"].as_str(), Some("name"));
        assert!(first["line"].as_u64().unwrap_or(0) >= 1);
        assert!(first["column"].as_u64().unwrap_or(0) >= 1);
        assert!(first["path"].as_str().unwrap_or_default().ends_with("a.rs"));
    }

    #[tokio::test]
    async fn ast_empty_query_is_pre_validation_failure() {
        let dir = tempdir().expect("tempdir");
        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "ast",
            "path": dir.path().to_string_lossy(),
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("empty ast_query must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn ast_invalid_query_for_some_languages_does_not_error() {
        // Use a query that's valid for Python but invalid for Rust. The
        // tool should absorb the per-language compile failure and still
        // return Python matches.
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").expect("write rs");
        std::fs::write(dir.path().join("b.py"), "def beta():\n    pass\n").expect("write py");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "ast",
            "ast_query": "(function_definition name: (identifier) @name)",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("ast ok despite mixed language compile results");

        let matches = out.content["matches"].as_array().expect("matches array");
        let names: Vec<&str> = matches
            .iter()
            .map(|m| m["text"].as_str().unwrap_or_default())
            .collect();
        assert!(names.contains(&"beta"), "expected Python beta in {names:?}");
    }

    #[tokio::test]
    async fn unknown_mode_returns_clear_error() {
        let dir = tempdir().expect("tempdir");
        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "nonsense",
            "path": dir.path().to_string_lossy(),
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("unknown mode must fail");
        match err {
            ToolError::PreValidationFailed { payload } => {
                assert_eq!(
                    payload.kind,
                    crate::tool::failure::ToolErrorKind::InvalidArguments
                );
                let message = &payload.message;
                assert!(
                    message.contains("content")
                        && message.contains("files")
                        && message.contains("fuzzy")
                        && message.contains("ast"),
                    "error message should list valid modes, got `{message}`"
                );
            }
            other => panic!("expected PreValidationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn all_four_modes_produce_results_on_same_tree() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
        std::fs::write(
            dir.path().join("src").join("login.rs"),
            "fn login() {}\nfn signin() {}\n",
        )
        .expect("write login.rs");
        std::fs::write(dir.path().join("src").join("notes.txt"), "login here\n")
            .expect("write notes.txt");

        let tool = SearchTool::new();
        let dir_str = dir.path().to_string_lossy().into_owned();

        // mode = content
        let env = envelope(json!({
            "mode": "content",
            "pattern": "login",
            "path": dir_str.clone(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("content ok");
        let content_matches = out.content["matches"].as_array().expect("content array");
        assert!(!content_matches.is_empty(), "content mode produced no hits");

        // mode = files
        let env = envelope(json!({
            "mode": "files",
            "glob": "**/*.rs",
            "path": dir_str.clone(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("files ok");
        let paths = out.content["paths"].as_array().expect("paths array");
        assert!(!paths.is_empty(), "files mode produced no paths");

        // mode = fuzzy
        let env = envelope(json!({
            "mode": "fuzzy",
            "pattern": "login",
            "path": dir_str.clone(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("fuzzy ok");
        let fuzzy_matches = out.content["matches"].as_array().expect("fuzzy array");
        assert!(!fuzzy_matches.is_empty(), "fuzzy mode produced no matches");

        // mode = ast
        let env = envelope(json!({
            "mode": "ast",
            "ast_query": "(function_item name: (identifier) @name)",
            "path": dir_str,
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("ast ok");
        let ast_matches = out.content["matches"].as_array().expect("ast array");
        assert!(!ast_matches.is_empty(), "ast mode produced no matches");
    }

    #[tokio::test]
    async fn brace_expansion_in_glob_produces_results() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "needle\n").expect("write a.rs");
        std::fs::write(dir.path().join("b.md"), "needle\n").expect("write b.md");
        std::fs::write(dir.path().join("c.txt"), "needle\n").expect("write c.txt");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
            "glob": "**/*.{rs,md}",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 2, "should match .rs and .md but not .txt");

        let mut paths: Vec<String> = matches
            .iter()
            .map(|m| m["path"].as_str().unwrap_or_default().to_owned())
            .collect();
        paths.sort();
        assert!(paths[0].ends_with("a.rs"));
        assert!(paths[1].ends_with("b.md"));
    }

    #[tokio::test]
    async fn file_path_as_root_searches_single_file() {
        let dir = tempdir().expect("tempdir");
        let file_path = dir.path().join("target.rs");
        std::fs::write(&file_path, "fn main() {}\nfn helper() {}\n").expect("write file");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "fn",
            "path": file_path.to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 2, "should find both fn lines in the file");
    }

    #[tokio::test]
    async fn plain_filename_glob_matches_files_in_subdirs() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir sub");
        std::fs::write(dir.path().join("README.md"), "needle\n").expect("write top");
        std::fs::write(dir.path().join("sub").join("README.md"), "needle\n").expect("write nested");
        std::fs::write(dir.path().join("other.txt"), "needle\n").expect("write other");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
            "glob": "README.md",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(
            matches.len(),
            2,
            "should match README.md in root and sub, but not other.txt"
        );
    }

    #[tokio::test]
    async fn brace_expansion_in_file_find_mode() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "").expect("write a.rs");
        std::fs::write(dir.path().join("b.md"), "").expect("write b.md");
        std::fs::write(dir.path().join("c.txt"), "").expect("write c.txt");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "files",
            "path": dir.path().to_string_lossy(),
            "glob": "**/*.{rs,md}",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let paths = out.content["paths"].as_array().expect("paths array");
        assert_eq!(paths.len(), 2, "should find .rs and .md but not .txt");
    }
}
